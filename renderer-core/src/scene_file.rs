use std::{fs, path::Path, str::FromStr};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    AnimationConfig, AnimationPath, CameraConfig, ExponentialDivePath, FractalConfig, LightConfig,
    MandelboxConfig, MandelbulbConfig, Precision, Qf32, QfVec3, RenderConfig, RenderSettings,
};

pub const CURRENT_SCENE_VERSION: u32 = 1;

/// A validated scene loaded from the versioned YAML format.
#[derive(Clone, Debug)]
pub struct LoadedScene {
    pub name: String,
    pub config: RenderConfig,
    pub animation: Option<AnimationConfig>,
}

impl LoadedScene {
    /// Serializes the scene using the current schema version.
    pub fn to_yaml(&self) -> Result<String> {
        let document = SceneDocument::from(self);
        serde_yaml_ng::to_string(&document).context("could not serialize scene as YAML")
    }
}

/// Loads and validates a scene file.
pub fn load_scene(path: impl AsRef<Path>) -> Result<LoadedScene> {
    let path = path.as_ref();
    let yaml = fs::read_to_string(path)
        .with_context(|| format!("could not read scene file {}", path.display()))?;
    parse_scene(&yaml).with_context(|| format!("invalid scene file {}", path.display()))
}

/// Parses and validates a scene from a YAML string.
pub fn parse_scene(yaml: &str) -> Result<LoadedScene> {
    let document: SceneDocument =
        serde_yaml_ng::from_str(yaml).context("YAML does not match the scene schema")?;
    document.try_into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneDocument {
    version: u32,
    name: String,
    #[serde(default)]
    precision: ScenePrecision,
    seed: u32,
    camera: SceneCamera,
    fractal: SceneFractal,
    light: SceneLight,
    render: SceneRender,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    animation: Option<SceneAnimation>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ScenePrecision {
    #[default]
    F32,
    QuadFloat,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneCamera {
    position: [SceneScalar; 3],
    target: [SceneScalar; 3],
    up: [f32; 3],
    vertical_fov_degrees: f32,
}

/// Coordinate scalars accept convenient YAML numbers, exact decimal strings,
/// or an exact four-limb expansion emitted by `LoadedScene::to_yaml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum SceneScalar {
    Number(f64),
    Decimal(String),
    Expansion([f32; 4]),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "kebab-case")]
enum SceneFractal {
    Mandelbulb(SceneMandelbulb),
    Mandelbox(SceneMandelbox),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneMandelbulb {
    power: f32,
    iterations: u32,
    bailout: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneMandelbox {
    scale: f32,
    min_radius_squared: f32,
    fixed_radius_squared: f32,
    fold_limit: f32,
    iterations: u32,
    bound_radius: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneLight {
    direction: [f32; 3],
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneRender {
    width: u32,
    height: u32,
    max_steps: u32,
    max_distance: f32,
    epsilon: f32,
    step_safety: f32,
    pixel_epsilon_multiplier: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneAnimation {
    fps: u32,
    frame_count: u32,
    path: SceneAnimationPath,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "kebab-case")]
enum SceneAnimationPath {
    ExponentialDive(SceneExponentialDive),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneExponentialDive {
    overview_distance: SceneScalar,
    minimum_distance: SceneScalar,
    overview_duration: f64,
    dive_duration: f64,
}

impl TryFrom<SceneDocument> for LoadedScene {
    type Error = anyhow::Error;

    fn try_from(document: SceneDocument) -> Result<Self> {
        if document.version != CURRENT_SCENE_VERSION {
            bail!(
                "unsupported scene version {}; this build supports version {}",
                document.version,
                CURRENT_SCENE_VERSION
            );
        }
        validate_scene_name(&document.name)?;
        let precision = match document.precision {
            ScenePrecision::F32 => Precision::F32,
            ScenePrecision::QuadFloat => Precision::QuadFloat,
        };

        let fractal = match document.fractal {
            SceneFractal::Mandelbulb(config) => FractalConfig::Mandelbulb(MandelbulbConfig {
                power: config.power,
                iterations: config.iterations,
                bailout: config.bailout,
            }),
            SceneFractal::Mandelbox(config) => FractalConfig::Mandelbox(MandelboxConfig {
                scale: config.scale,
                min_radius_squared: config.min_radius_squared,
                fixed_radius_squared: config.fixed_radius_squared,
                fold_limit: config.fold_limit,
                iterations: config.iterations,
                bound_radius: config.bound_radius,
            }),
        };
        let config = RenderConfig {
            precision,
            camera: CameraConfig {
                position: parse_coordinate(document.camera.position, precision)
                    .context("invalid camera position")?,
                target: parse_coordinate(document.camera.target, precision)
                    .context("invalid camera target")?,
                up: document.camera.up,
                vertical_fov_degrees: document.camera.vertical_fov_degrees,
            },
            fractal,
            light: LightConfig {
                direction: document.light.direction,
            },
            render: RenderSettings {
                width: document.render.width,
                height: document.render.height,
                max_steps: document.render.max_steps,
                max_distance: document.render.max_distance,
                epsilon: document.render.epsilon,
                step_safety: document.render.step_safety,
                pixel_epsilon_multiplier: document.render.pixel_epsilon_multiplier,
            },
            seed: document.seed,
        };
        config
            .validate()
            .context("scene configuration is invalid")?;

        let animation = document
            .animation
            .map(AnimationConfig::try_from)
            .transpose()
            .context("invalid animation configuration")?;
        if let Some(animation) = &animation {
            animation
                .validate(&config)
                .context("animation configuration is invalid")?;
        }

        Ok(Self {
            name: document.name,
            config,
            animation,
        })
    }
}

impl From<&LoadedScene> for SceneDocument {
    fn from(scene: &LoadedScene) -> Self {
        let fractal = match &scene.config.fractal {
            FractalConfig::Mandelbulb(config) => SceneFractal::Mandelbulb(SceneMandelbulb {
                power: config.power,
                iterations: config.iterations,
                bailout: config.bailout,
            }),
            FractalConfig::Mandelbox(config) => SceneFractal::Mandelbox(SceneMandelbox {
                scale: config.scale,
                min_radius_squared: config.min_radius_squared,
                fixed_radius_squared: config.fixed_radius_squared,
                fold_limit: config.fold_limit,
                iterations: config.iterations,
                bound_radius: config.bound_radius,
            }),
        };
        Self {
            version: CURRENT_SCENE_VERSION,
            name: scene.name.clone(),
            precision: match scene.config.precision {
                Precision::F32 => ScenePrecision::F32,
                Precision::QuadFloat => ScenePrecision::QuadFloat,
            },
            seed: scene.config.seed,
            camera: SceneCamera {
                position: serialize_coordinate(
                    scene.config.camera.position,
                    scene.config.precision,
                ),
                target: serialize_coordinate(scene.config.camera.target, scene.config.precision),
                up: scene.config.camera.up,
                vertical_fov_degrees: scene.config.camera.vertical_fov_degrees,
            },
            fractal,
            light: SceneLight {
                direction: scene.config.light.direction,
            },
            render: SceneRender {
                width: scene.config.render.width,
                height: scene.config.render.height,
                max_steps: scene.config.render.max_steps,
                max_distance: scene.config.render.max_distance,
                epsilon: scene.config.render.epsilon,
                step_safety: scene.config.render.step_safety,
                pixel_epsilon_multiplier: scene.config.render.pixel_epsilon_multiplier,
            },
            animation: scene.animation.as_ref().map(SceneAnimation::from),
        }
    }
}

impl TryFrom<SceneAnimation> for AnimationConfig {
    type Error = anyhow::Error;

    fn try_from(animation: SceneAnimation) -> Result<Self> {
        let path = match animation.path {
            SceneAnimationPath::ExponentialDive(path) => {
                AnimationPath::ExponentialDive(ExponentialDivePath {
                    overview_distance: parse_scalar(path.overview_distance)
                        .context("invalid overview_distance")?,
                    minimum_distance: parse_scalar(path.minimum_distance)
                        .context("invalid minimum_distance")?,
                    overview_duration: path.overview_duration,
                    dive_duration: path.dive_duration,
                })
            }
        };
        Ok(Self {
            fps: animation.fps,
            frame_count: animation.frame_count,
            path,
        })
    }
}

impl From<&AnimationConfig> for SceneAnimation {
    fn from(animation: &AnimationConfig) -> Self {
        let path = match &animation.path {
            AnimationPath::ExponentialDive(path) => {
                SceneAnimationPath::ExponentialDive(SceneExponentialDive {
                    overview_distance: SceneScalar::Expansion(path.overview_distance.limbs()),
                    minimum_distance: SceneScalar::Expansion(path.minimum_distance.limbs()),
                    overview_duration: path.overview_duration,
                    dive_duration: path.dive_duration,
                })
            }
        };
        Self {
            fps: animation.fps,
            frame_count: animation.frame_count,
            path,
        }
    }
}

fn parse_coordinate(values: [SceneScalar; 3], precision: Precision) -> Result<QfVec3> {
    let [x, y, z] = values.map(parse_scalar);
    let coordinate = QfVec3::new(x?, y?, z?);
    if precision == Precision::F32 {
        Ok(QfVec3::from_f32(coordinate.to_f32()))
    } else {
        Ok(coordinate)
    }
}

fn parse_scalar(value: SceneScalar) -> Result<Qf32> {
    match value {
        SceneScalar::Number(value) => Ok(Qf32::from_f64(value)),
        SceneScalar::Decimal(value) => Qf32::from_str(&value)
            .with_context(|| format!("invalid high-precision decimal '{value}'")),
        SceneScalar::Expansion(limbs) => {
            if limbs.iter().any(|limb| !limb.is_finite()) {
                bail!("quad-float expansion must contain only finite limbs");
            }
            Ok(Qf32::from_limbs(limbs))
        }
    }
}

fn serialize_coordinate(value: QfVec3, precision: Precision) -> [SceneScalar; 3] {
    value.components().map(|component| match precision {
        Precision::F32 => SceneScalar::Number(f64::from(component.to_f32())),
        Precision::QuadFloat => SceneScalar::Expansion(component.limbs()),
    })
}

fn validate_scene_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("scene name must contain 1..=64 characters");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(anyhow!(
            "scene name may contain only ASCII letters, digits, '-' and '_'"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FractalKind;

    const MANDELBULB: &str = include_str!("../../scenes/examples/mandelbulb.yaml");
    const MANDELBOX: &str = include_str!("../../scenes/examples/mandelbox.yaml");
    const MANDELBOX_QUAD_DEEP: &str =
        include_str!("../../scenes/examples/mandelbox-quad-deep.yaml");
    const MANDELBOX_QUAD_ZOOM: &str =
        include_str!("../../scenes/examples/mandelbox-quad-zoom.yaml");

    #[test]
    fn parses_both_example_scenes() {
        let bulb = parse_scene(MANDELBULB).expect("Mandelbulb example must parse");
        assert_eq!(bulb.name, "mandelbulb");
        assert_eq!(bulb.config.fractal.kind(), FractalKind::Mandelbulb);

        let box_scene = parse_scene(MANDELBOX).expect("Mandelbox example must parse");
        assert_eq!(box_scene.name, "mandelbox");
        assert_eq!(box_scene.config.fractal.kind(), FractalKind::Mandelbox);
    }

    #[test]
    fn scene_round_trip_preserves_render_config() {
        let original = parse_scene(MANDELBOX).expect("example must parse");
        let yaml = original.to_yaml().expect("scene must serialize");
        let reparsed = parse_scene(&yaml).expect("serialized scene must parse");
        assert_eq!(reparsed.name, original.name);
        assert_eq!(
            reparsed.config.camera.position,
            original.config.camera.position
        );
        assert_eq!(reparsed.config.camera.target, original.config.camera.target);
        assert_eq!(reparsed.config.render.width, original.config.render.width);
        assert_eq!(
            reparsed.config.fractal.kind(),
            original.config.fractal.kind()
        );
    }

    #[test]
    fn rejects_unknown_version_and_fields() {
        let wrong_version = MANDELBULB.replacen("version: 1", "version: 99", 1);
        let error = parse_scene(&wrong_version).expect_err("unknown version must fail");
        assert!(error.to_string().contains("unsupported scene version"));

        let unknown_field = format!("{MANDELBULB}\nunknown: true\n");
        let error = parse_scene(&unknown_field).expect_err("unknown field must fail");
        assert!(error.to_string().contains("scene schema"));
    }

    #[test]
    fn rejects_quad_float_for_unsupported_fractals() {
        let yaml = MANDELBULB.replacen("precision: f32", "precision: quad-float", 1);
        let error = parse_scene(&yaml).expect_err("Mandelbulb has no quad-float shader");
        assert!(error.to_string().contains("scene configuration is invalid"));
    }

    #[test]
    fn parses_exact_quad_float_coordinates_for_mandelbox() {
        let yaml = MANDELBOX
            .replacen("precision: f32", "precision: quad-float", 1)
            .replacen(
                "position: [11.417633, -1.764267, 5.605854]",
                "position: [\"11.4176330000000000000000000001\", -1.764267, 5.605854]",
                1,
            );
        let scene = parse_scene(&yaml).expect("quad-float Mandelbox must parse");
        assert_eq!(scene.config.precision, Precision::QuadFloat);
        let residual = scene.config.camera.position.x
            - Qf32::from_str("11.417633").expect("baseline must parse");
        assert!(residual > Qf32::ZERO);
    }

    #[test]
    fn deep_scene_requires_quad_float_to_preserve_its_camera() {
        let scene = parse_scene(MANDELBOX_QUAD_DEEP).expect("deep scene must parse");
        assert_ne!(scene.config.camera.position, scene.config.camera.target);
        assert_eq!(
            scene.config.camera.position.to_f32(),
            scene.config.camera.target.to_f32()
        );
        let serialized = scene.to_yaml().expect("quad scene must serialize");
        let reparsed = parse_scene(&serialized).expect("quad scene must round trip");
        assert_eq!(reparsed.config.precision, Precision::QuadFloat);
        assert_eq!(
            reparsed.config.camera.position,
            scene.config.camera.position
        );
        assert_eq!(reparsed.config.camera.target, scene.config.camera.target);

        let f32_yaml = MANDELBOX_QUAD_DEEP.replacen("quad-float", "f32", 1);
        let error = parse_scene(&f32_yaml).expect_err("f32 must collapse the deep camera");
        assert!(error.to_string().contains("scene configuration is invalid"));
    }

    #[test]
    fn parses_and_round_trips_quad_float_animation() {
        let scene = parse_scene(MANDELBOX_QUAD_ZOOM).expect("zoom scene must parse");
        let animation = scene.animation.as_ref().expect("animation must be present");
        assert_eq!(animation.fps, 60);
        assert_eq!(animation.frame_count, 1_621);
        let FractalConfig::Mandelbox(fractal) = &scene.config.fractal else {
            unreachable!();
        };
        assert_eq!(
            scene.config.camera.target.x,
            Qf32::from_f32(2.0 * fractal.fold_limit)
        );
        let final_frame = animation
            .sample(&scene.config, animation.frame_count - 1)
            .expect("final animation frame must be representable");
        assert_eq!(final_frame.time_seconds, 27.0);
        assert_eq!(
            final_frame.camera_distance,
            Qf32::from_str("1e-26").unwrap()
        );

        let yaml = scene.to_yaml().expect("animation scene must serialize");
        let reparsed = parse_scene(&yaml).expect("serialized animation must parse");
        let reparsed_animation = reparsed
            .animation
            .expect("animation must survive round trip");
        let reparsed_final = reparsed_animation
            .sample(&reparsed.config, reparsed_animation.frame_count - 1)
            .unwrap();
        assert_eq!(reparsed_final.camera_distance, final_frame.camera_distance);
    }

    #[test]
    fn rejects_animation_beyond_the_measured_quad_depth() {
        let yaml = MANDELBOX_QUAD_ZOOM.replacen("1e-26", "1e-27", 1);
        let error = parse_scene(&yaml).expect_err("unverified depth must fail");
        assert!(
            error
                .to_string()
                .contains("animation configuration is invalid")
        );
    }

    #[test]
    fn reports_domain_validation_errors() {
        let yaml = MANDELBULB.replacen("width: 640", "width: 0", 1);
        let error = parse_scene(&yaml).expect_err("zero width must fail");
        assert!(error.to_string().contains("scene configuration is invalid"));
    }
}
