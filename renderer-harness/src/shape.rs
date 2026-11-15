//! Shape discovery on the CPU. The harness, not an agent, proposes fractal
//! formulas and camera positions; agents only choose among rendered results.
//! Sampling the formula space directly is what keeps new films from being
//! perturbations of earlier ones.

use fractal_renderer_core::{
    DistanceEstimator, DslFractalConfig, DslMaterial, FractalConfig, MandelbulbConfig,
    OrbitTransform, QfVec3, RenderConfig, RenderSettings,
};

use crate::{
    axes::{FormulaFamily, ScaleRegime},
    rng::Rng,
};

/// Radius from which survey rays are cast towards the origin.
const SURVEY_RADIUS: f64 = 24.0;
const SURVEY_RAYS: usize = 96;
/// A shape whose surface reaches this far is treated as unbounded.
const UNBOUNDED_RADIUS: f64 = 10.0;
/// Working radius for camera placement inside an unbounded shape.
const UNBOUNDED_WORK_RADIUS: f64 = 3.0;
const MIN_BOUND_RADIUS: f64 = 0.3;
/// Share of survey rays that must hit before a shape counts as present.
const MIN_HIT_SHARE: f64 = 0.2;
const TRACE_STEPS: u32 = 160;
/// Conservative step factor: generated estimators can overestimate distance.
const TRACE_SAFETY: f64 = 0.7;

/// Attempts at finding free space with the wanted clearance.
const PLACEMENT_ATTEMPTS: usize = 600;
/// Directions compared for each camera position.
const DIRECTION_CANDIDATES: usize = 24;
/// Half-angle of the ray fan used to judge depth layering, in radians.
const FAN_ANGLE: f64 = 0.22;

const THUMBNAIL_FOV_DEGREES: f32 = 40.0;
/// Fog optical depth at the subject: about 15% of the sky colour mixed in.
const SUBJECT_FOG: f64 = 0.16;
/// Upper bound the renderer accepts for fog density.
const MAX_FOG_DENSITY: f64 = 100.0;

/// A fractal that survived the CPU checks.
#[derive(Clone, Debug)]
pub struct Shape {
    pub family: FormulaFamily,
    pub fractal: FractalConfig,
    pub survey: Survey,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Survey {
    /// False for shapes that tile space, which only have interior views.
    pub bounded: bool,
    /// Radius containing the surface, or the working radius when unbounded.
    pub radius: f64,
}

/// A camera placement found for a shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewpoint {
    pub position: [f64; 3],
    pub target: [f64; 3],
    pub up: [f64; 3],
    pub scale_regime: ScaleRegime,
    /// Distance from the camera to the nearest surface.
    pub clearance: f64,
}

struct Estimator<'a>(&'a FractalConfig);

