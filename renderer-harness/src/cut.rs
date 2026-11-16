//! Compiles one cut of a shot list into a renderable scene. A director names
//! a scouted viewpoint and a motion in relative terms; absolute camera
//! coordinates and path parameters are derived here.

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{
    AnimationConfig, AnimationPath, ExponentialDivePath, FractalConfig, LoadedScene,
    MultiTargetDivePath, Qf32, QfVec3, RenderConfig, SurfaceFlyoverPath, TargetOrbitPath,
    TargetSearchConfig,
};
use serde::{Deserialize, Serialize};

use crate::{
    axes::{Motion, ScaleRegime},
    classify,
    shape::{self, Survey},
    tuning::Tuning,
};

/// Most frames a route check evaluates; longer cuts are sampled evenly.
const MAX_ROUTE_SAMPLES: u32 = 400;

// Searches used by the automatic paths, as in the renderer's own examples.
const SEARCH_ATTEMPTS: u32 = 192;
const SEARCH_MAX_STEPS: u32 = 1000;
const SEARCH_AIM_JITTER: f64 = 0.28;

/// Output raster and frame rate of a film.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Format {
    /// Formats YouTube accepts without letterboxing or frame-rate conversion.
    pub fn validate(&self) -> Result<()> {
        if !matches!(
            (self.width, self.height),
            (1280, 720) | (1920, 1080) | (2560, 1440) | (3840, 2160)
        ) {
            bail!(
                "format {}x{} is not one of 1280x720, 1920x1080, 2560x1440, 3840x2160",
                self.width,
                self.height
            );
        }
        if !matches!(self.fps, 24 | 25 | 30 | 48 | 50 | 60) {
            bail!(
                "format fps {} is not one of 24, 25, 30, 48, 50, 60",
                self.fps
            );
        }
        Ok(())
    }
}

/// Camera motion of a cut, in terms relative to its viewpoint and shape.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MotionSpec {
    /// Circle the viewpoint's subject, starting from the viewpoint.
    Orbit {
        /// Signed fraction of a full turn.
        revolutions: f64,
        /// Camera distance from the subject, as a multiple of the
        /// viewpoint's own distance. Below 1 frames the subject tighter.
        #[serde(default = "unit_distance")]
        distance: f64,
    },
    /// Approach the viewpoint's subject along the line of sight.
    Dive {
        /// Final camera distance as a fraction of the starting distance.
        depth: f64,
        /// Seconds to hold the starting frame before moving.
        #[serde(default)]
        hold_seconds: f64,
        /// Starting distance from the subject, as a multiple of the
        /// viewpoint's own distance.
        #[serde(default = "unit_distance")]
        distance: f64,
    },
    /// Retreat from the viewpoint's subject along the line of sight,
    /// revealing what surrounds it.
    PullBack {
        /// Final camera distance as a multiple of the starting distance.
        factor: f64,
        /// Seconds to hold the starting frame before moving.
        #[serde(default)]
        hold_seconds: f64,
        /// Starting distance from the subject, as a multiple of the
        /// viewpoint's own distance.
        #[serde(default = "unit_distance")]
        distance: f64,
    },
    /// Glide over a surface the harness finds on the shape. The viewpoint
    /// only supplies the light; bounded shapes only.
    Flyover {
        /// Distance travelled, relative to the shape's radius.
        travel: f64,
        /// Height above the surface, relative to the shape's radius.
        height: f64,
        /// 0 looks straight down; larger values look further ahead.
        #[serde(default)]
        look_ahead: f64,
    },
    /// Dive repeatedly towards features the harness finds on the shape,
    /// with a fade between dives. Bounded shapes only.
    MultiDive {
        /// Final camera distance as a fraction of the starting distance.
        depth: f64,
        dives: u32,
    },
}

const fn unit_distance() -> f64 {
    1.0
}

