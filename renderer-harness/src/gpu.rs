//! Binds the harness to the GPU renderer. Everything that touches the GPU
//! goes through here so duty-cycle limits and time accounting apply
//! uniformly.

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{
    AnimationConfig, LoadedScene, RenderConfig, RenderRegion, RenderedImage, Renderer,
    RendererOptions,
};
use serde::Serialize;

use crate::{
    metrics::{Frame, LEGIBILITY_WIDTH, Legibility, legibility},
    quality::{Probe, Sample, Settings},
};

/// Samples used for legibility thumbnails; enough to remove aliasing.
const THUMBNAIL_SPP: u32 = 4;

#[derive(Clone, Debug, Default)]
pub struct GpuOptions {
    /// Average GPU duty cycle in percent, `1..=100`.
    pub duty_cycle_percent: Option<f64>,
    pub allow_software: bool,
    pub adapter: Option<String>,
}

impl GpuOptions {
    fn renderer_options(&self) -> Result<RendererOptions> {
        let gpu_duty_cycle = match self.duty_cycle_percent {
            None => None,
            Some(percent) if percent.is_finite() && (1.0..=100.0).contains(&percent) => {
                Some(percent / 100.0)
            }
            Some(percent) => bail!("GPU duty cycle {percent} must be in 1..=100 percent"),
        };
        Ok(RendererOptions {
            allow_software_adapter: self.allow_software,
            adapter_name: self.adapter.clone(),
            gpu_duty_cycle,
        })
    }