impl Estimator<'_> {
    fn distance(&self, point: [f64; 3]) -> f64 {
        match self.0 {
            FractalConfig::Mandelbulb(config) => config.distance_estimate(point),
            FractalConfig::Mandelbox(config) => config.distance_estimate(point),
            FractalConfig::Dsl(config) => config.distance_estimate(point),
        }
    }

    /// Sphere-traces a ray and returns the distance to the first hit.
    fn trace(&self, origin: [f64; 3], direction: [f64; 3], max_distance: f64) -> Trace {
        let hit_epsilon = max_distance * 1.0e-4;
        let mut travelled = 0.0;
        for _ in 0..TRACE_STEPS {
            let point = add(origin, scale(direction, travelled));
            let distance = self.distance(point);
            if !distance.is_finite() {
                return Trace::Invalid;
            }
            if distance < hit_epsilon {
                return Trace::Hit(travelled);
            }
            // Escape-time estimators overestimate far from the set, where a
            // full step would jump over it. Bounding the step by the distance
            // to the origin keeps distant rays converging on the shape.
            travelled += (distance * TRACE_SAFETY).min(0.25 * length(point) + 0.05);
            if travelled > max_distance {
                break;
            }
        }
        Trace::Miss
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Trace {
    Hit(f64),
    Miss,
    Invalid,
}

/// Generates a random formula of the given family. The result always passes
/// the renderer's validation; whether it is worth looking at is decided by
/// [`survey`] and by the rendered thumbnails.
#[must_use]
pub fn generate(rng: &mut Rng, family: FormulaFamily) -> FractalConfig {
    let iterations = 10 + rng.below(15) as u32;
    let orbit = match family {
        FormulaFamily::Mandelbulb => {
            return FractalConfig::Mandelbulb(MandelbulbConfig {
                // Half-integer powers keep the familiar lobed symmetry.
                power: (rng.range(3.0, 12.0) * 2.0).round() as f32 / 2.0,
                iterations: 8 + rng.below(9) as u32,
                bailout: 4.0,
            });
        }
        FormulaFamily::Mandelbox => mandelbox_core(rng).to_vec(),
        FormulaFamily::RotatedMandelbox => {
            let [box_fold, sphere_fold, recurrence] = mandelbox_core(rng);
            let mut orbit = vec![
                OrbitTransform::Rotate {
                    axis: rng.unit_vector().map(round3),
                    degrees: round3(rng.range(3.0, 35.0) * sign(rng)),
                },
                box_fold,
                sphere_fold,
            ];
            if rng.chance(0.4) {
                orbit.push(OrbitTransform::Translate {
                    offset: [(); 3].map(|()| round3(rng.range(-0.3, 0.3))),
                });
            }
            orbit.push(recurrence);
            orbit
        }
        FormulaFamily::JuliaBox => {
            let [box_fold, sphere_fold, _] = mandelbox_core(rng);
            let mut orbit = Vec::new();
            if rng.chance(0.5) {
                orbit.push(OrbitTransform::Rotate {
                    axis: rng.unit_vector().map(round3),
                    degrees: round3(rng.range(3.0, 25.0) * sign(rng)),
                });
            }
            orbit.extend([
                box_fold,
                sphere_fold,
                OrbitTransform::ScaleAddConstant {
                    scale: round3(rng.range(1.6, 2.6) * sign(rng)),
                    constant: [(); 3].map(|()| round3(rng.range(-1.5, 1.5))),
                },
            ]);
            orbit
        }
        FormulaFamily::AmazingSurf => vec![amazing_surf(rng, 0, iterations)],
        FormulaFamily::MandelboxJulia => vec![mandelbox_julia(rng, 0, iterations)],
        FormulaFamily::AmazingHybrid => {
            let switch = (iterations / 6).max(1) + rng.below((iterations / 4) as usize) as u32;
            vec![
                amazing_surf(rng, 0, switch),
                mandelbox_julia(rng, switch, iterations),
            ]
        }
    };

    let self_recurring = matches!(
        family,
        FormulaFamily::AmazingSurf | FormulaFamily::MandelboxJulia | FormulaFamily::AmazingHybrid
    );
    FractalConfig::Dsl(DslFractalConfig {
        iterations,
        orbit_period: None,
        color_iterations: iterations,
        // Folds that carry their own recurrence need an explicit escape
        // radius to terminate.
        bailout: self_recurring.then_some(100.0),
        normal_epsilon: 1.0e-5,
        orbit,
        material: neutral_material(),
    })
}

/// Returns a nearby variant of a formula: the same transforms with every
/// continuous parameter moved by up to `amount` of its value.
#[must_use]
pub fn mutate(rng: &mut Rng, fractal: &FractalConfig, amount: f64) -> FractalConfig {
    let mut jitter = |value: f32| round3(f64::from(value) * (1.0 + rng.range(-amount, amount)));
    match fractal {
        FractalConfig::Mandelbulb(config) => FractalConfig::Mandelbulb(MandelbulbConfig {
            power: jitter(config.power).clamp(2.0, 32.0),
            ..config.clone()
        }),
        FractalConfig::Mandelbox(config) => FractalConfig::Mandelbox(config.clone()),
        FractalConfig::Dsl(config) => {
            let mut config = config.clone();
            for transform in &mut config.orbit {
                match transform {
                    OrbitTransform::AmazingSurfFold {
                        limits,
                        minimum_radius_squared,
                        scale,
                        ..
                    } => {
                        *limits = limits.map(&mut jitter);
                        *minimum_radius_squared = jitter(*minimum_radius_squared).clamp(0.0, 1.0);
                        *scale = jitter(*scale);
                    }
                    OrbitTransform::MandelboxJuliaFold {
                        fold_limit,
                        min_radius_squared,
                        fixed_radius_squared,
                        scale,
                        constant,
                        ..
                    } => {
                        *fold_limit = jitter(*fold_limit);
                        *min_radius_squared = jitter(*min_radius_squared);
                        *fixed_radius_squared =
                            jitter(*fixed_radius_squared).max(*min_radius_squared * 1.05 + 1.0e-3);
                        *scale = keep_expanding(jitter(*scale));
                        *constant = constant.map(&mut jitter);
                    }
                    OrbitTransform::BoxFold { limit } => *limit = jitter(*limit),
                    OrbitTransform::SphereFold {
                        min_radius_squared,
                        fixed_radius_squared,
                    } => {
                        *min_radius_squared = jitter(*min_radius_squared);
                        *fixed_radius_squared =
                            jitter(*fixed_radius_squared).max(*min_radius_squared * 1.05 + 1.0e-3);
                    }
                    OrbitTransform::ScaleAddPoint { scale } => {
                        *scale = keep_expanding(jitter(*scale));
                    }
                    OrbitTransform::ScaleAddConstant { scale, constant } => {
                        *scale = keep_expanding(jitter(*scale));
                        *constant = constant.map(&mut jitter);
                    }
                    OrbitTransform::Rotate { degrees, .. } => *degrees = jitter(*degrees),
                    OrbitTransform::Translate { offset } => *offset = offset.map(&mut jitter),
                }
            }
            FractalConfig::Dsl(config)
        }
    }
}

/// Affine recurrences must expand, or the orbit never escapes.
fn keep_expanding(scale: f32) -> f32 {
    scale.signum() * scale.abs().clamp(1.05, 4.0)
}

fn mandelbox_core(rng: &mut Rng) -> [OrbitTransform; 3] {
    let min_radius_squared = round3(rng.range(0.15, 0.8));
    // Negative scales give the open, architectural Mandelbox variants.
    let scale_sign = if rng.chance(0.65) { -1.0 } else { 1.0 };
    [
        OrbitTransform::BoxFold {
            limit: round3(rng.range(0.8, 1.5)),
        },
        OrbitTransform::SphereFold {
            min_radius_squared,
            fixed_radius_squared: round3(f64::from(min_radius_squared) + rng.range(0.6, 2.4)),
        },
        OrbitTransform::ScaleAddPoint {
            scale: round3(rng.range(1.7, 3.0) * scale_sign),
        },
    ]
}

fn amazing_surf(rng: &mut Rng, start_iteration: u32, stop_iteration: u32) -> OrbitTransform {
    let mut rotation = || match rng.below(3) {
        0 => 0.0,
        1 => 90.0,
        _ => round3(rng.range(-20.0, 20.0)),
    };
    let rotation_degrees = [rotation(), rotation(), rotation()];
    OrbitTransform::AmazingSurfFold {
        start_iteration,
        stop_iteration,
        limits: [round3(rng.range(0.7, 1.3)), round3(rng.range(0.7, 1.3))],
        minimum_radius_squared: round3(rng.range(0.0, 0.3)),
        scale: round3(rng.range(1.0, 1.8) * sign(rng)),
        rotation_degrees,
    }
}

fn mandelbox_julia(rng: &mut Rng, start_iteration: u32, stop_iteration: u32) -> OrbitTransform {
    let min_radius_squared = round3(rng.range(0.02, 0.5));
    OrbitTransform::MandelboxJuliaFold {
        start_iteration,
        stop_iteration,
        fold_limit: round3(rng.range(0.8, 1.3)),
        min_radius_squared,
        fixed_radius_squared: round3(f64::from(min_radius_squared) + rng.range(0.5, 1.6)),
        scale: round3(rng.range(1.1, 2.4) * sign(rng)),
        constant: [(); 3].map(|()| round3(rng.range(-2.0, 2.0))),
        rotation_degrees: [(); 3].map(|()| round3(rng.range(-10.0, 10.0))),
    }
}

/// Grey material used for scouting, so candidates are judged on form alone.
fn neutral_material() -> DslMaterial {
    DslMaterial {
        base_color: [0.42, 0.42, 0.44],
        accent_color: [0.78, 0.77, 0.74],
        specular_color: [1.0, 1.0, 1.0],
        background_bottom: [0.02, 0.02, 0.025],
        background_top: [0.10, 0.11, 0.13],
        specular_strength: 0.15,
        ..DslMaterial::default()
    }
}

/// Checks on the CPU that a formula describes a visible surface, and measures
/// its extent. Returns `None` for empty, degenerate, or numerically broken
/// shapes.
#[must_use]
pub fn survey(rng: &mut Rng, fractal: &FractalConfig) -> Option<Survey> {
    let estimator = Estimator(fractal);
    let mut hit_radii = Vec::with_capacity(SURVEY_RAYS);
    for _ in 0..SURVEY_RAYS {
        let outward = rng.unit_vector();
        let origin = scale(outward, SURVEY_RADIUS);
        // Slightly off-centre aims find surfaces that a ray through the
        // origin would thread past.
        let aim = normalize(add(scale(outward, -1.0), scale(rng.unit_vector(), 0.08)));
        match estimator.trace(origin, aim, SURVEY_RADIUS * 2.0) {
            Trace::Invalid => return None,
            Trace::Miss => {}
            Trace::Hit(travelled) => {
                hit_radii.push(length(add(origin, scale(aim, travelled))));
            }
        }
    }
    if (hit_radii.len() as f64) < MIN_HIT_SHARE * SURVEY_RAYS as f64 {
        return None;
    }
    hit_radii.sort_by(f64::total_cmp);
    let typical_extent = hit_radii[hit_radii.len() * 9 / 10];
    if typical_extent >= UNBOUNDED_RADIUS {
        return Some(Survey {
            bounded: false,
            radius: UNBOUNDED_WORK_RADIUS,
        });
    }
    let radius = hit_radii[hit_radii.len() - 1] * 1.1;
    (radius >= MIN_BOUND_RADIUS).then_some(Survey {
        bounded: true,
        radius,
    })
}

/// Generates a formula and keeps it only if it surveys as a usable shape.
#[must_use]
pub fn discover(rng: &mut Rng, family: FormulaFamily) -> Option<Shape> {
    let fractal = generate(rng, family);
    let survey = survey(rng, &fractal)?;
    Some(Shape {
        family,
        fractal,
        survey,
    })
}

/// Finds camera placements across the scale regimes the shape supports.
#[must_use]
pub fn viewpoints(rng: &mut Rng, shape: &Shape) -> Vec<Viewpoint> {
    // An unbounded shape has no outside to look at it from.
    let regimes: &[ScaleRegime] = if shape.survey.bounded {
        &[ScaleRegime::Overview, ScaleRegime::Mid, ScaleRegime::Deep]
    } else {
        &[ScaleRegime::Mid, ScaleRegime::Mid, ScaleRegime::Deep]
    };
    regimes
        .iter()
        .filter_map(|regime| match regime {
            ScaleRegime::Overview => Some(overview(rng, shape)),
            regime => interior(rng, shape, *regime),
        })
        .collect()
}

fn overview(rng: &mut Rng, shape: &Shape) -> Viewpoint {
    let outward = rng.unit_vector();
    let distance = shape.survey.radius * 2.6;
    let position = scale(outward, distance);
    Viewpoint {
        position,
        target: [0.0; 3],
        up: perpendicular_up(scale(outward, -1.0)),
        scale_regime: ScaleRegime::Overview,
        clearance: Estimator(&shape.fractal).distance(position).max(0.0),
    }
}

/// Places a camera in free space with the clearance the regime calls for,
/// then turns it towards the most layered view.
fn interior(rng: &mut Rng, shape: &Shape, regime: ScaleRegime) -> Option<Viewpoint> {
    let estimator = Estimator(&shape.fractal);
    let unit = shape.survey.radius.min(UNBOUNDED_WORK_RADIUS);
    let (low, high) = match regime {
        ScaleRegime::Deep => (0.004 * unit, 0.03 * unit),
        _ => (0.08 * unit, 0.4 * unit),
    };
    let reach = if shape.survey.bounded {
        shape.survey.radius * 1.1
    } else {
        shape.survey.radius
    };

    let (position, clearance) = (0..PLACEMENT_ATTEMPTS).find_map(|_| {
        // Cube-root radius distributes points uniformly through the ball.
        let point = scale(rng.unit_vector(), reach * rng.unit().cbrt());
        let clearance = estimator.distance(point);
        (clearance.is_finite() && (low..=high).contains(&clearance)).then_some((point, clearance))
    })?;

    let max_distance = clearance * 60.0;
    let mut best: Option<(f64, [f64; 3], f64)> = None;
    for _ in 0..DIRECTION_CANDIDATES {
        let direction = rng.unit_vector();
        let Trace::Hit(centre) = estimator.trace(position, direction, max_distance) else {
            continue;
        };
        let up = perpendicular_up(direction);
        let right = cross(direction, up);
        let mut depths = vec![centre.ln()];
        for (horizontal, vertical) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            let offset = add(
                scale(right, horizontal * FAN_ANGLE),
                scale(up, vertical * FAN_ANGLE),
            );
            let depth =
                match estimator.trace(position, normalize(add(direction, offset)), max_distance) {
                    Trace::Hit(distance) => distance,
                    _ => max_distance,
                };
            depths.push(depth.ln());
        }
        let mean = depths.iter().sum::<f64>() / depths.len() as f64;
        let layering = (depths
            .iter()
            .map(|depth| (depth - mean).powi(2))
            .sum::<f64>()
            / depths.len() as f64)
            .sqrt();
        // A subject a few clearances away fills the frame without clipping
        // into the lens; depth spread across the fan gives the eye layers.
        let framing = 1.0 - ((centre / (6.0 * clearance)).ln().abs() / 6.0_f64.ln()).min(1.0);
        let score = framing + 0.5 * layering.min(2.0);
        if best.is_none_or(|(best_score, ..)| score > best_score) {
            best = Some((score, direction, centre));
        }
    }
    let (_, direction, centre) = best?;

    Some(Viewpoint {
        position,
        target: add(position, scale(direction, centre)),
        up: perpendicular_up(direction),
        scale_regime: regime,
        clearance,
    })
}