impl MotionSpec {
    #[must_use]
    pub const fn motion(&self) -> Motion {
        match self {
            Self::Orbit { .. } => Motion::Orbit,
            Self::Dive { .. } => Motion::Dive,
            Self::PullBack { .. } => Motion::PullBack,
            Self::Flyover { .. } => Motion::Flyover,
            Self::MultiDive { .. } => Motion::MultiDive,
        }
    }

    /// A representative specification of each motion, used to test whether a
    /// shape supports that motion at all.
    #[must_use]
    pub const fn typical(motion: Motion) -> Self {
        match motion {
            Motion::Orbit => Self::Orbit {
                revolutions: 0.25,
                distance: 1.0,
            },
            Motion::Dive => Self::Dive {
                depth: 0.3,
                hold_seconds: 0.0,
                distance: 1.0,
            },
            Motion::PullBack => Self::PullBack {
                factor: 3.0,
                hold_seconds: 0.0,
                distance: 1.0,
            },
            Motion::Flyover => Self::Flyover {
                travel: 0.6,
                height: 0.3,
                look_ahead: 0.3,
            },
            Motion::MultiDive => Self::MultiDive {
                depth: 0.05,
                dives: 2,
            },
        }
    }

    pub(crate) fn validate(&self, duration_seconds: f64, max_cut_seconds: f64) -> Result<()> {
        let within = |name: &str, value: f64, low: f64, high: f64| {
            if value.is_finite() && (low..=high).contains(&value) {
                Ok(())
            } else {
                bail!("{name} {value} must be in {low}..={high}")
            }
        };
        match *self {
            Self::Orbit {
                revolutions,
                distance,
            } => {
                within("orbit revolutions", revolutions.abs(), 0.01, 1.0)?;
                within("orbit distance", distance, 0.2, 4.0)
            }
            Self::Dive {
                depth,
                hold_seconds,
                distance,
            } => {
                within("dive depth", depth, 0.001, 0.9)?;
                within(
                    "dive hold_seconds",
                    hold_seconds,
                    0.0,
                    duration_seconds * 0.5,
                )?;
                within("dive distance", distance, 0.2, 4.0)
            }
            Self::PullBack {
                factor,
                hold_seconds,
                distance,
            } => {
                within("pull-back factor", factor, 1.1, 20.0)?;
                within(
                    "pull-back hold_seconds",
                    hold_seconds,
                    0.0,
                    duration_seconds * 0.5,
                )?;
                within("pull-back distance", distance, 0.2, 4.0)
            }
            Self::Flyover {
                travel,
                height,
                look_ahead,
            } => {
                within("flyover travel", travel, 0.05, 2.0)?;
                within("flyover height", height, 0.02, 1.0)?;
                within("flyover look_ahead", look_ahead, 0.0, 2.0)
            }
            Self::MultiDive { depth, dives } => {
                within("multi-dive depth", depth, 0.0001, 0.5)?;
                within("multi-dive dives", f64::from(dives), 1.0, 4.0)?;
                // Each dive needs room for an overview, the dive, and a fade.
                within(
                    "multi-dive seconds per dive",
                    duration_seconds / f64::from(dives),
                    3.0,
                    max_cut_seconds,
                )
            }
        }
    }
}

