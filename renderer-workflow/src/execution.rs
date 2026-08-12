use std::time::Instant;

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{
    AnimationFrame, LoadedScene, RenderConfig, RenderRegion, RenderedImage, Renderer,
    RendererOptions,
};

use crate::RenderEnvironment;

/// Operator-controlled adapter and pacing settings shared by every frontend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RendererPolicy {
    pub allow_software: bool,
    pub adapter: Option<String>,
    /// Average renderer duty cycle in percent.
    pub gpu_duty_cycle: Option<f64>,
}

impl RendererPolicy {
    pub fn validate(&self) -> Result<()> {
        if let Some(percent) = self.gpu_duty_cycle
            && (!percent.is_finite() || !(1.0..=100.0).contains(&percent))
        {
            bail!("GPU duty cycle must be finite and in 1.0..=100.0 percent");
        }
        if self
            .adapter
            .as_ref()
            .is_some_and(|name| name.trim().is_empty())
        {
            bail!("adapter filter must not be empty");
        }
        Ok(())
    }

    fn core_options(&self) -> RendererOptions {
        RendererOptions {
            allow_software_adapter: self.allow_software,
            adapter_name: self.adapter.clone(),
            gpu_duty_cycle: self.gpu_duty_cycle.map(|percent| percent / 100.0),
        }
    }
}

/// Information available once the adapter and render pipeline are ready.
#[derive(Clone, Debug)]
pub struct FrameRenderStart {
    pub environment: RenderEnvironment,
    pub initial_config: RenderConfig,
    pub frame_indices: Vec<u32>,
    pub region: RenderRegion,
    pub pipeline_rebuilt: bool,
}

/// One completed frame passed to an output adapter before its pixels are dropped.
#[derive(Debug)]
pub struct RenderedFrame {
    pub frame: AnimationFrame,
    pub image: RenderedImage,
    pub render_seconds: f64,
}

/// Aggregate result of a synchronous frame execution.
#[derive(Clone, Debug)]
pub struct FrameRenderSummary {
    pub start: FrameRenderStart,
    pub total_seconds: f64,
}

/// Reusable renderer owner. A CLI watch loop can retain this between scene
/// reloads, while one-shot harness jobs simply create it for one operation.
pub struct FrameRenderSession {
    policy: RendererPolicy,
    renderer: Option<Renderer>,
}

impl FrameRenderSession {
    pub fn new(policy: RendererPolicy) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            policy,
            renderer: None,
        })
    }

    #[must_use]
    pub const fn policy(&self) -> &RendererPolicy {
        &self.policy
    }

    /// Samples and renders the requested frames through one pipeline.
    ///
    /// `ready` is called after adapter/pipeline initialization, `progress`
    /// before each frame and once at completion, and `sink` after each frame.
    /// Keeping artifact publication in the sink lets the CLI and harness share
    /// GPU execution without forcing them to share output layout.
    pub fn render_frames<Ready, Progress, Sink>(
        &mut self,
        scene: &LoadedScene,
        frame_indices: &[u32],
        region: Option<RenderRegion>,
        mut ready: Ready,
        mut progress: Progress,
        mut sink: Sink,
    ) -> Result<FrameRenderSummary>
    where
        Ready: FnMut(&FrameRenderStart) -> Result<()>,
        Progress: FnMut(u32, u32) -> Result<()>,
        Sink: FnMut(&RenderedFrame) -> Result<()>,
    {
        let first_index = *frame_indices
            .first()
            .context("frame execution requires at least one frame")?;
        let first_frame = sample_scene_frame(scene, first_index)?;
        let region = region.unwrap_or_else(|| {
            RenderRegion::full(
                first_frame.config.render.width,
                first_frame.config.render.height,
            )
        });
        let pipeline_rebuilt = self
            .renderer
            .as_ref()
            .is_none_or(|renderer| !renderer.supports_config(&first_frame.config));
        if pipeline_rebuilt {
            self.renderer = Some(
                pollster::block_on(Renderer::new_with_options(
                    first_frame.config.clone(),
                    self.policy.core_options(),
                ))
                .context("could not initialize the offscreen renderer")?,
            );
        }
        let renderer = self
            .renderer
            .as_ref()
            .expect("frame renderer was initialized");
        let start = FrameRenderStart {
            environment: RenderEnvironment::from_renderer(renderer),
            initial_config: first_frame.config.clone(),
            frame_indices: frame_indices.to_vec(),
            region,
            pipeline_rebuilt,
        };
        ready(&start)?;

        let total_started = Instant::now();
        let total = u32::try_from(frame_indices.len()).context("too many requested frames")?;
        for (position, frame_index) in frame_indices.iter().copied().enumerate() {
            progress(position as u32, total)?;
            let frame = if frame_index == first_frame.index {
                first_frame.clone()
            } else {
                sample_scene_frame(scene, frame_index)?
            };
            let frame_started = Instant::now();
            let image = renderer
                .render_region_with_config(
                    &frame.config,
                    frame.index,
                    frame.time_seconds as f32,
                    region,
                )
                .with_context(|| format!("failed to render frame {frame_index}"))?;
            sink(&RenderedFrame {
                frame,
                image,
                render_seconds: frame_started.elapsed().as_secs_f64(),
            })?;
        }
        progress(total, total)?;
        Ok(FrameRenderSummary {
            start,
            total_seconds: total_started.elapsed().as_secs_f64(),
        })
    }
}

/// Resolves one static or animated frame without involving an output adapter.
pub fn sample_scene_frame(scene: &LoadedScene, frame_index: u32) -> Result<AnimationFrame> {
    if let Some(animation) = &scene.animation {
        return animation
            .sample(&scene.config, frame_index)
            .with_context(|| format!("could not sample animation frame {frame_index}"));
    }
    if frame_index != 0 {
        bail!("static scene has only frame 0");
    }
    let camera_distance = (scene.config.camera.target - scene.config.camera.position)
        .length_squared()
        .sqrt();
    Ok(AnimationFrame {
        index: 0,
        time_seconds: 0.0,
        camera_distance,
        config: scene.config.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_policy_rejects_invalid_limits_and_empty_adapters() {
        assert!(RendererPolicy::default().validate().is_ok());
        for gpu_duty_cycle in [Some(0.0), Some(100.1), Some(f64::NAN)] {
            assert!(
                RendererPolicy {
                    gpu_duty_cycle,
                    ..RendererPolicy::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            RendererPolicy {
                adapter: Some("  ".to_owned()),
                ..RendererPolicy::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn static_scene_sampling_accepts_only_frame_zero() {
        let scene = LoadedScene {
            name: "static".to_owned(),
            config: RenderConfig::default(),
            animation: None,
            video: None,
        };
        assert_eq!(sample_scene_frame(&scene, 0).unwrap().index, 0);
        assert!(sample_scene_frame(&scene, 1).is_err());
    }
}
