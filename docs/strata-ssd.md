# Strata SSD expert streaming

Studio can supervise the [Strata Metal engine](https://github.com/Niko1221/Strata) and its upstream OpenAI server. The actual expert pager and Metal kernels run in Strata, outside the Studio process. This is a separate backend from llama.cpp's mmap/CPU offload.

The integration currently supports **prepared Strata Metal MoE profiles on Apple Silicon, macOS 15+**. It does not turn arbitrary GGUFs into supported Strata models. In particular, the dense Huihui Qwen 27B model has no routed experts and continues to use llama.cpp.

## Install the native engine

From the Studio checkout, with Python 3.10+ and Apple's Command Line Tools:

```sh
python3 scripts/install-strata.py
```

The installer checks out upstream commit `c62d5e3a15b40ea51c96aae9e8d36928965f53ab` into the user's application data directory, creates a Python environment and builds the real Metal engine. Upstream pins its runtime packages and ggml source and verifies the metal-cpp archive's SHA-256. The source and MIT license remain in that separate installation. The installer downloads **no model weights** and prints a model preparation command when finished. `--directory PATH` and `--jobs N` are available; it refuses to overwrite a different existing checkout.

## Prepare and register a model

1. Check the SSD space needed by the chosen pack. For example, upstream's Q2_0 Flash-Next pack includes about 37.6 GB of main weights plus a 28.8 GB n-gram table, with additional preparation space. Optional contiguous expert copies need more space. Use the upstream [Mac setup documentation](https://github.com/Niko1221/Strata/blob/main/docs/MAC.md) for the chosen pack.
2. Run the preparation command printed by the installer. It uses upstream setup with `--context 8192 --vision no --no-start`. Keep the generated model, pack/tokenizer and companion GGUFs together with their `strata-<model>.json` configuration.
3. In **Settings → Backend → Strata · SSD expert streaming**, enter that JSON profile's absolute path and save. The Python interpreter is detected from the Strata installation's `.venv/bin/python`, or can be specified explicitly.
4. The profile's native GGUF appears in the model list, even when it is outside the ordinary models directory. Choose it in Chat or Code. Auto mode routes this exact file to Strata. Other GGUFs retain their normal backend. The explicit Strata engine preference also falls back to the normal engine for unrelated GGUFs.

Readiness checks report missing executables, companion files, malformed profiles, dense models or missing Python dependencies before startup. Choosing a matching profile model whose companion has disappeared reports a Strata error; it does not silently try llama.cpp.

## What the settings control

- **Expert cache:** decimal GB, matching upstream `STRATA_METAL_ARENA_GB`. Empty means upstream's adaptive budget. This is not a total process-memory limit. Explicit values leave at least 15 GB outside the arena in Studio's preflight calculation, for macOS, dense tensors, KV and working buffers; actual requirements still depend on the model and context.
- **SSD readers:** 1–16 I/O workers, default 4.
- **Experts to prefetch:** 0–32 predicted experts, default 4. Zero disables predictive read-ahead; demanded experts still load.

The bridge forces real SSD expert streaming (`STRATA_METAL_STREAM=1`, fake mode disabled), passes mmap-experts, uses a paged n-gram table, disables MTP drafting and uses 1,024-token prompt chunks. Strata retains its native cache eviction, demand reads and predictive prefetch. It can use a prepared contiguous expert file when present; Studio does not generate an extra many-GB copy automatically.

The model list reports **SSD streaming**, rather than pretending all weights fit in RAM. Its full-resident weight/KV estimates remain visible. At least 16 GiB is required to attempt a load; this is not a guarantee that every pack/context fits. Upstream's published smaller-Mac streaming measurements are on 32 GB hardware; a 24 GB machine needs its own model-level validation. SSD latency and throughput affect performance.

Studio launches a foreground upstream HTTP server on a dynamically selected **127.0.0.1** port, then supervises it using the shared health/SSE/cancellation/error handling. The bridge uses a private temporary configuration, leaves the original profile unchanged, overrides inherited server authentication for its private loopback connection, disables upstream MCP/image sidecars and lazy/idle unloading, and uses upstream's child lifeline to stop the native engine with its server. The server's reported context wins over the requested context. Studio's web/Tor settings continue to govern Studio web tools; loading a prepared Strata pack does not require browsing or downloading weights.

A pack has companion weights and a separate tokenizer. Until Studio commits the full asset set, Strata inference does **not** expose the native GGUF alone as the complete model provenance identity. This backend is not a deterministic PALW mining class.

## Verification

```sh
cargo test -p misaka-studio-core -p misaka-studio-runtime --lib --locked
python3 -m unittest discover -s sidecars/strata -p 'test_*.py'
cd ui && npm test && npm run build
```

Tests cover prepared profile registration, rejection of dense profiles, missing companions, routing isolation, cache limits, selection of a sparse 80-GiB fixture on a 24-GiB hardware fixture, bridge configuration, streaming and process cleanup through an HTTP child. These fixtures are API tests, not model inference benchmarks. Native upstream `expert_parity` and `arena_budget_test` can additionally validate the Metal expert kernels and adaptive budget without downloading a complete model.
