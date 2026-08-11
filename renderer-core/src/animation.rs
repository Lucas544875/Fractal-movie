use anyhow::{Context, Result, bail};

use crate::{
    ExponentialDivePath, FractalConfig, MIN_QUAD_CAMERA_DISTANCE, MultiTargetDivePath, Precision,
    Qf32, QfVec3, RenderConfig, SurfaceFlyoverPath, TargetOrbitPath,
};

/// Safety ceilings for scene-driven image sequences.
pub const MAX_ANIMATION_FPS: u32 = 240;
pub const MAX_ANIMATION_FRAMES: u32 = 1_000_000;

/// A validated animation timeline whose frame times are deterministic.
#[derive(Clone, Debug)]
pub struct AnimationConfig {
    pub fps: u32,
    pub frame_count: u32,
    pub path: AnimationPath,
}

#[derive(Clone, Debug)]
pub enum AnimationPath {
    ExponentialDive(ExponentialDivePath),
    TargetOrbit(TargetOrbitPath),
    MultiTargetDive(MultiTargetDivePath),
    SurfaceFlyover(SurfaceFlyoverPath),
}

/// A complete per-frame sample, ready to pass to [`crate::Renderer`].
#[derive(Clone, Debug)]
pub struct AnimationFrame {
    pub index: u32,
    pub time_seconds: f64,
    pub camera_distance: Qf32,
    pub config: RenderConfig,
}

impl AnimationConfig {
    /// Resolves automatic path searches once from the CPU distance estimator.
    /// Calling this again deliberately replans from the effective scene seed,
    /// which keeps CLI seed overrides deterministic.
    pub fn plan(&mut self, base: &RenderConfig) -> Result<()> {
        if self.fps == 0 || self.fps > MAX_ANIMATION_FPS {
            bail!("animation fps must be in 1..={MAX_ANIMATION_FPS}");
        }
        if self.frame_count == 0 || self.frame_count > MAX_ANIMATION_FRAMES {
            bail!("animation frame_count must be in 1..={MAX_ANIMATION_FRAMES}");
        }
        let light_direction = base.light.direction.map(f64::from);
        match &mut self.path {
            AnimationPath::ExponentialDive(_) | AnimationPath::TargetOrbit(_) => {}
            AnimationPath::MultiTargetDive(path) => {
                if base.precision != Precision::F32 {
                    bail!("multi-target-dive currently requires f32 precision");
                }
                path.validate_parameters()
                    .context("invalid multi-target-dive path")?;
                let cycle_duration = path.cycle_duration();
                if cycle_duration < 1.0 / f64::from(self.fps) {
                    bail!("multi-target-dive cycle duration must be at least one animation frame");
                }
                let final_time =
                    f64::from(self.frame_count.saturating_sub(1)) / f64::from(self.fps.max(1));
                let final_cycle = (final_time / cycle_duration).floor() as usize;
                let final_local_time = final_time - final_cycle as f64 * cycle_duration;
                let target_count = final_cycle
                    + 1
                    + usize::from(final_local_time >= path.overview_duration + path.dive_duration);
                match &base.fractal {
                    FractalConfig::Mandelbulb(fractal) => {
                        path.plan(fractal, light_direction, base.seed, target_count.max(1))?
                    }
                    FractalConfig::Mandelbox(fractal) => {
                        path.plan(fractal, light_direction, base.seed, target_count.max(1))?
                    }
                    FractalConfig::Dsl(fractal) => {
                        path.plan(fractal, light_direction, base.seed, target_count.max(1))?
                    }
                }
            }
            AnimationPath::SurfaceFlyover(path) => {
                if base.precision != Precision::F32 {
                    bail!("surface-flyover currently requires f32 precision");
                }
                path.validate_parameters()
                    .context("invalid surface-flyover path")?;
                match &base.fractal {
                    FractalConfig::Mandelbulb(fractal) => {
                        path.plan(fractal, light_direction, base.seed)?
                    }
                    FractalConfig::Mandelbox(fractal) => {
                        path.plan(fractal, light_direction, base.seed)?
                    }
                    FractalConfig::Dsl(fractal) => {
                        path.plan(fractal, light_direction, base.seed)?
                    }
                }
            }
        }
        Ok(())
    }