/// Builds the render configuration for a scouting thumbnail of a viewpoint.
#[must_use]
pub fn thumbnail_config(
    shape: &Shape,
    viewpoint: &Viewpoint,
    width: u32,
    height: u32,
    seed: u32,
) -> RenderConfig {
    let direction = normalize(sub(viewpoint.target, viewpoint.position));
    let right = cross(direction, viewpoint.up);
    // Key light from behind the camera's upper shoulder: readable relief
    // without committing to a lighting mood.
    let light = normalize(add(
        add(scale(direction, -0.6), scale(right, 0.5)),
        scale(viewpoint.up, 0.6),
    ));
    let subject_distance = length(sub(viewpoint.target, viewpoint.position));
    let mut fractal = shape.fractal.clone();
    if let FractalConfig::Dsl(dsl) = &mut fractal {
        // Fog grows with the square of distance. Tying it to the subject
        // distance separates depth layers at every scale without colour; a
        // fixed density would erase overviews and do nothing in close-ups.
        dsl.material.fog_density =
            (SUBJECT_FOG / (subject_distance * subject_distance)).min(MAX_FOG_DENSITY) as f32;
    }
    let mut config = RenderConfig {
        fractal,
        seed,
        ..RenderConfig::default()
    };
    config.camera.position = QfVec3::from_f64(viewpoint.position);
    config.camera.target = QfVec3::from_f64(viewpoint.target);
    config.camera.up = viewpoint.up.map(|value| value as f32);
    config.camera.vertical_fov_degrees = THUMBNAIL_FOV_DEGREES;
    config.camera.focus_distance = subject_distance.max(1.0e-6) as f32;
    config.light.direction = light.map(|value| value as f32);
    config.render = RenderSettings {
        width,
        height,
        max_steps: 320,
        max_distance: (subject_distance * 12.0).max(shape.survey.radius * 4.0) as f32,
        // The projected term scales precision with distance; the fixed term
        // only guards against a zero footprint at the lens.
        epsilon: (viewpoint.clearance * 1.0e-4).clamp(1.0e-7, 1.0e-3) as f32,
        step_safety: TRACE_SAFETY as f32,
        pixel_epsilon_multiplier: 1.0,
    };
    config
}

