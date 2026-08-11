use std::{fs, path::Path};

use anyhow::{Context, Result};
use fractal_renderer_core::RenderedImage;

/// Encodes a tightly packed render result as PNG.
///
/// Keeping encoding outside `renderer-core` leaves room for HDR/OpenEXR sinks
/// without changing GPU rendering and readback.
pub fn save_png(path: &Path, image: &RenderedImage) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create output directory {}", parent.display()))?;
    }
    image::save_buffer_with_format(
        path,
        image.pixels(),
        image.width(),
        image.height(),
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .context("PNG encoder failed")
}
