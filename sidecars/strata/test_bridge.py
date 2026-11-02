import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("bridge", Path(__file__).with_name("bridge.py"))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)


class BridgeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        (self.root / "serve").mkdir()
        (self.root / "serve/server.py").touch()
        for name in ("strata", "native.gguf", "ple.gguf"):
            (self.root / name).touch()
        (self.root / "pack/tokenizer").mkdir(parents=True)
        self.profile = self.root / "strata-test.json"
        self.cfg = dict(exe="strata", cwd=".", tokenizer="pack/tokenizer", backend="metal",
                        host="0.0.0.0", lazy_load=True, idle_unload_s=5,
                        args=["--pack", "pack", "--native", "native.gguf", "--ple-gguf", "ple.gguf",
                              "--max-context", "65536", "--mtp", "missing-draft", "--spec", "3"],
                        env={"STRATA_METAL_STREAM": "0", "STRATA_METAL_STREAM_FAKE": "1", "STRATA_METAL_ARENA_GB": "90"},
                        mcp_servers={"x": {}}, vision={"exe": "missing-vision"})
        self.saved_env = dict(os.environ)
        self.addCleanup(lambda: (os.environ.clear(), os.environ.update(self.saved_env)))

    def prepare(self, arena=None):
        self.profile.write_text(json.dumps(self.cfg))
        return bridge.prepare(self.profile, 8192, arena, 3, 0)[1]

    def test_real_paging_context_and_paths_override_original_config_without_mutating_it(self):
        cfg = self.prepare(6)
        self.assertEqual(cfg["host"], "127.0.0.1")
        self.assertEqual(cfg["args"].count("--max-context"), 1)
        self.assertIn("8192", cfg["args"])
        self.assertNotIn("--mtp", cfg["args"])
        self.assertIn(str(self.root / "native.gguf"), cfg["args"])
        self.assertIn("--mmap-experts", cfg["args"])
        self.assertFalse(cfg["lazy_load"])
        self.assertEqual(cfg["idle_unload_s"], 0)
        self.assertEqual(cfg["api_key"], "none")
        self.assertNotIn("mcp_servers", cfg)
        self.assertNotIn("vision", cfg)
        self.assertEqual(cfg["env"]["STRATA_METAL_STREAM"], "1")
        self.assertEqual(cfg["env"]["STRATA_METAL_STREAM_FAKE"], "0")
        self.assertEqual(cfg["env"]["STRATA_METAL_ARENA_GB"], "6")
        self.assertEqual(cfg["env"]["STRATA_METAL_PREFETCH"], "0")
        self.assertEqual(json.loads(self.profile.read_text()), self.cfg)

    def test_auto_budget_removes_stale_environment_override(self):
        os.environ["STRATA_METAL_ARENA_GB"] = "90"
        cfg = self.prepare()
        self.assertNotIn("STRATA_METAL_ARENA_GB", cfg["env"])
        self.assertNotIn("STRATA_METAL_ARENA_GB", os.environ)

    def test_missing_companion_is_actionable_before_start(self):
        (self.root / "ple.gguf").unlink()
        with self.assertRaises(FileNotFoundError):
            self.prepare()


if __name__ == "__main__":
    unittest.main()