/// Builds the scene of one cut.
///
/// `viewpoint` is the scouted viewpoint's scene: its fractal, camera, light,
/// and march settings. The result has the film's raster and an animation.
pub fn compile(
    viewpoint: &LoadedScene,
    survey: &Survey,
    motion: &MotionSpec,
    duration_seconds: f64,
    format: &Format,
    name: &str,
    tuning: &Tuning,
) -> Result<LoadedScene> {
    format.validate()?;
    let rules = &tuning.shotlist;
    if !duration_seconds.is_finite()
        || !(rules.min_cut_seconds..=rules.max_cut_seconds).contains(&duration_seconds)
    {
        bail!(
            "cut duration {duration_seconds} must be in {}..={} seconds",
            rules.min_cut_seconds,
            rules.max_cut_seconds
        );
    }
    motion.validate(duration_seconds, rules.max_cut_seconds)?;
    let subject_fog = tuning.scouting.subject_fog;

    let mut config = viewpoint.config.clone();
    config.render.width = format.width;
    config.render.height = format.height;

    let target = config.camera.target.to_f64();
    // Orbits and dives may start nearer to or further from the subject than
    // the scouted viewpoint, along the same line of sight.
    let framing = match *motion {
        MotionSpec::Orbit { distance, .. }
        | MotionSpec::Dive { distance, .. }
        | MotionSpec::PullBack { distance, .. } => distance,
        MotionSpec::Flyover { .. } | MotionSpec::MultiDive { .. } => 1.0,
    };
    if framing != 1.0 {
        let scouted = config.camera.position.to_f64();
        let moved = [0, 1, 2].map(|axis| target[axis] + (scouted[axis] - target[axis]) * framing);
        config.camera.position = QfVec3::from_f64(moved);
        let moved_distance = distance(moved, target);
        config.camera.focus_distance = moved_distance as f32;
        reframe(&mut config, 0.0, moved_distance, subject_fog);
    }
    let position = config.camera.position.to_f64();
    let subject_distance = distance(position, target);
    let path = match *motion {
        MotionSpec::Orbit { revolutions, .. } => AnimationPath::TargetOrbit(TargetOrbitPath {
            radius: Qf32::from_f64(subject_distance),
            duration: duration_seconds,
            revolutions,
            // The viewpoint's up is perpendicular to its line of sight, so
            // the camera starts on this great circle and pans sideways.
            axis: config.camera.up.map(f64::from),
            cone_angle_degrees: 90.0,
            start_angle_degrees: 0.0,
        }),
        MotionSpec::Dive {
            depth,
            hold_seconds,
            ..
        } => AnimationPath::ExponentialDive(ExponentialDivePath {
            overview_distance: Qf32::from_f64(subject_distance),
            minimum_distance: Qf32::from_f64(subject_distance * depth),
            overview_duration: hold_seconds,
            dive_duration: duration_seconds - hold_seconds,
        }),
        MotionSpec::PullBack {
            factor,
            hold_seconds,
            ..
        } => {
            // Retreating multiplies the distance; the march must reach the
            // subject from the far end, and fog is set for the middle.
            let far = subject_distance * factor;
            reframe(
                &mut config,
                0.0,
                (subject_distance * far).sqrt(),
                subject_fog,
            );
            config.render.max_distance = config.render.max_distance.max((far * 12.0) as f32);
            AnimationPath::ExponentialDive(ExponentialDivePath {
                overview_distance: Qf32::from_f64(subject_distance),
                minimum_distance: Qf32::from_f64(far),
                overview_duration: hold_seconds,
                dive_duration: duration_seconds - hold_seconds,
            })
        }
        MotionSpec::Flyover {
            travel,
            height,
            look_ahead,
        } => {
            let radius = bounded_radius(survey, "flyover")?;
            let camera_height = height * radius;
            reframe(&mut config, radius, camera_height, subject_fog);
            AnimationPath::SurfaceFlyover(SurfaceFlyoverPath::new(
                camera_height,
                travel * radius,
                duration_seconds,
                look_ahead,
                // The path projects this onto the surface it finds.
                [0.2, 1.0, 0.35],
                1.5e-4 * radius,
                search(radius),
            ))
        }
        MotionSpec::MultiDive { depth, dives } => {
            let radius = bounded_radius(survey, "multi-dive")?;
            let overview_distance = radius * 1.7;
            reframe(&mut config, radius, overview_distance, subject_fog);
            let cycle = duration_seconds / f64::from(dives);
            let transition = (cycle * 0.15).min(1.0);
            let overview = cycle * 0.2;
            AnimationPath::MultiTargetDive(MultiTargetDivePath::new(
                Qf32::from_f64(overview_distance),
                Qf32::from_f64(overview_distance * depth),
                overview,
                cycle - overview - transition,
                transition,
                search(radius),
            ))
        }
    };

    let mut animation = AnimationConfig {
        fps: format.fps,
        // The last frame lands on the final instant of the motion.
        frame_count: (duration_seconds * f64::from(format.fps)).round() as u32 + 1,
        path,
    };
    animation.plan(&config).with_context(|| {
        format!(
            "the shape does not support this {} path",
            motion.motion().as_str()
        )
    })?;
    animation
        .validate(&config)
        .context("the compiled path is not renderable")?;
    config
        .validate()
        .context("the compiled cut is not renderable")?;

    Ok(LoadedScene {
        name: name.to_owned(),
        config,
        animation: Some(animation),
        video: None,
    })
}

