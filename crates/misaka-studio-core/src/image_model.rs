//! A diffusion model on disk, and what it costs to run it.
//!
//! An image model's memory bill is shaped differently from a chat model's ([`crate::model`]):
//! there is no growing KV cache, but a diffusion transformer's activations scale with the
//! *image* — width, height and batch size — rather than with a token count. The itemisation
//! mirrors [`crate::model::ModelRequirements`] on purpose (weights + working set + overhead) so
//! the two read the same way on a model card, even though the working-set term is computed from
//! pixels here and from tokens there.

use crate::hardware::HardwareSnapshot;
use crate::model::{FitVerdict, ModelSource};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One diffusers-format checkpoint the Studio knows about — a directory carrying a
/// `model_index.json` plus one or more `.safetensors` components (transformer/unet, VAE, text
/// encoders), the standard Hugging Face `diffusers` layout.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalImageModel {
    /// Stable identifier: the directory's own name.
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    /// Sum of every `.safetensors` file's size under the directory.
    pub size_bytes: u64,
    /// The pipeline class `model_index.json` names, e.g. `QwenImagePipeline`, `FluxPipeline`.
    pub architecture: Option<String>,
    /// Whether the checkpoint's `model_index.json` declares an image-conditioned component
    /// (`image_encoder`, or a pipeline class containing `Img2Img`/`Edit`/`Inpaint`) — the
    /// signal that reference images are worth offering in the generate panel at all.
    pub supports_reference_images: bool,
    pub source: ModelSource,
    pub sha256: Option<String>,
    pub modified_at: Option<u64>,
}

impl LocalImageModel {
    pub fn requirements(&self, width: u32, height: u32, batch: u32) -> ImageModelRequirements {
        ImageModelRequirements::estimate(self.size_bytes, width, height, batch)
    }

    pub fn fit(&self, hardware: &HardwareSnapshot, width: u32, height: u32, batch: u32) -> FitVerdict {
        FitVerdict::for_bytes(self.requirements(width, height, batch).total_bytes, hardware)
    }
}

/// The memory bill for one generation, itemised like [`crate::model::ModelRequirements`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageModelRequirements {
    pub weights_bytes: u64,
    /// Latents and activations for the requested image size and batch — the diffusion
    /// equivalent of the KV cache: the term a bare file size does not contain.
    pub activation_bytes: u64,
    pub overhead_bytes: u64,
    pub total_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub batch: u32,
}

impl ImageModelRequirements {
    /// Latent channels most current diffusion transformers use (SD3/FLUX/Qwen-Image all run a
    /// 16-channel latent space at 8x spatial downsampling). Wrong for an architecture that
    /// differs, but this term is dwarfed by the weights for any model big enough to matter, so
    /// a fixed constant beats requiring per-model metadata the directory scan cannot read.
    const LATENT_CHANNELS: u64 = 16;
    const LATENT_DOWNSAMPLE: u64 = 8;
    /// f16 activations: 2 bytes/element.
    const BYTES_PER_ELEMENT: u64 = 2;
    /// UNet/DiT attention and cross-attention buffers, several multiples of the raw latent
    /// tensor. Deliberately generous — the same "a little pessimistic beats a swap-to-death"
    /// choice `ModelRequirements::overhead` makes.
    const ACTIVATION_MULTIPLIER: u64 = 8;

    fn overhead(weights: u64) -> u64 {
        ((weights as f64 * 0.06) as u64).max(256 << 20)
    }

    pub fn estimate(weights_bytes: u64, width: u32, height: u32, batch: u32) -> Self {
        let latent_w = (width as u64).div_ceil(Self::LATENT_DOWNSAMPLE);
        let latent_h = (height as u64).div_ceil(Self::LATENT_DOWNSAMPLE);
        let batch = batch.max(1) as u64;
        let activation_bytes = latent_w
            .saturating_mul(latent_h)
            .saturating_mul(Self::LATENT_CHANNELS)
            .saturating_mul(Self::BYTES_PER_ELEMENT)
            .saturating_mul(Self::ACTIVATION_MULTIPLIER)
            .saturating_mul(batch);
        let overhead_bytes = Self::overhead(weights_bytes);
        ImageModelRequirements {
            weights_bytes,
            activation_bytes,
            overhead_bytes,
            total_bytes: weights_bytes.saturating_add(activation_bytes).saturating_add(overhead_bytes),
            width,
            height,
            batch: batch as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{Accelerator, AcceleratorKind};

    fn machine(ram_gb: u64, gpu: Option<(&str, u64)>) -> HardwareSnapshot {
        HardwareSnapshot {
            os: "test".into(),
            arch: "x86_64".into(),
            cpu_name: "test cpu".into(),
            physical_cores: Some(8),
            logical_cores: 16,
            total_memory: ram_gb << 30,
            available_memory: ram_gb << 30,
            accelerators: gpu
                .map(|(name, vram_gb)| Accelerator {
                    kind: AcceleratorKind::Cuda,
                    name: name.into(),
                    total_memory: Some(vram_gb << 30),
                    free_memory: Some(vram_gb << 30),
                    usable_memory: Some(vram_gb << 30),
                    driver: None,
                    index: 0,
                })
                .into_iter()
                .collect(),
        }
    }

    /// A bigger image costs more activation memory at the same weights — the term a bare
    /// checkpoint size does not contain, same argument as the KV cache for chat models.
    #[test]
    fn a_larger_image_costs_more_than_a_smaller_one() {
        let small = ImageModelRequirements::estimate(8 << 30, 512, 512, 1);
        let large = ImageModelRequirements::estimate(8 << 30, 2048, 2048, 1);
        assert!(large.total_bytes > small.total_bytes);
        assert_eq!(large.weights_bytes, small.weights_bytes);
    }

    /// Batch size multiplies the activation term, not the weights.
    #[test]
    fn batch_size_scales_activations_not_weights() {
        let one = ImageModelRequirements::estimate(8 << 30, 1024, 1024, 1);
        let four = ImageModelRequirements::estimate(8 << 30, 1024, 1024, 4);
        assert_eq!(four.weights_bytes, one.weights_bytes);
        assert_eq!(four.activation_bytes, one.activation_bytes * 4);
    }

    /// The fit verdict for an image model reuses the same judgment a chat model's does.
    #[test]
    fn fit_reuses_the_shared_verdict_logic() {
        let model = LocalImageModel {
            id: "flux-klein".into(),
            name: "FLUX.2 [klein]".into(),
            path: PathBuf::from("/models/flux-klein"),
            size_bytes: 8 << 30,
            architecture: Some("FluxPipeline".into()),
            supports_reference_images: false,
            source: ModelSource::default(),
            sha256: None,
            modified_at: None,
        };
        let verdict = model.fit(&machine(32, Some(("RTX 4090", 24))), 1024, 1024, 1);
        assert!(matches!(verdict, FitVerdict::Fits { .. } | FitVerdict::Tight { .. }), "got {verdict:?}");

        let tiny_machine = machine(4, None);
        let verdict = model.fit(&tiny_machine, 1024, 1024, 1);
        assert!(matches!(verdict, FitVerdict::DoesNotFit { .. }), "got {verdict:?}");
    }
}