fn perpendicular_up(direction: [f64; 3]) -> [f64; 3] {
    // World Z is up unless the view is nearly vertical.
    let reference = if direction[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let right = normalize(cross(direction, reference));
    normalize(cross(right, direction))
}

fn sign(rng: &mut Rng) -> f64 {
    if rng.chance(0.5) { 1.0 } else { -1.0 }
}

/// Three decimals keep generated formulas legible in saved scene files.
fn round3(value: f64) -> f32 {
    ((value * 1000.0).round() / 1000.0) as f32
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f64; 3], factor: f64) -> [f64; 3] {
    a.map(|value| value * factor)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let length = length(a);
    if length > 0.0 {
        scale(a, 1.0 / length)
    } else {
        [0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axes::classify_family;

    fn config_for(fractal: &FractalConfig) -> RenderConfig {
        RenderConfig {
            fractal: fractal.clone(),
            ..RenderConfig::default()
        }
    }

    #[test]
    fn generated_formulas_are_valid_and_of_the_requested_family() {
        let mut rng = Rng::new(1);
        for family in FormulaFamily::ALL {
            for _ in 0..200 {
                let fractal = generate(&mut rng, *family);
                config_for(&fractal)
                    .validate()
                    .unwrap_or_else(|error| panic!("{family:?}: {error:#}\n{fractal:?}"));
                assert_eq!(classify_family(&fractal), *family, "{fractal:?}");
            }
        }
    }

    #[test]
    fn generation_is_deterministic_and_varied() {
        let fractal = |seed| {
            format!(
                "{:?}",
                generate(&mut Rng::new(seed), FormulaFamily::JuliaBox)
            )
        };
        assert_eq!(fractal(5), fractal(5));
        assert_ne!(fractal(5), fractal(6));
    }

    #[test]
    fn mutations_stay_valid_close_and_in_family() {
        let mut rng = Rng::new(2);
        for family in FormulaFamily::ALL {
            for _ in 0..100 {
                let original = generate(&mut rng, *family);
                let variant = mutate(&mut rng, &original, 0.1);
                config_for(&variant)
                    .validate()
                    .unwrap_or_else(|error| panic!("{family:?}: {error:#}\n{variant:?}"));
                assert_eq!(classify_family(&variant), *family);
            }
        }
        let original = generate(&mut rng, FormulaFamily::Mandelbox);
        assert_ne!(
            format!("{original:?}"),
            format!("{:?}", mutate(&mut rng, &original, 0.1))
        );
    }

    #[test]
    fn survey_measures_a_bounded_shape() {
        let fractal = FractalConfig::Mandelbulb(MandelbulbConfig::default());
        let survey = survey(&mut Rng::new(3), &fractal).unwrap();
        assert!(survey.bounded);
        // The power-8 Mandelbulb reaches a little beyond the unit sphere.
        assert!((1.0..1.6).contains(&survey.radius), "{survey:?}");
    }

    #[test]
    fn every_family_yields_usable_shapes() {
        let mut rng = Rng::new(4);
        for family in FormulaFamily::ALL {
            let found = (0..40).filter_map(|_| discover(&mut rng, *family)).count();
            assert!(found >= 4, "{family:?} produced only {found} of 40 shapes");
        }
    }

    #[test]
    fn overview_looks_at_the_shape_from_outside() {
        let mut rng = Rng::new(5);
        let shape = Shape {
            family: FormulaFamily::Mandelbulb,
            fractal: FractalConfig::Mandelbulb(MandelbulbConfig::default()),
            survey: Survey {
                bounded: true,
                radius: 1.3,
            },
        };
        let found = viewpoints(&mut rng, &shape);
        let overview = found
            .iter()
            .find(|viewpoint| viewpoint.scale_regime == ScaleRegime::Overview)
            .unwrap();
        assert!((length(overview.position) - 1.3 * 2.6).abs() < 1e-9);
        assert_eq!(overview.target, [0.0; 3]);
        assert!(overview.clearance > 1.0);
    }

    #[test]
    fn interior_viewpoints_respect_their_clearance_window() {
        let mut rng = Rng::new(6);
        let mut checked = 0;
        for family in FormulaFamily::ALL {
            let Some(shape) = (0..40).find_map(|_| discover(&mut rng, *family)) else {
                continue;
            };
            let estimator = Estimator(&shape.fractal);
            let unit = shape.survey.radius.min(UNBOUNDED_WORK_RADIUS);
            for viewpoint in viewpoints(&mut rng, &shape) {
                let direction = normalize(sub(viewpoint.target, viewpoint.position));
                // The camera frame must be orthonormal for the renderer.
                assert!(
                    direction
                        .iter()
                        .zip(&viewpoint.up)
                        .map(|(a, b)| a * b)
                        .sum::<f64>()
                        .abs()
                        < 1e-9
                );
                assert!((length(viewpoint.up) - 1.0).abs() < 1e-9);
                if viewpoint.scale_regime == ScaleRegime::Overview {
                    continue;
                }
                let clearance = estimator.distance(viewpoint.position);
                let (low, high) = match viewpoint.scale_regime {
                    ScaleRegime::Deep => (0.004 * unit, 0.03 * unit),
                    _ => (0.08 * unit, 0.4 * unit),
                };
                assert!((low..=high).contains(&clearance), "{viewpoint:?}");
                assert!(length(sub(viewpoint.target, viewpoint.position)) > clearance * 0.5);
                checked += 1;
            }
        }
        assert!(
            checked >= 6,
            "only {checked} interior viewpoints were found"
        );
    }

    #[test]
    fn unbounded_shapes_get_no_overview() {
        let mut rng = Rng::new(7);
        let shape = Shape {
            family: FormulaFamily::Mandelbulb,
            fractal: FractalConfig::Mandelbulb(MandelbulbConfig::default()),
            survey: Survey {
                bounded: false,
                radius: 3.0,
            },
        };
        assert!(
            viewpoints(&mut rng, &shape)
                .iter()
                .all(|viewpoint| viewpoint.scale_regime != ScaleRegime::Overview)
        );
    }

    #[test]
    fn thumbnail_configs_are_renderable() {
        let mut rng = Rng::new(8);
        for family in FormulaFamily::ALL {
            let Some(shape) = (0..40).find_map(|_| discover(&mut rng, *family)) else {
                continue;
            };
            for viewpoint in viewpoints(&mut rng, &shape) {
                let config = thumbnail_config(&shape, &viewpoint, 320, 180, 1);
                config
                    .validate()
                    .unwrap_or_else(|error| panic!("{family:?} {viewpoint:?}: {error:#}"));
                assert_eq!((config.render.width, config.render.height), (320, 180));
            }
        }
    }
}