fn bounded_radius(survey: &Survey, motion: &str) -> Result<f64> {
    if !survey.bounded {
        bail!("{motion} needs a bounded shape; this location fills space");
    }
    Ok(survey.radius)
}

fn search(radius: f64) -> TargetSearchConfig {
    TargetSearchConfig {
        bound_radius: radius,
        hit_epsilon: 2.0e-7 * radius,
        max_steps: SEARCH_MAX_STEPS,
        attempts: SEARCH_ATTEMPTS,
        aim_jitter: SEARCH_AIM_JITTER,
    }
}

/// Adapts march range and scouting fog to a camera the path, not the
/// viewpoint, will place.
fn reframe(config: &mut RenderConfig, radius: f64, subject_distance: f64, subject_fog: f64) {
    config.render.max_distance = config
        .render
        .max_distance
        .max((radius * 8.0).max(subject_distance * 12.0) as f32);
    if let FractalConfig::Dsl(dsl) = &mut config.fractal {
        dsl.material.fog_density =
            shape::subject_fog_density(subject_distance.max(radius * 0.5), subject_fog) as f32;
    }
}

/// What a route check found along a cut.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Route {
    pub frames_checked: u32,
    /// Smallest ratio of clearance to subject distance, and where it occurs.
    pub min_clearance_ratio: f64,
    pub worst_frame: u32,
    /// Scale regime at the middle of the cut.
    pub scale_regime: ScaleRegime,
    pub safe: bool,
}

