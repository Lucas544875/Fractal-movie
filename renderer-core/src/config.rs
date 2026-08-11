use anyhow::{Result, bail};

use crate::QfVec3;

/// Hard safety ceilings shared by validation and the bounded WGSL loops.
pub const MAX_IMAGE_DIMENSION: u32 = 8_192;
pub const MAX_PIXEL_COUNT: u64 = 33_554_432;
pub const MAX_RAY_STEPS: u32 = 1_024;
pub const MAX_FRACTAL_ITERATIONS: u32 = 96;
/// Deepest camera-to-target separation validated for the built-in quad-float
/// Mandelbox path. One decade deeper reaches the representation's guard-bit
/// boundary on the current GL backend.
pub const MIN_QUAD_CAMERA_DISTANCE: f64 = 1.0e-26;

#[derive(Clone, Debug)]
pub struct CameraConfig {
    pub position: QfVec3,
    pub target: QfVec3,
    /// Preferred world-up direction. Mandelbulb uses Y-up; the portfolio
    /// Mandelbox uses the original WebGL scene's Z-up convention.
    pub up: [f32; 3],
    pub vertical_fov_degrees: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FractalKind {
    Mandelbulb,
    Mandelbox,
}

/// Coordinate precision used by the camera, ray marcher, and fractal DE.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Precision {
    #[default]
    F32,
    QuadFloat,
}

#[derive(Clone, Debug)]
pub struct MandelbulbConfig {
    pub power: f32,
    pub iterations: u32,
    pub bailout: f32,
}

impl Default for MandelbulbConfig {
    fn default() -> Self {
        Self {
            power: 8.0,
            iterations: 16,
            bailout: 4.0,
        }
    }
}

/// Parameters from `portfolio/site/src/pages/mandelbox.js`.
#[derive(Clone, Debug)]
pub struct MandelboxConfig {
    pub scale: f32,
    pub min_radius_squared: f32,
    pub fixed_radius_squared: f32,
    pub fold_limit: f32,
    pub iterations: u32,
    /// Conservative sphere containing the fractal, used by CPU path search.
    pub bound_radius: f64,
}

impl Default for MandelboxConfig {
    fn default() -> Self {
        Self {
            scale: -2.18,
            min_radius_squared: 0.60,
            fixed_radius_squared: 2.65,
            fold_limit: 1.14,
            iterations: 16,
            bound_radius: 4.2,
        }
    }
}

#[derive(Clone, Debug)]
pub enum FractalConfig {
    Mandelbulb(MandelbulbConfig),
    Mandelbox(MandelboxConfig),
}

impl FractalConfig {
    #[must_use]
    pub const fn kind(&self) -> FractalKind {
        match self {
            Self::Mandelbulb(_) => FractalKind::Mandelbulb,
            Self::Mandelbox(_) => FractalKind::Mandelbox,
        }
    }

    pub(crate) const fn iterations(&self) -> u32 {
        match self {
            Self::Mandelbulb(config) => config.iterations,
            Self::Mandelbox(config) => config.iterations,
        }
    }

    pub(crate) const fn shader_parameters(&self) -> [f32; 4] {
        match self {
            Self::Mandelbulb(config) => [config.power, config.bailout, 0.0, 0.0],
            Self::Mandelbox(config) => [
                config.scale,
                config.min_radius_squared,
                config.fixed_radius_squared,
                config.fold_limit,
            ],
        }
    }
}

impl Default for FractalConfig {
    fn default() -> Self {
        Self::Mandelbulb(MandelbulbConfig::default())
    }
}

#[derive(Clone, Debug)]
pub struct LightConfig {
    /// Unit direction from the surface towards the directional light.
    pub direction: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct RenderSettings {
    pub width: u32,
    pub height: u32,
    pub max_steps: u32,
    pub max_distance: f32,
    pub epsilon: f32,
    /// Multiplier applied to every distance-estimator step.
    pub step_safety: f32,
    /// Multiplier for distance- and pixel-projected hit precision. Zero keeps
    /// a fixed epsilon.
    pub pixel_epsilon_multiplier: f32,
}

/// Complete in-memory render description.
#[derive(Clone, Debug)]
pub struct RenderConfig {
    pub precision: Precision,
    pub camera: CameraConfig,
    pub fractal: FractalConfig,
    pub light: LightConfig,
    pub render: RenderSettings,
    pub seed: u32,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            precision: Precision::F32,
            camera: CameraConfig {
                position: QfVec3::from_f32([2.5, 1.7, 2.5]),
                target: QfVec3::ZERO,
                up: [0.0, 1.0, 0.0],
                vertical_fov_degrees: 38.0,
            },
            fractal: FractalConfig::default(),
            light: LightConfig {
                direction: [-0.45, 0.75, 0.55],
            },
            render: RenderSettings {
                width: 640,
                height: 360,
                max_steps: 256,
                max_distance: 100.0,
                epsilon: 0.0001,
                step_safety: 1.0,
                pixel_epsilon_multiplier: 0.0,
            },
            seed: 12_345,
        }
    }
}