    /// Validates timeline limits and whether the path is representable by the
    /// selected renderer precision.
    pub fn validate(&self, base: &RenderConfig) -> Result<()> {
        if self.fps == 0 || self.fps > MAX_ANIMATION_FPS {
            bail!("animation fps must be in 1..={MAX_ANIMATION_FPS}");
        }
        if self.frame_count == 0 || self.frame_count > MAX_ANIMATION_FRAMES {
            bail!("animation frame_count must be in 1..={MAX_ANIMATION_FRAMES}");
        }

        match &self.path {
            AnimationPath::ExponentialDive(path) => {
                path.validate().context("invalid exponential-dive path")?;
                if base.precision == Precision::QuadFloat {
                    let FractalConfig::Mandelbox(_) = base.fractal else {
                        bail!("quad-float animation is currently supported only for Mandelbox");
                    };
                    if path.minimum_distance.to_f64() < MIN_QUAD_CAMERA_DISTANCE {
                        bail!(
                            "quad-float animation minimum_distance must be at least {MIN_QUAD_CAMERA_DISTANCE:e}"
                        );
                    }
                }
            }
            AnimationPath::TargetOrbit(path) => {
                path.validate().context("invalid target-orbit path")?;
            }
            AnimationPath::MultiTargetDive(path) => {
                if base.precision != Precision::F32 {
                    bail!("multi-target-dive currently requires f32 precision");
                }
                path.validate().context("invalid multi-target-dive path")?;
            }
            AnimationPath::SurfaceFlyover(path) => {
                if base.precision != Precision::F32 {
                    bail!("surface-flyover currently requires f32 precision");
                }
                path.validate().context("invalid surface-flyover path")?;
            }
        }

        // Validate both ends eagerly so a long render cannot fail only when it
        // reaches its deepest frame.
        self.sample(base, 0)
            .context("animation's first frame is invalid")?;
        self.sample(base, self.frame_count - 1)
            .context("animation's last frame is invalid")?;
        Ok(())
    }

    #[must_use]
    pub fn time_for_frame(&self, frame_index: u32) -> f64 {
        f64::from(frame_index) / f64::from(self.fps)
    }

