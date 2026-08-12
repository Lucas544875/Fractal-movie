use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Artifact {
    pub id: String,
    pub kind: String,
    pub media_type: String,
    pub path: PathBuf,
    pub byte_size: u64,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

impl Artifact {
    pub(crate) fn from_file(
        kind: impl Into<String>,
        media_type: impl Into<String>,
        path: PathBuf,
    ) -> Result<Self> {
        let bytes = fs::read(&path)
            .with_context(|| format!("could not read artifact {}", path.display()))?;
        let digest = sha256_hex(&bytes);
        Ok(Self {
            id: format!("artifact-{}", &digest[..16]),
            kind: kind.into(),
            media_type: media_type.into(),
            path,
            byte_size: bytes.len() as u64,
            metadata: serde_json::Map::new(),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ImageMetrics {
    pub width: u32,
    pub height: u32,
    pub mean_luminance: f64,
    pub luminance_standard_deviation: f64,
    pub clipped_shadow_ratio: f64,
    pub clipped_highlight_ratio: f64,
    pub edge_density: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RenderEnvironment {
    pub workflow_version: String,
    pub git_commit: Option<String>,
    pub adapter_name: String,
    pub backend: String,
    pub device_type: String,
    pub hardware_accelerated: bool,
    pub gpu_duty_cycle_percent: Option<f64>,
}

impl RenderEnvironment {
    pub(crate) fn from_renderer(renderer: &fractal_renderer_core::Renderer) -> Self {
        let adapter = renderer.adapter_info();
        Self {
            workflow_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_commit: option_env!("FRACTAL_RENDERER_GIT_COMMIT").map(str::to_owned),
            adapter_name: adapter.name.clone(),
            backend: format!("{:?}", adapter.backend),
            device_type: format!("{:?}", adapter.device_type),
            hardware_accelerated: renderer.is_hardware_accelerated(),
            gpu_duty_cycle_percent: renderer.gpu_duty_cycle().map(|value| value * 100.0),
        }
    }
}

impl ImageMetrics {
    #[must_use]
    pub fn from_rgba(image: &RgbaImage) -> Self {
        let pixel_count = u64::from(image.width()) * u64::from(image.height());
        if pixel_count == 0 {
            return Self::default();
        }

        let mut luminance_sum = 0.0;
        let mut luminance_squared_sum = 0.0;
        let mut shadows = 0_u64;
        let mut highlights = 0_u64;
        let mut edge_sum = 0.0;
        let mut edge_samples = 0_u64;

        for y in 0..image.height() {
            for x in 0..image.width() {
                let pixel = image.get_pixel(x, y).0;
                let luminance = srgb_luminance(pixel);
                luminance_sum += luminance;
                luminance_squared_sum += luminance * luminance;
                if pixel[..3].iter().all(|&channel| channel <= 3) {
                    shadows += 1;
                }
                if pixel[..3].iter().any(|&channel| channel >= 252) {
                    highlights += 1;
                }
                if x > 0 {
                    edge_sum += (luminance - srgb_luminance(image.get_pixel(x - 1, y).0)).abs();
                    edge_samples += 1;
                }
                if y > 0 {
                    edge_sum += (luminance - srgb_luminance(image.get_pixel(x, y - 1).0)).abs();
                    edge_samples += 1;
                }
            }
        }

        let count = pixel_count as f64;
        let mean = luminance_sum / count;
        let variance = (luminance_squared_sum / count - mean * mean).max(0.0);
        Self {
            width: image.width(),
            height: image.height(),
            mean_luminance: mean,
            luminance_standard_deviation: variance.sqrt(),
            clipped_shadow_ratio: shadows as f64 / count,
            clipped_highlight_ratio: highlights as f64 / count,
            edge_density: if edge_samples == 0 {
                0.0
            } else {
                edge_sum / edge_samples as f64
            },
        }
    }
}

fn srgb_luminance(pixel: [u8; 4]) -> f64 {
    (0.2126 * f64::from(pixel[0]) + 0.7152 * f64::from(pixel[1]) + 0.0722 * f64::from(pixel[2]))
        / 255.0
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .context("atomic output path must end in a file name")?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    fs::write(&temporary, contents)
        .with_context(|| format!("could not write temporary file {}", temporary.display()))?;
    let mut moved = fs::rename(&temporary, path);
    // Unix replaces atomically. Windows requires an explicit replacement
    // fallback, which has a short non-atomic window but preserves portability.
    if moved.is_err() && path.is_file() {
        fs::remove_file(path).with_context(|| format!("could not replace {}", path.display()))?;
        moved = fs::rename(&temporary, path);
    }
    if let Err(error) = moved {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("could not publish {}", path.display()));
    }
    Ok(())
}

pub(crate) fn save_png_atomic(path: &Path, image: &RgbaImage) -> Result<()> {
    if image.width() == 0 || image.height() == 0 {
        bail!("cannot save an empty PNG image");
    }
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .context("PNG encoder failed")?;
    write_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_metrics_detect_clipped_pixels_and_edges() {
        let image = RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            }
        });
        let metrics = ImageMetrics::from_rgba(&image);
        assert_eq!(metrics.clipped_shadow_ratio, 0.5);
        assert_eq!(metrics.clipped_highlight_ratio, 0.5);
        assert!(metrics.edge_density > 0.9);
    }
}