impl RenderConfig {
    /// Validates CPU-side inputs before allocating GPU resources.
    pub fn validate(&self) -> Result<()> {
        let render = &self.render;
        if render.width == 0 || render.height == 0 {
            bail!("render dimensions must be greater than zero");
        }
        if render.width > MAX_IMAGE_DIMENSION || render.height > MAX_IMAGE_DIMENSION {
            bail!("render dimensions must not exceed {MAX_IMAGE_DIMENSION} on either axis");
        }
        let pixel_count = u64::from(render.width) * u64::from(render.height);
        if pixel_count > MAX_PIXEL_COUNT {
            bail!("render pixel count {pixel_count} exceeds safety limit {MAX_PIXEL_COUNT}");
        }
        if render.max_steps == 0 || render.max_steps > MAX_RAY_STEPS {
            bail!("max_steps must be in 1..={MAX_RAY_STEPS}");
        }
        let iterations = self.fractal.iterations();
        if iterations == 0 || iterations > MAX_FRACTAL_ITERATIONS {
            bail!("fractal iterations must be in 1..={MAX_FRACTAL_ITERATIONS}");
        }
        match &self.fractal {
            FractalConfig::Mandelbulb(config) => {
                finite_positive("fractal power", config.power)?;
                if !(2.0..=32.0).contains(&config.power) {
                    bail!("fractal power must be in 2.0..=32.0");
                }
                finite_positive("fractal bailout", config.bailout)?;
            }
            FractalConfig::Mandelbox(config) => {
                if !config.scale.is_finite()
                    || config.scale.abs() <= 1.0
                    || config.scale.abs() > 4.0
                {
                    bail!("Mandelbox scale magnitude must be in (1.0, 4.0]");
                }
                finite_positive(
                    "Mandelbox minimum radius squared",
                    config.min_radius_squared,
                )?;
                finite_positive(
                    "Mandelbox fixed radius squared",
                    config.fixed_radius_squared,
                )?;
                if config.fixed_radius_squared <= config.min_radius_squared {
                    bail!("Mandelbox fixed radius squared must exceed its minimum radius squared");
                }
                finite_positive("Mandelbox fold limit", config.fold_limit)?;
                if !config.bound_radius.is_finite() || config.bound_radius <= 0.0 {
                    bail!("Mandelbox bound radius must be finite and greater than zero");
                }
            }
        }
        if self.precision == Precision::QuadFloat && self.fractal.kind() != FractalKind::Mandelbox {
            bail!("quad-float precision is currently supported only for Mandelbox scenes");
        }
        finite_positive("max_distance", render.max_distance)?;
        finite_positive("epsilon", render.epsilon)?;
        if render.epsilon >= 0.1 {
            bail!("epsilon must be less than 0.1");
        }
        if !render.step_safety.is_finite() || !(0.0..=1.0).contains(&render.step_safety) {
            bail!("step_safety must be finite and in (0.0, 1.0]");
        }
        if render.step_safety == 0.0 {
            bail!("step_safety must be greater than zero");
        }
        if !render.pixel_epsilon_multiplier.is_finite()
            || !(0.0..=10.0).contains(&render.pixel_epsilon_multiplier)
        {
            bail!("pixel_epsilon_multiplier must be finite and in 0.0..=10.0");
        }
        let fov = self.camera.vertical_fov_degrees;
        if !fov.is_finite() || !(1.0..179.0).contains(&fov) {
            bail!("vertical camera FOV must be finite and in 1.0..179.0 degrees");
        }
        finite_coordinate("camera position", self.camera.position)?;
        finite_coordinate("camera target", self.camera.target)?;
        finite_vector("camera up", self.camera.up)?;
        finite_vector("light direction", self.light.direction)?;
        let forward_qf = self.camera.target - self.camera.position;
        if forward_qf == QfVec3::ZERO {
            bail!("camera position and target must not be equal");
        }
        if squared_length(self.camera.up) < 1.0e-8 {
            bail!("camera up must not be zero");
        }
        let forward = forward_qf
            .normalized_to_f32()
            .ok_or_else(|| anyhow::anyhow!("camera viewing direction could not be normalized"))?;
        if squared_length(cross(forward, self.camera.up)) < 1.0e-8 {
            bail!("camera up must not be parallel to the viewing direction");
        }
        if squared_length(self.light.direction) < 1.0e-8 {
            bail!("light direction must not be zero");
        }
        Ok(())
    }
}

fn finite_positive(name: &str, value: f32) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        bail!("{name} must be finite and greater than zero");
    }
    Ok(())
}

fn finite_vector(name: &str, value: [f32; 3]) -> Result<()> {
    if value.iter().any(|component| !component.is_finite()) {
        bail!("{name} must contain only finite values");
    }
    Ok(())
}

fn finite_coordinate(name: &str, value: QfVec3) -> Result<()> {
    if !value.is_finite() {
        bail!("{name} must contain only finite values");
    }
    Ok(())
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn squared_length(value: [f32; 3]) -> f32 {
    value.iter().map(|component| component * component).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        RenderConfig::default()
            .validate()
            .expect("the built-in Mandelbulb scene must remain valid");
    }

    #[test]
    fn rejects_unbounded_gpu_work() {
        let mut config = RenderConfig::default();
        config.render.max_steps = MAX_RAY_STEPS + 1;
        assert!(config.validate().is_err());

        config = RenderConfig::default();
        let FractalConfig::Mandelbulb(fractal) = &mut config.fractal else {
            panic!("default fractal changed unexpectedly");
        };
        fractal.iterations = MAX_FRACTAL_ITERATIONS + 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_non_finite_values() {
        let mut config = RenderConfig::default();
        let FractalConfig::Mandelbulb(fractal) = &mut config.fractal else {
            panic!("default fractal changed unexpectedly");
        };
        fractal.power = f32::NAN;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_degenerate_camera() {
        let mut config = RenderConfig::default();
        config.camera.target = config.camera.position;
        assert!(config.validate().is_err());
    }
}