    /// Composes a camera sample in quad-float coordinates. Each path updates
    /// only the camera fields it owns and leaves the rest of the base render
    /// configuration unchanged.
    pub fn sample(&self, base: &RenderConfig, frame_index: u32) -> Result<AnimationFrame> {
        if self.fps == 0 {
            bail!("animation fps must be greater than zero");
        }
        if frame_index >= self.frame_count {
            bail!(
                "animation frame {frame_index} is outside 0..{}",
                self.frame_count
            );
        }
        let time_seconds = self.time_for_frame(frame_index);
        let mut config = base.clone();
        let (camera_distance, fade_to_black) = match &self.path {
            AnimationPath::ExponentialDive(path) => {
                path.validate().context("invalid exponential-dive path")?;
                let camera_distance = path.distance_qf_at(time_seconds);
                let view_direction = (base.camera.target - base.camera.position)
                    .normalized()
                    .context("base camera position and target do not define a view direction")?;
                config.camera.position = config.camera.target - view_direction * camera_distance;
                (camera_distance, 0.0)
            }
            AnimationPath::TargetOrbit(path) => {
                let sample = path.sample(base.camera.target, base.camera.position, time_seconds)?;
                config.camera.position = sample.position;
                config.camera.up = sample.up;
                (sample.camera_distance, 0.0)
            }
            AnimationPath::MultiTargetDive(path) => {
                let sample = path.sample(time_seconds)?;
                config.camera.target = sample.target.point;
                config.camera.position = sample.target.point
                    - QfVec3::from_f64(sample.target.view_direction) * sample.distance;
                (sample.distance, sample.fade_to_black)
            }
            AnimationPath::SurfaceFlyover(path) => {
                let sample = path.sample(time_seconds)?;
                config.camera.position = sample.position;
                config.camera.target = sample.target;
                config.camera.up = sample.up;
                (sample.camera_distance, 0.0)
            }
        };
        if config.precision == Precision::F32 {
            config.camera.position = QfVec3::from_f32(config.camera.position.to_f32());
            config.camera.target = QfVec3::from_f32(config.camera.target.to_f32());
        }
        if config.precision == Precision::QuadFloat {
            config
                .tune_mandelbox_quad_zoom(camera_distance)
                .context("could not tune quad-float Mandelbox for animation frame")?;
        } else {
            config
                .tune_camera_relative_effects(camera_distance.to_f32())
                .context("could not tune camera-relative effects for animation frame")?;
        }
        if fade_to_black > 0.0 {
            config.quality.post_process.enabled = true;
            config.quality.post_process.exposure_stops +=
                (-20.0 - config.quality.post_process.exposure_stops) * fade_to_black;
        }
        config
            .validate()
            .context("sampled render configuration is invalid")?;

        Ok(AnimationFrame {
            index: frame_index,
            time_seconds,
            camera_distance,
            config,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn deep_animation() -> AnimationConfig {
        AnimationConfig {
            fps: 2,
            frame_count: 5,
            path: AnimationPath::ExponentialDive(ExponentialDivePath {
                overview_distance: Qf32::from_f32(1.0),
                minimum_distance: Qf32::from_str("1e-26").unwrap(),
                overview_duration: 0.0,
                dive_duration: 2.0,
            }),
        }
    }

    #[test]
    fn samples_exact_endpoints_and_quad_camera_separation() {
        let base = RenderConfig::mandelbox_quad(12_345, Qf32::ONE).unwrap();
        let animation = deep_animation();
        animation.validate(&base).unwrap();

        let first = animation.sample(&base, 0).unwrap();
        let last = animation.sample(&base, 4).unwrap();
        assert_eq!(first.time_seconds, 0.0);
        assert_eq!(last.time_seconds, 2.0);
        assert_eq!(first.camera_distance, Qf32::ONE);
        assert_eq!(last.camera_distance, Qf32::from_str("1e-26").unwrap());
        assert_eq!(last.config.fractal.iterations(), 82);
        assert_ne!(last.config.camera.position, last.config.camera.target);
        assert_eq!(
            last.config.camera.position.to_f32(),
            last.config.camera.target.to_f32()
        );
    }

    #[test]
    fn rejects_out_of_range_frames_and_unsafe_depths() {
        let base = RenderConfig::mandelbox_quad(12_345, Qf32::ONE).unwrap();
        let mut animation = deep_animation();
        assert!(animation.sample(&base, 5).is_err());
        let AnimationPath::ExponentialDive(path) = &mut animation.path else {
            unreachable!();
        };
        path.minimum_distance = Qf32::from_str("1e-27").unwrap();
        assert!(animation.validate(&base).is_err());
    }

    #[test]
    fn target_orbit_keeps_the_target_fixed_and_camera_on_its_cone() {
        let base = RenderConfig::default();
        let animation = AnimationConfig {
            fps: 4,
            frame_count: 17,
            path: AnimationPath::TargetOrbit(TargetOrbitPath {
                radius: Qf32::from_f32(5.0),
                duration: 4.0,
                revolutions: -1.0,
                axis: [0.0, 0.0, 1.0],
                cone_angle_degrees: 35.0,
                start_angle_degrees: 20.0,
            }),
        };
        animation.validate(&base).unwrap();

        let expected_height = 35_f64.to_radians().cos();
        let first = animation.sample(&base, 0).unwrap();
        let quarter = animation.sample(&base, 4).unwrap();
        let last = animation.sample(&base, 16).unwrap();
        for frame in [&first, &quarter, &last] {
            assert_eq!(frame.config.camera.target, base.camera.target);
            assert_eq!(frame.camera_distance, Qf32::from_f32(5.0));
            let offset = (frame.config.camera.position - frame.config.camera.target)
                .normalized_to_f32()
                .unwrap();
            assert!((f64::from(offset[2]) - expected_height).abs() < 1.0e-6);
        }
        assert_ne!(first.config.camera.position, quarter.config.camera.position);
        assert_eq!(first.config.camera.position, last.config.camera.position);
    }

    #[test]
    fn f32_animation_scales_lens_and_secondary_effect_ranges() {
        let mut base = RenderConfig::default();
        base.camera.focus_distance = 1.0;
        base.camera.aperture_radius = 0.1;
        base.quality.ambient_occlusion.radius = 0.5;
        base.quality.soft_shadow.max_distance = 2.0;
        base.quality.reflection.max_distance = 3.0;
        let animation = AnimationConfig {
            fps: 1,
            frame_count: 2,
            path: AnimationPath::ExponentialDive(ExponentialDivePath {
                overview_distance: Qf32::ONE,
                minimum_distance: Qf32::from_f32(0.1),
                overview_duration: 0.0,
                dive_duration: 1.0,
            }),
        };
        let last = animation.sample(&base, 1).unwrap();
        assert!((last.config.camera.focus_distance - 0.1).abs() < 1.0e-7);
        assert!((last.config.camera.aperture_radius - 0.01).abs() < 1.0e-7);
        assert!((last.config.quality.ambient_occlusion.radius - 0.05).abs() < 1.0e-7);
        assert!((last.config.quality.soft_shadow.max_distance - 0.2).abs() < 1.0e-7);
        assert!((last.config.quality.reflection.max_distance - 0.3).abs() < 1.0e-7);
    }
}
