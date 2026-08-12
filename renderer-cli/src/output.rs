use std::{fs, path::Path};

use anyhow::{Context, Result};
use fractal_renderer_core::RenderedImage;

/// Encodes a tightly packed render result as PNG.
///
/// Keeping encoding outside `renderer-core` leaves room for HDR/OpenEXR sinks
/// without changing GPU rendering and readback.
pub fn save_png(path: &Path, image: &RenderedImage, overwrite: bool) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create output directory {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .context("output path must end in a file name")?
        .to_string_lossy();
    let temporary_path = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let encoded = image::save_buffer_with_format(
        &temporary_path,
        image.pixels(),
        image.width(),
        image.height(),
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    );
    if let Err(error) = encoded {
        let _ = fs::remove_file(&temporary_path);
        return Err(error).context("PNG encoder failed");
    }
    if path.exists() && !overwrite {
        let _ = fs::remove_file(&temporary_path);
        anyhow::bail!(
            "output {} appeared while rendering; pass --overwrite to replace it",
            path.display()
        );
    }
    let mut moved = fs::rename(&temporary_path, path);
    // Unix replaces atomically. Windows rename does not replace an existing
    // file, so honor explicit --overwrite with a portable fallback.
    if moved.is_err() && overwrite && path.is_file() {
        fs::remove_file(path)
            .with_context(|| format!("could not replace existing output {}", path.display()))?;
        moved = fs::rename(&temporary_path, path);
    }
    if let Err(error) = moved {
        let _ = fs::remove_file(&temporary_path);
        return Err(error).context("could not move the completed PNG into place");
    }
    Ok(())
}
