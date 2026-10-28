"""Diffusion sidecar — a small HTTP server around `diffusers`.

MISAKA Studio's Rust backends supervise an external OpenAI-compatible server as a
child process (see `crates/misaka-studio-runtime/src/backend/llamacpp.rs` and
`mlx.rs`). Image generation follows the same shape: this script is spawned by
`crates/misaka-studio-runtime/src/backend/diffusion.rs`, and the Rust side never
implements a model's forward pass itself — `diffusers.DiffusionPipeline.from_pretrained`
resolves whichever architecture the checkpoint declares in its `model_index.json`,
so a new model (Qwen-Image, FLUX.2, or anything else diffusers supports) needs no
code change here.

Run with `--mock` (or `MISAKA_DIFFUSION_MOCK=1`) to skip importing torch/diffusers
entirely and answer every request with a placeholder image. That path is what lets
the plumbing (spawn, health check, load, generate) be exercised without a GPU or any
downloaded weights.
"""

from __future__ import annotations

import argparse
import base64
import io
import os
import time
from typing import Optional

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel
from PIL import Image, ImageDraw

app = FastAPI()

_mock = False
_pipeline = None
_loaded_model_id: Optional[str] = None
_load_ms: int = 0


class LoadRequest(BaseModel):
    model_id: str
    repo_id_or_path: str
    dtype: str = "fp16"
    device: str = "cpu"


class GenerateRequest(BaseModel):
    prompt: str
    negative_prompt: Optional[str] = None
    width: int = 1024
    height: int = 1024
    steps: int = 20
    guidance_scale: float = 4.5
    seed: Optional[int] = None
    reference_images: list[str] = []
    n: int = 1


def _encode_png(image: Image.Image) -> str:
    buf = io.BytesIO()
    image.save(buf, format="PNG")
    return base64.b64encode(buf.getvalue()).decode("ascii")


def _mock_image(request: GenerateRequest) -> Image.Image:
    image = Image.new("RGB", (request.width, request.height), color=(40, 40, 60))
    draw = ImageDraw.Draw(image)
    label = f"MOCK\n{request.prompt[:40]}\n{request.width}x{request.height} steps={request.steps}"
    if request.reference_images:
        label += f"\nrefs={len(request.reference_images)}"
    draw.multiline_text((16, 16), label, fill=(230, 230, 240))
    return image


@app.get("/health")
def health():
    return {"status": "ok", "model_loaded": _pipeline is not None or (_mock and _loaded_model_id is not None)}


@app.post("/load")
def load(request: LoadRequest):
    global _pipeline, _loaded_model_id, _load_ms
    started = time.monotonic()
    if _mock:
        _loaded_model_id = request.model_id
        _load_ms = int((time.monotonic() - started) * 1000)
        return {"model_id": _loaded_model_id, "load_ms": _load_ms}

    import torch
    from diffusers import DiffusionPipeline

    dtype = {"fp16": torch.float16, "bf16": torch.bfloat16, "fp32": torch.float32}.get(request.dtype, torch.float16)
    try:
        pipeline = DiffusionPipeline.from_pretrained(request.repo_id_or_path, torch_dtype=dtype)
        pipeline = pipeline.to(request.device)
    except Exception as exc:  # the caller reports this as the engine's own failure
        raise HTTPException(status_code=500, detail=f"could not load {request.repo_id_or_path}: {exc}") from exc

    _pipeline = pipeline
    _loaded_model_id = request.model_id
    _load_ms = int((time.monotonic() - started) * 1000)
    return {"model_id": _loaded_model_id, "load_ms": _load_ms}


@app.post("/unload")
def unload():
    global _pipeline, _loaded_model_id
    _pipeline = None
    _loaded_model_id = None
    return {"status": "ok"}


@app.post("/generate")
def generate(request: GenerateRequest):
    if _mock:
        images = [_encode_png(_mock_image(request)) for _ in range(max(1, request.n))]
        return {"images": images}

    if _pipeline is None:
        raise HTTPException(status_code=409, detail="no model loaded")

    kwargs = {
        "prompt": request.prompt,
        "num_inference_steps": request.steps,
        "guidance_scale": request.guidance_scale,
        "width": request.width,
        "height": request.height,
        "num_images_per_prompt": max(1, request.n),
    }
    if request.negative_prompt:
        kwargs["negative_prompt"] = request.negative_prompt
    if request.seed is not None:
        import torch

        kwargs["generator"] = torch.Generator().manual_seed(request.seed)
    if request.reference_images:
        # Not every pipeline accepts reference/edit images under this name — pipelines
        # that don't recognize `image` fail loudly rather than silently ignoring it,
        # which is preferable to a generation that quietly dropped the user's references.
        ref_images = [Image.open(io.BytesIO(base64.b64decode(b))) for b in request.reference_images]
        kwargs["image"] = ref_images if len(ref_images) > 1 else ref_images[0]

    try:
        result = _pipeline(**kwargs)
    except Exception as exc:
        raise HTTPException(status_code=500, detail=f"generation failed: {exc}") from exc

    images = [_encode_png(img) for img in result.images]
    return {"images": images}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--mock", action="store_true", default=os.environ.get("MISAKA_DIFFUSION_MOCK") == "1")
    args = parser.parse_args()

    global _mock
    _mock = args.mock

    import uvicorn

    uvicorn.run(app, host=args.host, port=args.port, log_level="info")


if __name__ == "__main__":
    main()
