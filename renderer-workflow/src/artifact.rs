use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const TEMPORARY_FILE_MARKER: &str = ".fractal-tmp-";
static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(1);

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
    let temporary = temporary_file_path(path, false)?;
    let mut cleanup = TemporaryFileCleanup::new(temporary.clone());
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
        return Err(error).with_context(|| format!("could not publish {}", path.display()));
    }
    cleanup.disarm();
    Ok(())
}

pub(crate) fn temporary_file_path(path: &Path, preserve_extension: bool) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .context("temporary output path must end in a file name")?
        .to_string_lossy();
    let token = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
    if preserve_extension {
        let stem = path
            .file_stem()
            .context("temporary output path must have a file stem")?
            .to_string_lossy();
        let extension = path
            .extension()
            .context("temporary output path must have an extension")?
            .to_string_lossy();
        Ok(path.with_file_name(format!(
            ".{stem}{TEMPORARY_FILE_MARKER}{}-{token}.{extension}",
            std::process::id()
        )))
    } else {
        Ok(path.with_file_name(format!(
            ".{file_name}{TEMPORARY_FILE_MARKER}{}-{token}",
            std::process::id()
        )))
    }
}

pub(crate) fn cleanup_abandoned_temporary_files(root: &Path) -> Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in
        fs::read_dir(root).with_context(|| format!("could not inspect {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            removed += cleanup_abandoned_temporary_files(&path)?;
        } else if file_type.is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .contains(TEMPORARY_FILE_MARKER)
        {
            fs::remove_file(&path).with_context(|| {
                format!(
                    "could not remove abandoned temporary file {}",
                    path.display()
                )
            })?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub(crate) struct TemporaryFileCleanup {
    path: PathBuf,
    armed: bool,
}

impl TemporaryFileCleanup {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryFileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
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
    use std::{sync::Arc, thread};

    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fractal-artifact-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }

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

    #[test]
    fn atomic_writes_replace_on_the_same_filesystem_without_leaking_temporaries() {
        let directory = temporary_directory("atomic");
        let path = Arc::new(directory.join("manifest.json"));
        fs::create_dir_all(&directory).unwrap();
        write_atomic(&path, b"initial").unwrap();
        let mut writers = Vec::new();
        for value in [b"alpha".as_slice(), b"beta", b"gamma", b"delta"] {
            let path = Arc::clone(&path);
            let value = value.to_vec();
            writers.push(thread::spawn(move || write_atomic(&path, &value).unwrap()));
        }
        for writer in writers {
            writer.join().unwrap();
        }
        let value = fs::read(&*path).unwrap();
        assert!([b"alpha".as_slice(), b"beta", b"gamma", b"delta"].contains(&value.as_slice()));
        assert_eq!(cleanup_abandoned_temporary_files(&directory).unwrap(), 0);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_cleanup_removes_only_managed_temporary_files() {
        let directory = temporary_directory("cleanup");
        fs::create_dir_all(directory.join("nested")).unwrap();
        let managed = temporary_file_path(&directory.join("nested/movie.mp4"), true).unwrap();
        fs::write(&managed, b"partial").unwrap();
        let unrelated = directory.join("nested/user.tmp");
        fs::write(&unrelated, b"keep").unwrap();
        assert_eq!(cleanup_abandoned_temporary_files(&directory).unwrap(), 1);
        assert!(!managed.exists());
        assert!(unrelated.exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
