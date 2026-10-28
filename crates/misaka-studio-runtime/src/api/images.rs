//! Image generation: the diffusion-model counterpart to `management.rs`'s `/models`/`/runtime`.
//!
//! Kept as its own module, nested at `/images`, for the same reason [`crate::backend::ImageBackend`]
//! is its own trait rather than an extension of [`crate::backend::InferenceBackend`]: a diffusion
//! checkpoint is not a chat model, and forcing its endpoints into `management.rs`'s handlers would
//! mean threading an `Option<width>`/`Option<steps>` through code that has no other reason to know
//! about either.

use crate::backend::{Availability, ImageDtype, Txt2ImgRequest};
use crate::state::{AppState, ImageRuntimeStatus};
use crate::{Error, Result};
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use misaka_studio_core::HardwareSnapshot;
use misaka_studio_core::image_model::{ImageModelRequirements, LocalImageModel};
use misaka_studio_core::model::FitVerdict;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The resolution a model card's fit verdict is judged at. Not a generation default — a request
/// names its own width/height — but a stand-in so "will this fit" has one number to answer, the
/// same role [`LocalModel::recommended_context`](misaka_studio_core::model::LocalModel::recommended_context)
/// plays for a chat model's context size.
const CARD_WIDTH: u32 = 1024;
const CARD_HEIGHT: u32 = 1024;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/models", get(list_image_models))
        .route("/models/refresh", post(refresh_image_models))
        .route("/models/{id}/load", post(load_image_model))
        .route("/models/unload", post(unload_image_model))
        .route("/runtime", get(image_runtime_status))
        .route("/runtime/backends", get(image_backends))
        .route("/generate", post(generate_image))
}

/// An image model, with what it needs at a representative resolution and whether it fits.
#[derive(Serialize)]
pub struct ImageModelView {
    #[serde(flatten)]
    pub model: LocalImageModel,
    pub requirements: ImageModelRequirements,
    pub fit: FitVerdict,
    pub fit_summary: String,
}

fn view(model: &LocalImageModel, hardware: &HardwareSnapshot) -> ImageModelView {
    let requirements = model.requirements(CARD_WIDTH, CARD_HEIGHT, 1);
    let fit = FitVerdict::for_bytes(requirements.total_bytes, hardware);
    ImageModelView { requirements, fit_summary: fit.summary(), fit, model: model.clone() }
}

async fn list_image_models(State(state): State<Arc<AppState>>) -> Json<Vec<ImageModelView>> {
    let models = state.store.list_images().await;
    Json(models.iter().map(|m| view(m, &state.hardware)).collect())
}

async fn refresh_image_models(State(state): State<Arc<AppState>>) -> Result<Json<Vec<ImageModelView>>> {
    state.store.refresh().await?;
    let models = state.store.list_images().await;
    Ok(Json(models.iter().map(|m| view(m, &state.hardware)).collect()))
}

#[derive(Deserialize)]
struct LoadBody {
    #[serde(default)]
    dtype: Option<String>,
}

fn parse_dtype(raw: Option<String>) -> ImageDtype {
    match raw.as_deref() {
        Some("bf16") => ImageDtype::Bf16,
        Some("fp32") => ImageDtype::Fp32,
        _ => ImageDtype::Fp16,
    }
}

async fn load_image_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<LoadBody>>,
) -> Result<Json<ImageRuntimeStatus>> {
    let dtype = parse_dtype(body.and_then(|Json(b)| b.dtype));
    Ok(Json(state.load_image_model(&id, dtype).await?))
}

async fn unload_image_model(State(state): State<Arc<AppState>>) -> Result<Json<ImageRuntimeStatus>> {
    state.unload_image_model().await?;
    let backend = state.image_backend().await;
    let backend_available = backend.availability().await.is_available();
    Ok(Json(ImageRuntimeStatus { backend: backend.name().to_string(), backend_available, model_id: None, load_ms: None }))
}

async fn image_runtime_status(State(state): State<Arc<AppState>>) -> Json<ImageRuntimeStatus> {
    let backend = state.image_backend().await;
    let loaded = state.image_loaded().await;
    let backend_available = backend.availability().await.is_available();
    Json(ImageRuntimeStatus {
        backend: backend.name().to_string(),
        backend_available,
        model_id: loaded.as_ref().map(|l| l.loaded.model_id.clone()),
        load_ms: loaded.as_ref().map(|l| l.loaded.load_ms),
    })
}

#[derive(Serialize)]
struct ImageBackendInfo {
    name: String,
    selected: bool,
    availability: Availability,
}

/// Which image backends this build knows, with their availability. What the Settings screen
/// lists, mirroring `management.rs`'s `backends()` for the chat seam.
async fn image_backends(State(state): State<Arc<AppState>>) -> Json<Vec<ImageBackendInfo>> {
    Json(
        state
            .image_backend_status()
            .await
            .into_iter()
            .map(|(name, selected, availability)| ImageBackendInfo { name, selected, availability })
            .collect(),
    )
}

const fn default_dim() -> u32 {
    1024
}
const fn default_steps() -> u32 {
    20
}
const fn default_guidance() -> f32 {
    4.5
}
const fn default_n() -> u32 {
    1
}

#[derive(Deserialize)]
struct GenerateBody {
    prompt: String,
    #[serde(default)]
    negative_prompt: Option<String>,
    #[serde(default = "default_dim")]
    width: u32,
    #[serde(default = "default_dim")]
    height: u32,
    #[serde(default = "default_steps")]
    steps: u32,
    #[serde(default = "default_guidance")]
    guidance_scale: f32,
    #[serde(default)]
    seed: Option<u64>,
    /// Base64-encoded source images, for multi-reference generation or editing. Empty for a
    /// plain text-to-image request.
    #[serde(default)]
    reference_images: Vec<String>,
    #[serde(default = "default_n")]
    n: u32,
}

#[derive(Serialize)]
struct GenerateResponse {
    /// `data:image/png;base64,...` — directly usable as an `<img src>`, which is the only place
    /// the UI puts these.
    images: Vec<String>,
}

async fn generate_image(State(state): State<Arc<AppState>>, Json(body): Json<GenerateBody>) -> Result<Json<GenerateResponse>> {
    let reference_images = body
        .reference_images
        .iter()
        .map(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| Error::bad_request(format!("invalid reference image: {e}")))
        })
        .collect::<Result<Vec<_>>>()?;

    let request = Txt2ImgRequest {
        prompt: body.prompt,
        negative_prompt: body.negative_prompt,
        width: body.width,
        height: body.height,
        steps: body.steps,
        guidance_scale: body.guidance_scale,
        seed: body.seed,
        reference_images,
        n: body.n.max(1),
    };
    let images = state.generate_image(request).await?;
    let images = images
        .into_iter()
        .map(|img| format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(img.png_bytes)))
        .collect();
    Ok(Json(GenerateResponse { images }))
}
