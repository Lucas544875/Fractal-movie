use std::{collections::BTreeSet, path::Path, time::Instant};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{AnimationConfig, LoadedScene, RenderRegion};
use image::{Rgba, RgbaImage, imageops};
use serde::{Deserialize, Serialize};

use crate::{
    Artifact, FrameRenderSession, ImageMetrics, ProjectStore, RenderEnvironment, RendererPolicy,
    artifact::{save_png_atomic, write_atomic},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewProfile {
    Composition,
    #[default]
    Lookdev,
    Proof,
    Final,
}

impl PreviewProfile {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Composition => "composition",
            Self::Lookdev => "lookdev",
            Self::Proof => "proof",
            Self::Final => "final",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreviewRequest {
    pub project_id: String,
    #[serde(default)]
    pub revision_id: Option<String>,
    #[serde(default)]
    pub profile: PreviewProfile,
    #[serde(default)]
    pub frames: Vec<u32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// Normalized x, y, width, height crop in the full camera projection.
    #[serde(default)]
    pub region: Option<[f64; 4]>,
    #[serde(default)]
    pub render_passes: Vec<String>,
    #[serde(default)]
    pub gpu_duty_cycle: Option<f64>,
    #[serde(default)]
    pub allow_software: bool,
    #[serde(default)]
    pub adapter: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreviewFrameResult {
    pub frame_index: u32,
    pub time_seconds: f64,
    pub render_seconds: f64,
    pub metrics: ImageMetrics,
    pub artifact: Artifact,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreviewResult {
    pub project_id: String,
    pub revision_id: String,
    pub profile: PreviewProfile,
    pub environment: RenderEnvironment,
    pub frames: Vec<PreviewFrameResult>,
    pub contact_sheet: Artifact,
    pub metrics_manifest: Artifact,
    pub total_seconds: f64,
}

pub fn render_preview(
    store: &ProjectStore,
    request: &PreviewRequest,
    output_directory: &Path,
) -> Result<PreviewResult> {
    render_preview_with_progress(store, request, output_directory, &mut |_, _| Ok(()))
}

pub(crate) fn render_preview_with_progress(
    store: &ProjectStore,
    request: &PreviewRequest,
    output_directory: &Path,
    progress: &mut dyn FnMut(u32, u32) -> Result<()>,
) -> Result<PreviewResult> {
    validate_request(request)?;
    let (revision, spec) = store.scene(&request.project_id, request.revision_id.as_deref())?;
    let mut scene = spec.resolve()?;
    apply_preview_profile(&mut scene, request.profile, request.width, request.height)?;
    let frame_indices = preview_frame_indices(scene.animation.as_ref(), &request.frames)?;
    let region = normalized_render_region(
        scene.config.render.width,
        scene.config.render.height,
        request.region,
    )?;
    let mut session = FrameRenderSession::new(RendererPolicy {
        allow_software: request.allow_software,
        adapter: request.adapter.clone(),
        gpu_duty_cycle: request.gpu_duty_cycle,
    })?;

    std::fs::create_dir_all(output_directory).with_context(|| {
        format!(
            "could not create preview output {}",
            output_directory.display()
        )
    })?;
    let total_started = Instant::now();
    let mut frames = Vec::with_capacity(frame_indices.len());
    let mut environment = None;
    session
        .render_frames(
            &scene,
            &frame_indices,
            Some(region),
            |start| {
                environment = Some(start.environment.clone());
                Ok(())
            },
            &mut *progress,
            |rendered| {
                let frame_index = rendered.frame.index;
                let image = RgbaImage::from_raw(
                    rendered.image.width(),
                    rendered.image.height(),
                    rendered.image.pixels().to_vec(),
                )
                .context("renderer returned an invalid RGBA image")?;
                let metrics = ImageMetrics::from_rgba(&image);
                let path = output_directory.join(format!("frame_{frame_index:06}.png"));
                save_png_atomic(&path, &image)?;
                let mut artifact = Artifact::from_file("preview-frame", "image/png", path)?;
                artifact
                    .metadata
                    .insert("frame_index".to_owned(), serde_json::json!(frame_index));
                artifact.metadata.insert(
                    "render_pass".to_owned(),
                    serde_json::Value::String("beauty".to_owned()),
                );
                frames.push(PreviewFrameResult {
                    frame_index,
                    time_seconds: rendered.frame.time_seconds,
                    render_seconds: rendered.render_seconds,
                    metrics,
                    artifact,
                });
                Ok(())
            },
        )
        .context("could not execute preview frames")?;
    let environment = environment.context("preview renderer did not report its environment")?;

    let contact_sheet_path = output_directory.join("contact-sheet.png");
    create_contact_sheet(&frames, &contact_sheet_path)?;
    let contact_sheet = Artifact::from_file("contact-sheet", "image/png", contact_sheet_path)?;

    let metrics_path = output_directory.join("metrics.json");
    let metrics_bytes = serde_json::to_vec_pretty(&frames).context("could not encode metrics")?;
    write_atomic(&metrics_path, &metrics_bytes)?;
    let metrics_manifest =
        Artifact::from_file("preview-metrics", "application/json", metrics_path)?;

    Ok(PreviewResult {
        project_id: request.project_id.clone(),
        revision_id: revision.id,
        profile: request.profile,
        environment,
        frames,
        contact_sheet,
        metrics_manifest,
        total_seconds: total_started.elapsed().as_secs_f64(),
    })
}

fn validate_request(request: &PreviewRequest) -> Result<()> {
    if let Some(percent) = request.gpu_duty_cycle
        && (!percent.is_finite() || !(1.0..=100.0).contains(&percent))
    {
        bail!("preview gpu_duty_cycle must be in 1.0..=100.0 percent");
    }
    if request.render_passes.iter().any(|pass| pass != "beauty") {
        bail!("this renderer version supports only the beauty preview pass");
    }
    if let Some(region) = request.region {
        let [x, y, width, height] = region;
        if region.iter().any(|value| !value.is_finite())
            || x < 0.0
            || y < 0.0
            || width <= 0.0
            || height <= 0.0
            || x + width > 1.0
            || y + height > 1.0
        {
            bail!("preview region must be positive and contained in normalized coordinates");
        }
    }
    Ok(())
}

pub fn apply_preview_profile(
    scene: &mut LoadedScene,
    profile: PreviewProfile,
    width_override: Option<u32>,
    height_override: Option<u32>,
) -> Result<()> {
    let source_width = scene.config.render.width;
    let source_height = scene.config.render.height;
    let maximum_width = match profile {
        PreviewProfile::Composition => Some(320),
        PreviewProfile::Lookdev => Some(480),
        PreviewProfile::Proof => Some(810),
        PreviewProfile::Final => None,
    };
    if let Some(maximum_width) = maximum_width
        && source_width > maximum_width
    {
        scene.config.render.width = maximum_width;
        scene.config.render.height = scaled_dimension(source_height, source_width, maximum_width);
    }
    match (width_override, height_override) {
        (Some(width), Some(height)) => {
            scene.config.render.width = width;
            scene.config.render.height = height;
        }
        (Some(width), None) => {
            scene.config.render.width = width;
            scene.config.render.height = scaled_dimension(source_height, source_width, width);
        }
        (None, Some(height)) => {
            scene.config.render.height = height;
            scene.config.render.width = scaled_dimension(source_width, source_height, height);
        }
        (None, None) => {}
    }

    match profile {
        PreviewProfile::Composition => {
            scene.config.quality.samples_per_pixel = 1;
            scene.config.camera.aperture_radius = 0.0;
            scene.config.render.max_steps = scene.config.render.max_steps.min(256);
            scene.config.quality.ambient_occlusion.max_steps = 0;
            scene.config.quality.soft_shadow.max_steps = 0;
            scene.config.quality.reflection.max_steps = 0;
        }
        PreviewProfile::Lookdev => {
            scene.config.quality.samples_per_pixel = scene.config.quality.samples_per_pixel.min(8);
            scene.config.camera.aperture_radius = 0.0;
            scene.config.quality.ambient_occlusion.max_steps =
                scene.config.quality.ambient_occlusion.max_steps.min(24);
            scene.config.quality.soft_shadow.max_steps =
                scene.config.quality.soft_shadow.max_steps.min(32);
            scene.config.quality.reflection.max_steps =
                scene.config.quality.reflection.max_steps.min(16);
        }
        PreviewProfile::Proof => {
            scene.config.quality.samples_per_pixel = scene.config.quality.samples_per_pixel.min(32);
            scene.config.quality.ambient_occlusion.max_steps =
                scene.config.quality.ambient_occlusion.max_steps.min(48);
            scene.config.quality.soft_shadow.max_steps =
                scene.config.quality.soft_shadow.max_steps.min(64);
            scene.config.quality.reflection.max_steps =
                scene.config.quality.reflection.max_steps.min(32);
        }
        PreviewProfile::Final => {}
    }
    scene
        .config
        .validate()
        .context("preview profile is invalid")?;
    if let Some(animation) = &scene.animation {
        animation
            .validate(&scene.config)
            .context("preview profile is incompatible with the animation")?;
    }
    Ok(())
}

pub fn preview_frame_indices(
    animation: Option<&AnimationConfig>,
    requested_frames: &[u32],
) -> Result<Vec<u32>> {
    let frame_count = animation.map_or(1, |animation| animation.frame_count);
    let requested = if !requested_frames.is_empty() {
        requested_frames.to_vec()
    } else if frame_count == 1 {
        vec![0]
    } else {
        let last = frame_count - 1;
        vec![0, last / 4, last / 2, last.saturating_mul(3) / 4, last]
    };
    let unique = requested.into_iter().collect::<BTreeSet<_>>();
    if let Some(invalid) = unique.iter().find(|&&frame| frame >= frame_count) {
        bail!(
            "preview frame {invalid} is outside this scene's frame range 0..{}",
            frame_count - 1
        );
    }
    Ok(unique.into_iter().collect())
}

pub fn normalized_render_region(
    width: u32,
    height: u32,
    normalized: Option<[f64; 4]>,
) -> Result<RenderRegion> {
    let Some([x, y, region_width, region_height]) = normalized else {
        return Ok(RenderRegion::full(width, height));
    };
    validate_normalized_region([x, y, region_width, region_height])?;
    let left = (x * f64::from(width)).floor() as u32;
    let top = (y * f64::from(height)).floor() as u32;
    let right = ((x + region_width) * f64::from(width)).ceil() as u32;
    let bottom = ((y + region_height) * f64::from(height)).ceil() as u32;
    Ok(RenderRegion {
        x: left.min(width - 1),
        y: top.min(height - 1),
        width: right.clamp(left + 1, width) - left,
        height: bottom.clamp(top + 1, height) - top,
    })
}

fn validate_normalized_region(region: [f64; 4]) -> Result<()> {
    let [x, y, width, height] = region;
    if region.iter().any(|value| !value.is_finite())
        || x < 0.0
        || y < 0.0
        || width <= 0.0
        || height <= 0.0
        || x + width > 1.0
        || y + height > 1.0
    {
        bail!("preview region must be positive and contained in normalized coordinates");
    }
    Ok(())
}

fn create_contact_sheet(frames: &[PreviewFrameResult], output: &Path) -> Result<()> {
    let first = frames.first().context("preview produced no frames")?;
    let first_image = image::open(&first.artifact.path)?.to_rgba8();
    let cell_width = first_image.width();
    let cell_height = first_image.height();
    let columns = (frames.len() as f64).sqrt().ceil() as u32;
    let rows = (frames.len() as u32).div_ceil(columns);
    let mut sheet = RgbaImage::from_pixel(
        cell_width * columns,
        cell_height * rows,
        Rgba([12, 12, 16, 255]),
    );
    for (index, frame) in frames.iter().enumerate() {
        let image = image::open(&frame.artifact.path)?.to_rgba8();
        let x = index as u32 % columns * cell_width;
        let y = index as u32 / columns * cell_height;
        imageops::overlay(&mut sheet, &image, i64::from(x), i64::from(y));
    }
    save_png_atomic(output, &sheet)
}

fn scaled_dimension(value: u32, source: u32, target: u32) -> u32 {
    ((u64::from(value) * u64::from(target) + u64::from(source) / 2) / u64::from(source)).max(1)
        as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_frames_include_both_ends() {
        let animation = AnimationConfig {
            fps: 30,
            frame_count: 721,
            path: fractal_renderer_core::AnimationPath::TargetOrbit(
                fractal_renderer_core::TargetOrbitPath {
                    radius: fractal_renderer_core::Qf32::from_f32(1.0),
                    duration: 24.0,
                    revolutions: 0.25,
                    axis: [0.0, 1.0, 0.0],
                    cone_angle_degrees: 90.0,
                    start_angle_degrees: 0.0,
                },
            ),
        };
        assert_eq!(
            preview_frame_indices(Some(&animation), &[]).unwrap(),
            vec![0, 180, 360, 540, 720]
        );
    }
}
