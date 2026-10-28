# Diffusion sidecar

An HTTP server around `diffusers`, supervised as a child process by
`crates/misaka-studio-runtime/src/backend/diffusion.rs` — the same shape as the
`llamacpp`/`mlx` backends supervising `llama-server`/`mlx_lm.server`. It never hardcodes
a model architecture: `diffusers.DiffusionPipeline.from_pretrained` picks the pipeline
class from the checkpoint's own `model_index.json`.

## Setup

```bash
pip install -r requirements.txt
```

`torch`'s wheel selection depends on your platform/accelerator (CUDA build on Linux,
the default build on macOS for MPS, etc.) — if the plain `pip install torch` here pulls
the wrong build for your GPU, install it separately first per pytorch.org's instructions,
then run this file's install without touching the `torch` line.

## Mock mode

```bash
python server.py --port 8600 --mock
```

Skips importing `torch`/`diffusers` entirely and answers every `/generate` with a
placeholder PNG. This is what MISAKA Studio's own plumbing (spawn, health check, load,
generate, and the UI panel) can be verified against without any downloaded weights or a
GPU. Point `image_backend.python_path`/`diffusion_server_path` at a wrapper that adds
`--mock`, or set `MISAKA_DIFFUSION_MOCK=1` in the environment the Studio spawns this
script in.

## Real generation

```bash
python server.py --port 8600
```

Then `POST /load` with `{"model_id": "...", "repo_id_or_path": "<local dir or HF repo id>"}`.

Model-specific notes:

- **Qwen-Image** and **FLUX.2 [klein]**: the exact Hugging Face repository IDs and the
  minimum `diffusers`/`transformers` versions that support these specific checkpoints
  were not verified when this sidecar was written — check the checkpoint's own model
  card on Hugging Face and upgrade `diffusers` if `from_pretrained` reports an unknown
  pipeline class.
- Reference/edit images are passed to the pipeline as its `image` argument. A pipeline
  that doesn't accept one raises an error rather than silently generating from the text
  prompt alone — an editing model that ignored its references would be a worse failure
  than a loud one.

## Endpoints

- `GET /health` — `{"status": "ok", "model_loaded": bool}`
- `POST /load` — `{"model_id", "repo_id_or_path", "dtype": "fp16"|"bf16"|"fp32", "device": "cuda"|"mps"|"cpu"}`
- `POST /generate` — `{"prompt", "negative_prompt"?, "width", "height", "steps", "guidance_scale", "seed"?, "reference_images"?: [base64], "n"}` → `{"images": [base64 PNG]}`
- `POST /unload`