/// Walks a compiled cut on the CPU and reports how close the camera comes to
/// the surface relative to its subject distance.
pub fn check_route(scene: &LoadedScene, survey: &Survey, tuning: &Tuning) -> Result<Route> {
    let animation = scene
        .animation
        .as_ref()
        .context("a cut must have an animation")?;
    let last = animation.frame_count - 1;
    let step = (last / MAX_ROUTE_SAMPLES).max(1);
    let mut frames: Vec<u32> = (0..=last).step_by(step as usize).collect();
    if frames.last() != Some(&last) {
        frames.push(last);
    }

    // Clearance, subject distance, and camera position of a frame.
    let camera_at = |frame: u32| -> Result<(f64, f64, [f64; 3])> {
        let sample = animation.sample(&scene.config, frame)?;
        let position = sample.config.camera.position.to_f64();
        let target = sample.config.camera.target.to_f64();
        Ok((
            shape::clearance(&scene.config.fractal, position),
            distance(position, target),
            position,
        ))
    };

    let (mut min_ratio, mut worst_frame) = (f64::INFINITY, 0);
    for frame in &frames {
        let (clearance, subject_distance, _) = camera_at(*frame)?;
        let ratio = if clearance.is_finite() && subject_distance > 0.0 {
            clearance / subject_distance
        } else {
            // A camera inside the surface or a broken estimate is unsafe.
            f64::NEG_INFINITY
        };
        if ratio < min_ratio {
            (min_ratio, worst_frame) = (ratio, *frame);
        }
    }
    let (middle_clearance, _, middle_position) = camera_at(last / 2)?;
    Ok(Route {
        frames_checked: frames.len() as u32,
        min_clearance_ratio: min_ratio,
        worst_frame,
        scale_regime: classify::scale_regime(
            middle_clearance,
            middle_position,
            survey,
            &tuning.classification,
        ),
        safe: min_ratio >= tuning.shotlist.min_clearance_ratio,
    })
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Compiles with the default tuning.
    fn compile(
        viewpoint: &LoadedScene,
        survey: &Survey,
        motion: &MotionSpec,
        duration_seconds: f64,
        format: &Format,
        name: &str,
    ) -> Result<LoadedScene> {
        super::compile(
            viewpoint,
            survey,
            motion,
            duration_seconds,
            format,
            name,
            &Tuning::default(),
        )
    }

    fn check_route(scene: &LoadedScene, survey: &Survey) -> Result<Route> {
        super::check_route(scene, survey, &Tuning::default())
    }
    use crate::{axes::FormulaFamily, rng::Rng, shape::Shape};
    use fractal_renderer_core::MandelbulbConfig;

    pub(crate) const HD30: Format = Format {
        width: 1920,
        height: 1080,
        fps: 30,
    };

    /// A scouted viewpoint scene of the given regime on a generated shape.
    pub(crate) fn viewpoint_scene(
        family: FormulaFamily,
        regime: ScaleRegime,
        seed: u64,
    ) -> (LoadedScene, Survey) {
        let mut rng = Rng::new(seed);
        loop {
            let Some(shape) = shape::discover(&mut rng, family) else {
                continue;
            };
            let Some(viewpoint) = shape::viewpoints(&mut rng, &shape, &Tuning::default().scouting)
                .into_iter()
                .find(|viewpoint| viewpoint.scale_regime == regime)
            else {
                continue;
            };
            let scene = LoadedScene {
                name: "viewpoint".to_owned(),
                config: shape::thumbnail_config(
                    &shape,
                    &viewpoint,
                    320,
                    180,
                    1,
                    &Tuning::default().scouting,
                ),
                animation: None,
                video: None,
            };
            return (scene, shape.survey);
        }
    }

    fn mandelbulb() -> (LoadedScene, Survey) {
        let shape = Shape {
            family: FormulaFamily::Mandelbulb,
            fractal: FractalConfig::Mandelbulb(MandelbulbConfig::default()),
            survey: Survey {
                bounded: true,
                radius: 1.3,
            },
        };
        let viewpoint = shape::viewpoints(&mut Rng::new(2), &shape, &Tuning::default().scouting)
            .into_iter()
            .find(|viewpoint| viewpoint.scale_regime == ScaleRegime::Overview)
            .unwrap();
        let scene = LoadedScene {
            name: "viewpoint".to_owned(),
            config: shape::thumbnail_config(
                &shape,
                &viewpoint,
                320,
                180,
                1,
                &Tuning::default().scouting,
            ),
            animation: None,
            video: None,
        };
        (scene, shape.survey)
    }

    fn camera(scene: &LoadedScene, frame: u32) -> ([f64; 3], [f64; 3]) {
        let sample = scene
            .animation
            .as_ref()
            .unwrap()
            .sample(&scene.config, frame)
            .unwrap();
        (
            sample.config.camera.position.to_f64(),
            sample.config.camera.target.to_f64(),
        )
    }

    #[test]
    fn formats_are_limited_to_youtube_rasters() {
        HD30.validate().unwrap();
        for invalid in [
            Format {
                width: 1921,
                ..HD30
            },
            Format {
                height: 1000,
                ..HD30
            },
            Format { fps: 29, ..HD30 },
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn orbit_starts_at_the_viewpoint_and_keeps_its_distance() {
        let (viewpoint, survey) = viewpoint_scene(FormulaFamily::JuliaBox, ScaleRegime::Mid, 3);
        let cut = compile(
            &viewpoint,
            &survey,
            &MotionSpec::Orbit {
                revolutions: 0.25,
                distance: 1.0,
            },
            6.0,
            &HD30,
            "cut-01",
        )
        .unwrap();

        let animation = cut.animation.as_ref().unwrap();
        assert_eq!((animation.fps, animation.frame_count), (30, 181));
        assert_eq!(
            (cut.config.render.width, cut.config.render.height),
            (1920, 1080)
        );

        let start = viewpoint.config.camera.position.to_f64();
        let target = viewpoint.config.camera.target.to_f64();
        let radius = distance(start, target);
        let (first, first_target) = camera(&cut, 0);
        assert!(
            distance(first, start) < radius * 1.0e-5,
            "{first:?} vs {start:?}"
        );
        assert!(distance(first_target, target) < radius * 1.0e-5);
        for frame in [45, 90, 180] {
            let (position, _) = camera(&cut, frame);
            assert!((distance(position, target) - radius).abs() < radius * 1.0e-5);
        }
        // A quarter turn ends at right angles to the start.
        let (end, _) = camera(&cut, 180);
        let cosine: f64 = (0..3)
            .map(|axis| (end[axis] - target[axis]) * (start[axis] - target[axis]))
            .sum::<f64>()
            / (radius * radius);
        assert!(cosine.abs() < 1.0e-4, "{cosine}");
    }

    #[test]
    fn framing_distance_moves_the_start_along_the_line_of_sight() {
        let (viewpoint, survey) = viewpoint_scene(FormulaFamily::JuliaBox, ScaleRegime::Mid, 3);
        let target = viewpoint.config.camera.target.to_f64();
        let scouted = viewpoint.config.camera.position.to_f64();
        let scouted_distance = distance(scouted, target);

        for factor in [0.5, 2.0] {
            let cut = compile(
                &viewpoint,
                &survey,
                &MotionSpec::Orbit {
                    revolutions: 0.25,
                    distance: factor,
                },
                6.0,
                &HD30,
                "cut",
            )
            .unwrap();
            let (start, _) = camera(&cut, 0);
            assert!(
                (distance(start, target) - scouted_distance * factor).abs()
                    < scouted_distance * 1.0e-5
            );
            // Still on the scouted line of sight.
            let along: f64 = (0..3)
                .map(|axis| (start[axis] - target[axis]) * (scouted[axis] - target[axis]))
                .sum::<f64>()
                / (scouted_distance * scouted_distance * factor);
            assert!((along - 1.0).abs() < 1.0e-5, "{along}");
            let (end, _) = camera(&cut, 180);
            assert!(
                (distance(end, target) - scouted_distance * factor).abs()
                    < scouted_distance * 1.0e-5
            );
        }

        // A dive measures its depth from the adjusted start.
        let dive = compile(
            &viewpoint,
            &survey,
            &MotionSpec::Dive {
                depth: 0.5,
                hold_seconds: 0.0,
                distance: 2.0,
            },
            4.0,
            &HD30,
            "cut",
        )
        .unwrap();
        let (end, _) = camera(&dive, 120);
        assert!((distance(end, target) - scouted_distance).abs() < scouted_distance * 1.0e-4);

        for out_of_range in [0.1, 5.0] {
            assert!(
                compile(
                    &viewpoint,
                    &survey,
                    &MotionSpec::Orbit {
                        revolutions: 0.25,
                        distance: out_of_range,
                    },
                    6.0,
                    &HD30,
                    "cut",
                )
                .is_err()
            );
        }
    }

    #[test]
    fn cut_limits_and_route_safety_follow_the_tuning() {
        let (viewpoint, survey) = mandelbulb();
        let orbit = MotionSpec::Orbit {
            revolutions: 0.5,
            distance: 1.0,
        };
        let mut tuning = Tuning::default();
        tuning.shotlist.max_cut_seconds = 4.0;
        assert!(super::compile(&viewpoint, &survey, &orbit, 6.0, &HD30, "cut", &tuning).is_err());
        assert!(super::compile(&viewpoint, &survey, &orbit, 3.0, &HD30, "cut", &tuning).is_ok());

        let cut = compile(&viewpoint, &survey, &orbit, 6.0, &HD30, "cut").unwrap();
        assert!(check_route(&cut, &survey).unwrap().safe);
        // A stricter clearance rule turns the same route unsafe.
        tuning.shotlist.min_clearance_ratio = 0.9;
        assert!(!super::check_route(&cut, &survey, &tuning).unwrap().safe);
        // Regime boundaries move with the classification rules.
        tuning.classification.overview_radii = 5.0;
        assert_eq!(
            super::check_route(&cut, &survey, &tuning)
                .unwrap()
                .scale_regime,
            ScaleRegime::Mid
        );
    }

    #[test]
    fn framing_distance_defaults_to_the_scouted_viewpoint() {
        let orbit: MotionSpec =
            serde_json::from_str(r#"{"kind":"orbit","revolutions":0.2}"#).unwrap();
        assert_eq!(
            orbit,
            MotionSpec::Orbit {
                revolutions: 0.2,
                distance: 1.0
            }
        );
        let dive: MotionSpec =
            serde_json::from_str(r#"{"kind":"dive","depth":0.4,"distance":0.6}"#).unwrap();
        assert_eq!(
            dive,
            MotionSpec::Dive {
                depth: 0.4,
                hold_seconds: 0.0,
                distance: 0.6
            }
        );
    }

    #[test]
    fn dive_holds_then_closes_to_the_requested_depth() {
        let (viewpoint, survey) = viewpoint_scene(FormulaFamily::Mandelbox, ScaleRegime::Mid, 4);
        let cut = compile(
            &viewpoint,
            &survey,
            &MotionSpec::Dive {
                depth: 0.25,
                hold_seconds: 1.0,
                distance: 1.0,
            },
            5.0,
            &HD30,
            "cut-02",
        )
        .unwrap();
        let target = viewpoint.config.camera.target.to_f64();
        let start = distance(viewpoint.config.camera.position.to_f64(), target);
        let distance_at = |frame| distance(camera(&cut, frame).0, target);
        assert!((distance_at(0) - start).abs() < start * 1.0e-5);
        assert!((distance_at(30) - start).abs() < start * 1.0e-5);
        assert!(distance_at(90) < start * 0.9);
        assert!((distance_at(150) - start * 0.25).abs() < start * 1.0e-4);
    }

    #[test]
    fn pull_back_retreats_along_the_line_of_sight() {
        let (viewpoint, survey) = viewpoint_scene(FormulaFamily::Mandelbox, ScaleRegime::Deep, 4);
        let cut = compile(
            &viewpoint,
            &survey,
            &MotionSpec::PullBack {
                factor: 4.0,
                hold_seconds: 1.0,
                distance: 1.0,
            },
            5.0,
            &HD30,
            "cut",
        )
        .unwrap();
        let target = viewpoint.config.camera.target.to_f64();
        let start = distance(viewpoint.config.camera.position.to_f64(), target);
        let distance_at = |frame| distance(camera(&cut, frame).0, target);
        assert!((distance_at(0) - start).abs() < start * 1.0e-5);
        assert!((distance_at(30) - start).abs() < start * 1.0e-5);
        assert!(distance_at(90) > start * 1.2);
        assert!((distance_at(150) - start * 4.0).abs() < start * 1.0e-4);
        // The march reaches the subject from the far end.
        assert!(f64::from(cut.config.render.max_distance) > start * 4.0);
        assert_eq!(
            MotionSpec::typical(Motion::PullBack).motion(),
            Motion::PullBack
        );
        for factor in [1.0, 25.0] {
            assert!(
                compile(
                    &viewpoint,
                    &survey,
                    &MotionSpec::PullBack {
                        factor,
                        hold_seconds: 0.0,
                        distance: 1.0
                    },
                    5.0,
                    &HD30,
                    "cut",
                )
                .is_err()
            );
        }
    }

    #[test]
    fn out_of_range_requests_are_rejected() {
        let (viewpoint, survey) = mandelbulb();
        let attempt = |motion: MotionSpec, duration: f64| {
            compile(&viewpoint, &survey, &motion, duration, &HD30, "cut").is_err()
        };
        assert!(attempt(
            MotionSpec::Orbit {
                revolutions: 0.0,
                distance: 1.0,
            },
            5.0
        ));
        assert!(attempt(
            MotionSpec::Orbit {
                revolutions: 3.0,
                distance: 1.0,
            },
            5.0
        ));
        assert!(attempt(
            MotionSpec::Orbit {
                revolutions: 0.2,
                distance: 1.0,
            },
            0.2
        ));
        assert!(attempt(
            MotionSpec::Orbit {
                revolutions: 0.2,
                distance: 1.0,
            },
            600.0
        ));
        assert!(attempt(
            MotionSpec::Dive {
                depth: 1.5,
                hold_seconds: 0.0,
                distance: 1.0,
            },
            5.0
        ));
        assert!(attempt(
            MotionSpec::Dive {
                depth: 0.5,
                hold_seconds: 4.0,
                distance: 1.0,
            },
            5.0
        ));
        assert!(attempt(
            MotionSpec::MultiDive {
                depth: 0.05,
                dives: 4
            },
            6.0
        ));
    }

    #[test]
    fn automatic_paths_need_a_bounded_shape() {
        let (viewpoint, _) = mandelbulb();
        let unbounded = Survey {
            bounded: false,
            radius: 3.0,
        };
        for motion in [Motion::Flyover, Motion::MultiDive] {
            let error = compile(
                &viewpoint,
                &unbounded,
                &MotionSpec::typical(motion),
                12.0,
                &HD30,
                "cut",
            )
            .unwrap_err();
            assert!(error.to_string().contains("bounded shape"), "{error:#}");
        }
    }

    #[test]
    fn automatic_paths_plan_on_a_bounded_shape() {
        let (viewpoint, survey) = mandelbulb();
        for motion in [Motion::Flyover, Motion::MultiDive] {
            let spec = MotionSpec::typical(motion);
            assert_eq!(spec.motion(), motion);
            let cut = compile(&viewpoint, &survey, &spec, 12.0, &HD30, "cut")
                .unwrap_or_else(|error| panic!("{motion:?}: {error:#}"));
            // The planned camera must be sampled without error at both ends.
            camera(&cut, 0);
            camera(&cut, 360);
        }
    }

    #[test]
    fn route_check_reports_clearance_and_regime() {
        let (viewpoint, survey) = mandelbulb();
        let orbit = compile(
            &viewpoint,
            &survey,
            &MotionSpec::Orbit {
                revolutions: 0.5,
                distance: 1.0,
            },
            6.0,
            &HD30,
            "cut",
        )
        .unwrap();
        let route = check_route(&orbit, &survey).unwrap();
        assert!(route.safe, "{route:?}");
        assert_eq!(route.scale_regime, ScaleRegime::Overview);
        assert_eq!(route.frames_checked, 181);
        assert!(route.min_clearance_ratio > 0.3);

        // Diving through the surface point must be caught.
        let crash = compile(
            &viewpoint,
            &survey,
            &MotionSpec::Dive {
                depth: 0.001,
                hold_seconds: 0.0,
                distance: 1.0,
            },
            6.0,
            &HD30,
            "cut",
        )
        .unwrap();
        let route = check_route(&crash, &survey).unwrap();
        assert!(!route.safe, "{route:?}");
        assert_eq!(route.worst_frame, 180);
    }

    #[test]
    fn long_cuts_are_sampled_but_always_include_the_last_frame() {
        let (viewpoint, survey) = mandelbulb();
        let cut = compile(
            &viewpoint,
            &survey,
            &MotionSpec::Orbit {
                revolutions: 1.0,
                distance: 1.0,
            },
            60.0,
            &Format { fps: 60, ..HD30 },
            "cut",
        )
        .unwrap();
        let route = check_route(&cut, &survey).unwrap();
        assert!(route.frames_checked <= MAX_ROUTE_SAMPLES + 2);
        assert!(route.frames_checked >= MAX_ROUTE_SAMPLES / 2);
    }
}
