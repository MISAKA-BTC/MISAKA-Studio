"""Run an installed Strata server under Studio's process supervisor.

No model downloads, detached launchers or shell commands. A private run config keeps
Studio's context and SSD cache settings separate from the user's upstream profile.
"""
import argparse
import json
import os
from pathlib import Path
import runpy
import sys
import tempfile


def prepare(profile, context, arena_gb, io_threads, prefetch):
    profile = Path(profile).resolve(strict=True)
    cfg = json.loads(profile.read_text(encoding="utf-8-sig"))
    root = Path(cfg.get("cwd", profile.parent))
    if not root.is_absolute():
        root = profile.parent / root
    root = root.resolve(strict=True)
    server = root / "serve" / "server.py"
    if not server.is_file():
        raise ValueError(f"Strata server not found: {server}")
    if cfg.get("backend") != "metal":
        raise ValueError("Studio's Strata SSD backend currently supports prepared Metal profiles only")
    args = cfg.get("args")
    if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
        raise ValueError("Strata profile args must be a list of strings")
    # Resolve upstream paths against its own cwd, never the Studio executable's directory.
    def path(value):
        p = Path(value)
        return str((p if p.is_absolute() else root / p).resolve(strict=True))
    cfg["exe"] = path(cfg["exe"])
    cfg["tokenizer"] = path(cfg["tokenizer"])
    cfg["cwd"] = str(root)
    path_flags = {"--pack", "--native", "--ple-gguf", "--native-dense-gguf", "--expert-profile"}
    out = []
    i = 0
    while i < len(args):
        flag = args[i]
        if flag in {"--max-context", "--mtp", "--spec", "--suffix-draft", "--ple-io"}:
            if i + 1 >= len(args):
                raise ValueError(f"Missing value for {flag}")
            i += 2
            continue
        if flag == "--vision":
            i += 1
            continue
        if flag in path_flags:
            if i + 1 >= len(args):
                raise ValueError(f"Missing path for {flag}")
            value = args[i + 1]
            # Learned expert ranking is optional, and can be created on first use.
            if flag == "--expert-profile":
                p = Path(value)
                value = str(p if p.is_absolute() else root / p)
            else:
                value = path(value)
            out.extend([flag, value])
            i += 2
        else:
            out.append(flag)
            i += 1
    if "--native" not in out or "--pack" not in out or "--ple-gguf" not in out:
        raise ValueError("Use a prepared Strata Metal profile with --native, --pack and --ple-gguf")
    out.extend(["--max-context", str(context), "--spec", "1", "--suffix-draft", "0", "--ple-io", "mmap"])
    if "--mmap-experts" not in out:
        out.append("--mmap-experts")
    cfg["args"] = out
    env = dict(cfg.get("env") or {})
    env.update(STRATA_METAL_STREAM="1", STRATA_METAL_STREAM_FAKE="0",
               STRATA_METAL_IO_THREADS=str(io_threads), STRATA_METAL_PREFETCH=str(prefetch),
               STRATA_METAL_PROMPT_CHUNK="1024", STRATA_PLE_AUTO_RAM="0")
    env.pop("STRATA_API_KEY", None)
    env.pop("STRATA_METAL_ARENA_GB", None)
    os.environ.pop("STRATA_METAL_ARENA_GB", None)
    if arena_gb is not None:
        env["STRATA_METAL_ARENA_GB"] = str(arena_gb)  # upstream uses decimal GB
    cfg["env"] = env
    os.environ.update(env)
    os.environ.pop("STRATA_API_KEY", None)
    cfg.update(host="127.0.0.1", open_browser=False, parallel=1, lazy_load=False, idle_unload_s=0, api_key="none")
    # The inference sidecar has no need to expose tools or start additional engines.
    for key in ("replicas", "split", "mcp_servers", "mcpServers", "api_key_env", "tls", "vision", "images"):
        cfg.pop(key, None)
    return server, cfg


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--profile", required=True)
    ap.add_argument("--context", type=int, default=8192)
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--arena-gb", type=int)
    ap.add_argument("--io-threads", type=int, default=4)
    ap.add_argument("--prefetch", type=int, default=4)
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    server, cfg = prepare(a.profile, a.context, a.arena_gb, a.io_threads, a.prefetch)
    if a.check:
        # Importing the official server checks its Python dependencies without loading weights.
        runpy.run_path(str(server), run_name="studio_strata_preflight")
        print("Strata Metal SSD streaming: prepared profile and Python dependencies ready")
        return
    with tempfile.TemporaryDirectory(prefix="misaka-strata-") as work:
        config = Path(work) / "run.json"
        cfg["log"] = str(Path(work) / "engine.log")
        config.write_text(json.dumps(cfg), encoding="utf-8")
        sys.argv = [str(server), "--engine", "strata", "--config", str(config),
                    "--host", "127.0.0.1", "--port", str(a.port), "--parallel", "1"]
        runpy.run_path(str(server), run_name="__main__")


if __name__ == "__main__":
    main()