    pub(crate) fn renderer(&self, config: RenderConfig) -> Result<Renderer> {
        pollster::block_on(Renderer::new_with_options(config, self.renderer_options()?))
            .context("could not initialize the offscreen renderer")
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FrameLegibility {
    pub frame: u32,
    #[serde(flatten)]
    pub legibility: Legibility,
}

/// Renders probe frames of one scene at its final resolution.
pub struct SceneProbe {
    renderer: Renderer,
    base: RenderConfig,
    animation: Option<AnimationConfig>,
    region: RenderRegion,
    /// Share of the frame's pixels covered by `region`.
    region_share: f64,
}

impl SceneProbe {
    /// `region_fraction` is the side of the centred probe region as a
    /// fraction of the frame. The centre usually holds the subject and so
    /// the most expensive pixels, which keeps the extrapolated frame time on
    /// the pessimistic side.
    pub fn new(scene: &LoadedScene, options: &GpuOptions, region_fraction: f64) -> Result<Self> {
        if !region_fraction.is_finite() || !(0.05..=1.0).contains(&region_fraction) {
            bail!("probe region fraction {region_fraction} must be in 0.05..=1");
        }
        let (width, height) = (scene.config.render.width, scene.config.render.height);
        let region_width = probe_extent(width, region_fraction);
        let region_height = probe_extent(height, region_fraction);
        let region = RenderRegion {
            x: (width - region_width) / 2,
            y: (height - region_height) / 2,
            width: region_width,
            height: region_height,
        };
        Ok(Self {
            renderer: options.renderer(scene.config.clone())?,
            base: scene.config.clone(),
            animation: scene.animation.clone(),
            region,
            region_share: f64::from(region_width) * f64::from(region_height)
                / (f64::from(width) * f64::from(height)),
        })
    }

    #[must_use]
    pub fn adapter_name(&self) -> &str {
        &self.renderer.adapter_info().name
    }

    #[must_use]
    pub fn frame_count(&self) -> u32 {
        self.animation
            .as_ref()
            .map_or(1, |animation| animation.frame_count)
    }

    /// First, middle, and last frame of the scene.
    #[must_use]
    pub fn probe_frames(&self) -> Vec<u32> {
        let last = self.frame_count() - 1;
        let mut frames = vec![0, last / 2, last];
        frames.dedup();
        frames
    }

    /// The quality the scene itself asks for.
    #[must_use]
    pub fn requested(&self) -> Settings {
        let quality = &self.base.quality;
        let steps = |max_steps: u32, strength: f32| if strength > 0.0 { max_steps } else { 0 };
        Settings {
            samples_per_pixel: quality.samples_per_pixel,
            ambient_occlusion_steps: steps(
                quality.ambient_occlusion.max_steps,
                quality.ambient_occlusion.strength,
            ),
            soft_shadow_steps: quality.soft_shadow.max_steps,
            reflection_steps: steps(quality.reflection.max_steps, quality.reflection.strength),
            depth_of_field: self.base.camera.aperture_radius > 0.0,
            pixel_epsilon_multiplier: self.base.render.pixel_epsilon_multiplier,
        }
    }
}

impl Probe for SceneProbe {
    fn render(&mut self, frame_index: u32, settings: &Settings) -> Result<Sample> {
        let (mut config, time_seconds) =
            frame_config(&self.base, self.animation.as_ref(), frame_index)?;
        apply_settings(&mut config, settings);
        let image = self
            .renderer
            .render_region_with_config(&config, frame_index, time_seconds, self.region)
            .with_context(|| format!("could not render probe frame {frame_index}"))?;
        let gpu_seconds = image.gpu_time().as_secs_f64();
        Ok(Sample {
            frame: Frame::from_rgba8(image.width(), image.height(), image.pixels())?,
            gpu_seconds,
            frame_seconds: gpu_seconds / self.region_share,
        })
    }
}

/// A whole frame rendered at thumbnail size with the scene's own look.
pub struct Thumbnail {
    pub frame_index: u32,
    pub image: RenderedImage,
    /// Per-frame configuration the thumbnail was rendered with.
    pub config: RenderConfig,
}

/// Renders whole frames at thumbnail size. Returns the thumbnails and the
/// GPU seconds they cost.
pub fn render_thumbnails(
    scene: &LoadedScene,
    frames: &[u32],
    options: &GpuOptions,
) -> Result<(Vec<Thumbnail>, f64)> {
    let mut base = scene.config.clone();
    let (width, height) = (base.render.width, base.render.height);
    if width > LEGIBILITY_WIDTH {
        base.render.width = LEGIBILITY_WIDTH;
        // Rounded to the nearest pixel so the aspect ratio is preserved.
        base.render.height = ((u64::from(height) * u64::from(LEGIBILITY_WIDTH)
            + u64::from(width) / 2)
            / u64::from(width))
        .max(1) as u32;
    }
    // Composition and palette decide how a frame reads; secondary effects
    // only modulate them and are not worth their cost at this size.
    let settings = Settings {
        samples_per_pixel: THUMBNAIL_SPP,
        ambient_occlusion_steps: 0,
        soft_shadow_steps: 0,
        reflection_steps: 0,
        depth_of_field: false,
        pixel_epsilon_multiplier: base.render.pixel_epsilon_multiplier,
    };
    let renderer = options.renderer(base.clone())?;

    let mut gpu_seconds = 0.0;
    let mut thumbnails = Vec::with_capacity(frames.len());
    for frame in frames {
        let (mut config, time_seconds) = frame_config(&base, scene.animation.as_ref(), *frame)?;
        apply_settings(&mut config, &settings);
        let image = renderer
            .render_frame_with_config(&config, *frame, time_seconds)
            .with_context(|| format!("could not render thumbnail of frame {frame}"))?;
        gpu_seconds += image.gpu_time().as_secs_f64();
        thumbnails.push(Thumbnail {
            frame_index: *frame,
            image,
            config,
        });
    }
    Ok((thumbnails, gpu_seconds))
}

impl Thumbnail {
    pub fn frame(&self) -> Result<Frame> {
        Frame::from_rgba8(self.image.width(), self.image.height(), self.image.pixels())
    }
}

/// Measures legibility of whole frames at thumbnail size. Returns the
/// measurements and the GPU seconds they cost.
pub fn measure_legibility(
    scene: &LoadedScene,
    frames: &[u32],
    options: &GpuOptions,
) -> Result<(Vec<FrameLegibility>, f64)> {
    let (thumbnails, gpu_seconds) = render_thumbnails(scene, frames, options)?;
    let measured = thumbnails
        .iter()
        .map(|thumbnail| {
            Ok(FrameLegibility {
                frame: thumbnail.frame_index,
                legibility: legibility(&thumbnail.frame()?),
            })
        })
        .collect::<Result<_>>()?;
    Ok((measured, gpu_seconds))
}

/// First, middle, and last frame of a scene.
#[must_use]
pub fn key_frames(scene: &LoadedScene) -> Vec<u32> {
    let last = scene
        .animation
        .as_ref()
        .map_or(0, |animation| animation.frame_count - 1);
    let mut frames = vec![0, last / 2, last];
    frames.dedup();
    frames
}

/// A fraction of the extent, kept even so chroma-subsampled comparisons
/// stay aligned.
fn probe_extent(extent: u32, fraction: f64) -> u32 {
    let side = (f64::from(extent) * fraction) as u32;
    (side - side % 2).clamp(1, extent)
}

pub(crate) fn frame_config(
    base: &RenderConfig,
    animation: Option<&AnimationConfig>,
    frame_index: u32,
) -> Result<(RenderConfig, f32)> {
    match animation {
        Some(animation) => {
            let frame = animation
                .sample(base, frame_index)
                .with_context(|| format!("could not sample frame {frame_index}"))?;
            Ok((frame.config, frame.time_seconds as f32))
        }
        None if frame_index == 0 => Ok((base.clone(), 0.0)),
        None => bail!("a static scene has only frame 0"),
    }
}

pub(crate) fn apply_settings(config: &mut RenderConfig, settings: &Settings) {
    config.quality.samples_per_pixel = settings.samples_per_pixel;
    config.quality.ambient_occlusion.max_steps = settings.ambient_occlusion_steps;
    config.quality.soft_shadow.max_steps = settings.soft_shadow_steps;
    config.quality.reflection.max_steps = settings.reflection_steps;
    if !settings.depth_of_field {
        config.camera.aperture_radius = 0.0;
    }
    config.render.pixel_epsilon_multiplier = settings.pixel_epsilon_multiplier;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duty_cycle_percent_is_validated_and_converted() {
        let options = |percent| GpuOptions {
            duty_cycle_percent: percent,
            ..GpuOptions::default()
        };
        assert_eq!(
            options(None).renderer_options().unwrap().gpu_duty_cycle,
            None
        );
        assert_eq!(
            options(Some(40.0))
                .renderer_options()
                .unwrap()
                .gpu_duty_cycle,
            Some(0.4)
        );
        for invalid in [0.0, 100.5, f64::NAN] {
            assert!(options(Some(invalid)).renderer_options().is_err());
        }
    }

    #[test]
    fn probe_extent_is_even_and_inside_the_frame() {
        assert_eq!(probe_extent(1920, 0.5), 960);
        assert_eq!(probe_extent(1920, 0.25), 480);
        assert_eq!(probe_extent(1920, 1.0), 1920);
        assert_eq!(probe_extent(1080, 0.5), 540);
        assert_eq!(probe_extent(810, 0.5), 404);
        assert_eq!(probe_extent(1, 0.5), 1);
        assert_eq!(probe_extent(2, 0.5), 1);
    }

    #[test]
    fn settings_override_quality_without_touching_the_look() {
        let mut config = RenderConfig::default();
        config.camera.aperture_radius = 0.02;
        config.quality.ambient_occlusion.strength = 0.6;
        let settings = Settings {
            samples_per_pixel: 8,
            ambient_occlusion_steps: 24,
            soft_shadow_steps: 0,
            reflection_steps: 12,
            depth_of_field: true,
            pixel_epsilon_multiplier: 1.5,
        };
        apply_settings(&mut config, &settings);
        assert_eq!(config.quality.samples_per_pixel, 8);
        assert_eq!(config.quality.ambient_occlusion.max_steps, 24);
        assert_eq!(config.quality.ambient_occlusion.strength, 0.6);
        assert_eq!(config.quality.reflection.max_steps, 12);
        assert_eq!(config.camera.aperture_radius, 0.02);
        assert_eq!(config.render.pixel_epsilon_multiplier, 1.5);
        config.validate().unwrap();

        apply_settings(
            &mut config,
            &Settings {
                depth_of_field: false,
                ..settings
            },
        );
        assert_eq!(config.camera.aperture_radius, 0.0);
    }

    #[test]
    fn static_scenes_expose_a_single_frame() {
        let base = RenderConfig::default();
        assert!(frame_config(&base, None, 0).is_ok());
        assert!(frame_config(&base, None, 1).is_err());
    }
}
