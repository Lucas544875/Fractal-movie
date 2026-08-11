use anyhow::{Context, Result, bail};

use crate::{
    ExponentialDivePath, FractalConfig, MIN_QUAD_CAMERA_DISTANCE, Precision, Qf32, QfVec3,
    RenderConfig,
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

    /// Composes a camera sample in quad-float coordinates. The base camera
    /// supplies the target and viewing direction; the path supplies only its
    /// distance, so future path-selection algorithms can be reused unchanged.
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
        let camera_distance = match &self.path {
            AnimationPath::ExponentialDive(path) => {
                path.validate().context("invalid exponential-dive path")?;
                path.distance_qf_at(time_seconds)
            }
        };
        let view_direction = (base.camera.target - base.camera.position)
            .normalized()
            .context("base camera position and target do not define a view direction")?;

        let mut config = base.clone();
        config.camera.position = config.camera.target - view_direction * camera_distance;
        if config.precision == Precision::F32 {
            config.camera.position = QfVec3::from_f32(config.camera.position.to_f32());
        }
        if config.precision == Precision::QuadFloat {
            config
                .tune_mandelbox_quad_zoom(camera_distance)
                .context("could not tune quad-float Mandelbox for animation frame")?;
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
        let AnimationPath::ExponentialDive(path) = &mut animation.path;
        path.minimum_distance = Qf32::from_str("1e-27").unwrap();
        assert!(animation.validate(&base).is_err());
    }
}
