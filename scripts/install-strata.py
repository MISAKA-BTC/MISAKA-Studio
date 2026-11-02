#!/usr/bin/env python3
"""Install/build the pinned Strata Metal engine without downloading model weights.

Usage: python3 scripts/install-strata.py [--directory PATH] [--jobs 4]
Then run the printed upstream model-preparation command and register its profile in Studio.
The upstream MIT source and notices stay in this separate per-user engine installation.
"""
import argparse
from pathlib import Path
import platform
import subprocess
import sys
import shlex

REVISION = "c62d5e3a15b40ea51c96aae9e8d36928965f53ab"
SOURCE = "https://github.com/Niko1221/Strata.git"


def run(args, **kwargs):
    subprocess.run([str(a) for a in args], check=True, **kwargs)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--directory", type=Path, default=Path.home() / "Library/Application Support/MISAKA Studio/engines/strata" / REVISION[:12])
    ap.add_argument("--jobs", type=int, default=4)
    a = ap.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64" or int(platform.mac_ver()[0].split('.')[0]) < 15:
        ap.error("Strata Metal requires Apple Silicon and macOS 15+")
    if not 1 <= a.jobs <= 32:
        ap.error("--jobs must be 1–32")
    root = a.directory.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True)
    if not (root / ".git").exists():
        if any(root.iterdir()):
            ap.error("installation directory must be empty or an existing pinned Strata checkout")
        run(["git", "init", root])
        run(["git", "-C", root, "remote", "add", "origin", SOURCE])
        run(["git", "-C", root, "fetch", "--depth", "1", "origin", REVISION])
        run(["git", "-C", root, "checkout", "--detach", "FETCH_HEAD"])
    head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    if head != REVISION:
        ap.error(f"refusing to change existing checkout at {head}; choose an empty directory")
    if subprocess.run(["git", "-C", str(root), "diff", "--quiet", "HEAD", "--"]).returncode:
        ap.error("existing Strata source has local changes; choose a clean installation directory")
    python = root / ".venv/bin/python"
    if not python.is_file():
        run([sys.executable, "-m", "venv", root / ".venv"])
    # The pinned upstream owns dependency versions and build inputs (including metal-cpp SHA256).
    code = """import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import setup as S
from metal import setup_mac as M
S.pip_install(S.requirement_lines(), 'Strata runtime dependencies')
llama = S.get_llama_cpp()
print(M.build_engine(llama, jobs=int(sys.argv[2])))
"""
    run([python, "-c", code, root, a.jobs], cwd=root)
    print(f"\nEngine ready: {root / 'build-metal/strata'}")
    print("Model weights were not downloaded. Check SSD space before preparing a pack.")
    print("Prepare a supported model: " + shlex.join([str(python), str(root / "setup.py"), "--model", "Q2_0", "--context", "8192", "--vision", "no", "--no-start"]))
    print("In Studio Settings → Strata, select the generated strata-<model>.json.")


if __name__ == "__main__":
    main()
