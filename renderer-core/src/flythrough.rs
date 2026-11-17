//! One continuous camera move across many levels of scale.
//!
//! The planner starts beside a surface deep inside the shape and walks
//! outwards through whatever open space it finds, always staying a fraction
//! of the local clearance away from the walls. Travelling at a speed
//! proportional to the local scale turns that walk into a steady zoom, and
//! the same plan played backwards is a dive.

use anyhow::{Result, bail};

use crate::DistanceEstimator;

/// Directions probed at every step of the walk.
const PROBE_DIRECTIONS: usize = 40;
/// Reach of a probe ray, in local clearances.
const PROBE_REACH: f64 = 40.0;
const PROBE_STEPS: u32 = 24;
/// Generated estimators can overestimate distance, so no step trusts one in
/// full.
const STEP_SAFETY: f64 = 0.8;
/// Length of one step of the walk, in local clearances.
const STEP_CLEARANCES: f64 = 0.35;
/// Share of the newly chosen direction blended in per step. Limiting the
/// turn keeps the path a smooth curve.
const TURN_RATE: f64 = 0.3;
const MAX_WALK_STEPS: usize = 8_000;
/// Steps the clearance may fail to double in before the walk counts as
/// trapped, or as crawling along a wall instead of moving out.
const STALL_STEPS: usize = 300;
/// Weight of moving away from the nearest surface, which breaks the tie
/// between leaving a wall and running along it.
const OPENING_WEIGHT: f64 = 0.9;
/// A probe shorter than this many clearances counts as blocked when judging
/// how enclosed the path is.
const ENCLOSED_CLEARANCES: f64 = 12.0;
/// How far behind itself the camera looks, in units of travel divided by
/// local scale.
const LOOK_LAG: f64 = 2.5;
/// Samples of the finished plan per second of screen time.
const SAMPLES_PER_SECOND: f64 = 8.0;
const MIN_SAMPLES: usize = 64;
/// Share of the duration spent easing in and out of the move.
const EASE_SHARE: f64 = 0.08;
/// Farthest focus distance considered, in local scales.
const FOCUS_REACH: f64 = 60.0;
const FOCUS_FAN_ANGLE: f64 = 0.08;
/// Largest turn away from the way back, in radians.
const AIM_CONE: f64 = 0.6;
/// Rings of candidate directions within the aim cone.
const AIM_RINGS: usize = 2;
const AIM_PER_RING: usize = 8;
/// Half-angle of the ray fan that judges how layered a direction is.
const AIM_FAN_ANGLE: f64 = 0.3;
/// Weight of keeping to the way back against layering.
const AIM_KEEP: f64 = 0.8;
/// Width of the smoothing that turns chosen directions into a pan.
const AIM_SMOOTHING_SECONDS: f64 = 1.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlythroughDirection {
    /// From the overview down to the deep end.
    In,
    /// From the deep end out to the overview.
    Out,
}

/// A planned walk between a point deep inside a bounded shape and a view of
/// the whole of it.
#[derive(Clone, Debug)]
pub struct FlythroughPath {
    pub duration: f64,
    pub direction: FlythroughDirection,
    /// Distance from the camera to the nearest surface at the deep end.
    pub deep_clearance: f64,
    /// Distance from the origin at the far end.
    pub overview_distance: f64,
    /// Radius of a sphere containing the shape.
    pub bound_radius: f64,
    /// Roll about the line of sight accumulated over the whole move.
    pub roll_degrees: f64,
    /// Starting points tried; the most enclosed walk is kept.
    pub attempts: u32,
    pub(crate) plan: Option<FlythroughPlan>,
}

#[derive(Clone, Debug)]
pub(crate) struct FlythroughPlan {
    /// Evenly spaced in screen time, ordered from the deep end outwards.
    samples: Vec<PlanSample>,
    summary: FlythroughSummary,
}

#[derive(Clone, Copy, Debug)]
struct PlanSample {
    position: [f64; 3],
    forward: [f64; 3],
    up: [f64; 3],
    scale: f64,
    focus: f64,
}

/// What the planner found, for reports and for choosing between shapes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlythroughSummary {
    /// Natural logarithm of the ratio between the largest and smallest
    /// scale the camera passes through.
    pub zoom_e_folds: f64,
    /// Mean share of directions blocked by nearby structure along the walk.
    pub enclosure: f64,
    /// Length of the walk in local scales: how much travelling there is
    /// between the two ends.
    pub travel_scales: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FlythroughSample {
    pub position: [f64; 3],
    pub forward: [f64; 3],
    pub up: [f64; 3],
    /// Size of the detail around the camera.
    pub scale: f64,
    /// Distance to the surface at the centre of the view.
    pub focus: f64,
}

impl FlythroughPath {
    #[must_use]
    pub fn new(
        duration: f64,
        direction: FlythroughDirection,
        deep_clearance: f64,
        overview_distance: f64,
        bound_radius: f64,
    ) -> Self {
        Self {
            duration,
            direction,
            deep_clearance,
            overview_distance,
            bound_radius,
            roll_degrees: 0.0,
            attempts: 8,
            plan: None,
        }
    }

    pub(crate) fn validate_parameters(&self) -> Result<()> {
        if !self.duration.is_finite() || self.duration <= 0.0 {
            bail!("flythrough duration must be finite and greater than zero");
        }
        if !self.bound_radius.is_finite() || self.bound_radius <= 0.0 {
            bail!("flythrough bound_radius must be finite and greater than zero");
        }
        if !self.deep_clearance.is_finite()
            || self.deep_clearance < 1.0e-12
            || self.deep_clearance > self.bound_radius * 0.1
        {
            bail!("flythrough deep_clearance must be in 1e-12..=bound_radius / 10");
        }
        if !self.overview_distance.is_finite()
            || self.overview_distance < self.bound_radius * 1.2
            || self.overview_distance > self.bound_radius * 100.0
        {
            bail!("flythrough overview_distance must be 1.2 to 100 times bound_radius");
        }
        if !self.roll_degrees.is_finite() || self.roll_degrees.abs() > 3_600.0 {
            bail!("flythrough roll_degrees must be in -3600..=3600");
        }
        if self.attempts == 0 || self.attempts > 256 {
            bail!("flythrough attempts must be in 1..=256");
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_parameters()?;
        if self.plan.is_none() {
            bail!("flythrough has not been planned");
        }
        Ok(())
    }

    /// Iteration limit that resolves a DSL fractal down to the deep end of
    /// this path, given the limit that resolves it at the scale of the whole
    /// shape. Plan and render with it: a fractal is a different shape at
    /// another limit, and a route through one can run into the other.
    #[must_use]
    pub fn deep_iterations(&self, fractal: &crate::DslFractalConfig) -> u32 {
        let depth = (self.bound_radius / self.deep_clearance).max(1.0).ln();
        let extra = (depth / fractal.detail_growth().ln()).ceil() as u32;
        fractal
            .iterations
            .saturating_add(extra)
            .min(crate::config::MAX_FRACTAL_ITERATIONS)
    }

    /// What the planner found, once planned.
    #[must_use]
    pub fn summary(&self) -> Option<FlythroughSummary> {
        self.plan.as_ref().map(|plan| plan.summary)
    }

    /// Plans the walk from the CPU distance estimator. The result depends
    /// only on the estimator, the parameters, and the seed.
    pub fn plan<D: DistanceEstimator>(&mut self, estimator: &D, seed: u32) -> Result<()> {
        self.validate_parameters()?;
        let mut random = Random::new(seed);
        let mut best: Option<(f64, Walk)> = None;
        for _ in 0..self.attempts {
            let Some(walk) = self.walk(estimator, &mut random) else {
                continue;
            };
            let score = walk.enclosure;
            if best
                .as_ref()
                .is_none_or(|(best_score, _)| score > *best_score)
            {
                best = Some((score, walk));
            }
        }
        let Some((_, walk)) = best else {
            bail!(
                "no open route was found from a clearance of {:e} out of the shape in {} attempts; \
                 try another seed, a larger deep_clearance, or another shape",
                self.deep_clearance,
                self.attempts
            );
        };
        self.plan = Some(self.finish(estimator, &walk)?);
        Ok(())
    }

    /// Walks from a point beside a surface out to the overview distance.
    fn walk<D: DistanceEstimator>(&self, estimator: &D, random: &mut Random) -> Option<Walk> {
        let (start, surface) = self.deep_start(estimator, random)?;
        let probes = fibonacci_sphere(PROBE_DIRECTIONS);
        let mut position = start;
        // Begin by backing straight away from the surface.
        let mut direction = normalize(sub(start, surface));
        let mut points = Vec::new();
        let mut enclosure_sum = 0.0;
        let mut enclosure_count = 0_usize;
        let mut doubled_at = (estimator.distance_estimate(start), 0_usize);

        for step in 0..MAX_WALK_STEPS {
            let clearance = estimator.distance_estimate(position);
            if !clearance.is_finite() || clearance <= 0.0 {
                return None;
            }
            points.push(WalkPoint {
                position,
                clearance,
            });
            let radius = length(position);
            if radius >= self.overview_distance {
                return Some(Walk {
                    points,
                    surface,
                    enclosure: enclosure_sum / enclosure_count.max(1) as f64,
                });
            }
            if clearance >= doubled_at.0 * 2.0 {
                doubled_at = (clearance, step);
            } else if step - doubled_at.1 > STALL_STEPS {
                return None;
            }

            let outward = if radius > 0.0 {
                scale(position, radius.recip())
            } else {
                direction
            };
            let chosen = if radius >= self.bound_radius * 1.05 {
                // Clear of the shape: leave along the radius.
                outward
            } else {
                let opening = clearance_gradient(estimator, position, clearance);
                let mut best = (f64::NEG_INFINITY, direction);
                let mut blocked = 0_usize;
                for probe in &probes {
                    let free = free_distance(estimator, position, *probe, clearance);
                    if free < ENCLOSED_CLEARANCES * clearance {
                        blocked += 1;
                    }
                    // Open space first, then keeping course, then a mild
                    // preference for the way out.
                    let score = (free / clearance).max(1.0).ln()
                        + 1.2 * dot(*probe, direction)
                        + 0.6 * dot(*probe, outward)
                        + OPENING_WEIGHT * dot(*probe, opening);
                    if score > best.0 {
                        best = (score, *probe);
                    }
                }
                enclosure_sum += blocked as f64 / probes.len() as f64;
                enclosure_count += 1;
                best.1
            };
            direction = normalize(add(
                scale(direction, 1.0 - TURN_RATE),
                scale(chosen, TURN_RATE),
            ));

            // Shorten the step until it lands in open space.
            let mut stride = STEP_CLEARANCES * clearance;
            let mut moved = false;
            for _ in 0..6 {
                let candidate = add(position, scale(direction, stride));
                let landed = estimator.distance_estimate(candidate);
                if landed.is_finite() && landed > 0.4 * clearance {
                    position = candidate;
                    moved = true;
                    break;
                }
                stride *= 0.5;
            }
            if !moved {
                // Blocked ahead: turn to the most open probe outright.
                direction = chosen;
                let candidate = add(position, scale(direction, 0.1 * clearance));
                let landed = estimator.distance_estimate(candidate);
                if !(landed.is_finite() && landed > 0.4 * clearance) {
                    return None;
                }
                position = candidate;
            }
        }
        None
    }

    /// Finds a point at the deep clearance by tracing from open space inside
    /// the bound towards a surface. Returns the point and the surface point
    /// it stopped beside.
    fn deep_start<D: DistanceEstimator>(
        &self,
        estimator: &D,
        random: &mut Random,
    ) -> Option<([f64; 3], [f64; 3])> {
        for _ in 0..64 {
            let origin = scale(
                random.unit_vector(),
                self.bound_radius * random.next_f64().cbrt(),
            );
            let direction = random.unit_vector();
            let mut travel = 0.0;
            for _ in 0..400 {
                let point = add(origin, scale(direction, travel));
                let distance = estimator.distance_estimate(point);
                if !distance.is_finite() || length(point) > self.bound_radius * 1.5 {
                    break;
                }
                if distance <= self.deep_clearance {
                    if distance < self.deep_clearance * 0.25 {
                        // Overshot into the surface: not a usable start.
                        break;
                    }
                    let surface = add(point, scale(direction, distance));
                    return Some((point, surface));
                }
                // Approach without stepping past the wanted clearance.
                travel += ((distance - self.deep_clearance) * STEP_SAFETY)
                    .max(self.deep_clearance * 0.25);
            }
        }
        None
    }

    /// Turns a walk into evenly timed camera samples.
    fn finish<D: DistanceEstimator>(&self, estimator: &D, walk: &Walk) -> Result<FlythroughPlan> {
        let points = &walk.points;
        if points.len() < 8 {
            bail!("flythrough walk is too short to animate");
        }

        // Local scale: the clearance, held up by the largest space already
        // passed so that squeezing through a gap does not stall the move.
        let mut running_max = 0.0_f64;
        let raw_scale: Vec<f64> = points
            .iter()
            .map(|point| {
                running_max = running_max.max(point.clearance);
                (point.clearance * running_max).sqrt()
            })
            .collect();
        let raw_progress = progress_along(points, &raw_scale);
        let log_scale: Vec<f64> = raw_scale.iter().map(|value| value.ln()).collect();
        let scale_at: Vec<f64> = smooth(&raw_progress, &log_scale, 1.5)
            .into_iter()
            .map(f64::exp)
            .collect();
        // Progress counts travel in local scales, so equal progress is equal
        // apparent motion on screen.
        let progress = progress_along(points, &scale_at);
        let total = *progress.last().expect("non-empty walk");

        let count = ((self.duration * SAMPLES_PER_SECOND).ceil() as usize).max(MIN_SAMPLES);
        let eased = eased_progress(count);
        let position_at = |value: f64| -> [f64; 3] {
            if value <= 0.0 {
                // Before the walk began the camera would have been closer to
                // the surface it started beside.
                let amount = 1.0 - value.exp();
                return add(
                    points[0].position,
                    scale(sub(walk.surface, points[0].position), amount),
                );
            }
            let index = progress
                .partition_point(|entry| *entry <= value)
                .clamp(1, points.len() - 1);
            let (low, high) = (progress[index - 1], progress[index]);
            let amount = if high > low {
                ((value - low) / (high - low)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let at = |offset: isize| {
                let clamped = (index as isize + offset).clamp(0, points.len() as isize - 1);
                points[clamped as usize].position
            };
            catmull_rom(at(-2), at(-1), at(0), at(1), amount)
        };
        let scale_for = |value: f64| -> f64 {
            let index = progress
                .partition_point(|entry| *entry <= value)
                .clamp(1, points.len() - 1);
            let (low, high) = (progress[index - 1], progress[index]);
            let amount = if high > low {
                ((value - low) / (high - low)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (scale_at[index - 1].ln() * (1.0 - amount) + scale_at[index].ln() * amount).exp()
        };

        let mut samples = Vec::with_capacity(count);
        for fraction in &eased {
            let value = fraction * total;
            let position = position_at(value);
            let radius = length(position);
            // The camera looks back at where it came from, and at the whole
            // shape once it has left it.
            let behind = position_at(value - LOOK_LAG);
            let leaving =
                smoothstep((radius - 0.7 * self.bound_radius) / (0.6 * self.bound_radius));
            let look_at = scale(behind, 1.0 - leaving);
            let forward = normalize(sub(look_at, position));
            samples.push(PlanSample {
                position,
                forward,
                up: [0.0; 3],
                scale: scale_for(value.max(0.0)),
                focus: 0.0,
            });
        }

        // Inside the shape, turn from the way back towards the direction
        // with the most layered depth, so that a flat wall behind the camera
        // does not fill the frame. Outside, keep looking at the shape.
        for sample in &mut samples {
            if length(sample.position) < 0.9 * self.bound_radius {
                sample.forward =
                    layered_aim(estimator, sample.position, sample.forward, sample.scale);
            }
        }

        // Smooth the line of sight slowly enough that the camera pans
        // instead of jumping, then carry an up vector along it.
        let indices: Vec<f64> = (0..count).map(|index| index as f64).collect();
        let aim_width = AIM_SMOOTHING_SECONDS * SAMPLES_PER_SECOND;
        for axis in 0..3 {
            let component: Vec<f64> = samples.iter().map(|sample| sample.forward[axis]).collect();
            for (sample, value) in samples
                .iter_mut()
                .zip(smooth(&indices, &component, aim_width))
            {
                sample.forward[axis] = value;
            }
        }
        let mut up = perpendicular(normalize(samples[0].forward));
        for (index, sample) in samples.iter_mut().enumerate() {
            sample.forward = normalize(sample.forward);
            up = normalize(sub(up, scale(sample.forward, dot(up, sample.forward))));
            let roll = self.roll_degrees.to_radians() * eased[index];
            sample.up = rotate_about(up, sample.forward, roll);
        }

        // Focus on what the centre of the view rests on.
        let mut log_focus = Vec::with_capacity(count);
        for sample in &samples {
            let right = normalize(cross(sample.forward, sample.up));
            let reach = FOCUS_REACH * sample.scale;
            let mut hits: Vec<f64> = [(0.0, 0.0), (1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)]
                .iter()
                .map(|(horizontal, vertical)| {
                    let direction = normalize(add(
                        sample.forward,
                        add(
                            scale(right, horizontal * FOCUS_FAN_ANGLE),
                            scale(sample.up, vertical * FOCUS_FAN_ANGLE),
                        ),
                    ));
                    hit_distance(estimator, sample.position, direction, reach)
                })
                .collect();
            hits.sort_by(f64::total_cmp);
            log_focus.push(hits[2].clamp(0.3 * sample.scale, reach).ln());
        }
        for (sample, value) in samples.iter_mut().zip(smooth(&indices, &log_focus, 3.0)) {
            sample.focus = value.exp();
        }

        for sample in &samples {
            let clearance = estimator.distance_estimate(sample.position);
            if !clearance.is_finite() || clearance <= 0.0 {
                bail!("flythrough path touches the surface; try another seed");
            }
        }

        let smallest = scale_at.iter().copied().fold(f64::INFINITY, f64::min);
        let largest = scale_at.iter().copied().fold(0.0, f64::max);
        Ok(FlythroughPlan {
            samples,
            summary: FlythroughSummary {
                zoom_e_folds: (largest / smallest).ln(),
                enclosure: walk.enclosure,
                travel_scales: total,
            },
        })
    }

    pub(crate) fn sample(&self, time_seconds: f64) -> Result<FlythroughSample> {
        self.validate()?;
        let samples = &self.plan.as_ref().expect("validated plan").samples;
        let mut fraction = (time_seconds / self.duration).clamp(0.0, 1.0);
        if self.direction == FlythroughDirection::In {
            fraction = 1.0 - fraction;
        }
        let place = fraction * (samples.len() - 1) as f64;
        let index = (place.floor() as usize).min(samples.len() - 2);
        let amount = place - index as f64;
        let at = |offset: isize| {
            let clamped = (index as isize + offset).clamp(0, samples.len() as isize - 1);
            &samples[clamped as usize]
        };
        let (low, high) = (at(0), at(1));
        let blend =
            |a: [f64; 3], b: [f64; 3]| normalize(add(scale(a, 1.0 - amount), scale(b, amount)));
        let forward = blend(low.forward, high.forward);
        let up = blend(low.up, high.up);
        Ok(FlythroughSample {
            position: catmull_rom(
                at(-1).position,
                low.position,
                high.position,
                at(2).position,
                amount,
            ),
            forward,
            up: normalize(sub(up, scale(forward, dot(up, forward)))),
            scale: (low.scale.ln() * (1.0 - amount) + high.scale.ln() * amount).exp(),
            focus: (low.focus.ln() * (1.0 - amount) + high.focus.ln() * amount).exp(),
        })
    }
}

struct Walk {
    points: Vec<WalkPoint>,
    /// Surface point the walk started beside.
    surface: [f64; 3],
    enclosure: f64,
}

#[derive(Clone, Copy)]
struct WalkPoint {
    position: [f64; 3],
    clearance: f64,
}

/// Cumulative travel along the walk, each step measured in local scales.
fn progress_along(points: &[WalkPoint], scales: &[f64]) -> Vec<f64> {
    let mut progress = Vec::with_capacity(points.len());
    let mut total = 0.0;
    progress.push(total);
    for index in 1..points.len() {
        let step = length(sub(points[index].position, points[index - 1].position));
        total += step / (0.5 * (scales[index] + scales[index - 1]));
        progress.push(total);
    }
    progress
}

/// Screen-time fractions mapped to path fractions, easing in and out.
fn eased_progress(count: usize) -> Vec<f64> {
    let speed = |fraction: f64| {
        smoothstep(fraction / EASE_SHARE).min(smoothstep((1.0 - fraction) / EASE_SHARE)) + 0.02
    };
    let mut cumulative = Vec::with_capacity(count);
    let mut total = 0.0;
    cumulative.push(total);
    for index in 1..count {
        let middle = (index as f64 - 0.5) / (count - 1) as f64;
        total += speed(middle);
        cumulative.push(total);
    }
    cumulative.iter().map(|value| value / total).collect()
}

/// Gaussian smoothing of `values` over the coordinate `at`.
fn smooth(at: &[f64], values: &[f64], width: f64) -> Vec<f64> {
    let mut start = 0;
    (0..values.len())
        .map(|index| {
            while at[index] - at[start] > 3.0 * width {
                start += 1;
            }
            let mut weight_sum = 0.0;
            let mut value_sum = 0.0;
            for other in start..values.len() {
                let offset = at[other] - at[index];
                if offset > 3.0 * width {
                    break;
                }
                let weight = (-0.5 * (offset / width).powi(2)).exp();
                weight_sum += weight;
                value_sum += weight * values[other];
            }
            value_sum / weight_sum
        })
        .collect()
}

/// Direction in which the clearance grows fastest.
fn clearance_gradient<D: DistanceEstimator>(
    estimator: &D,
    position: [f64; 3],
    clearance: f64,
) -> [f64; 3] {
    let step = 0.25 * clearance;
    let at = |axis: usize, sign: f64| {
        let mut point = position;
        point[axis] += sign * step;
        estimator.distance_estimate(point)
    };
    let gradient = [0, 1, 2].map(|axis| at(axis, 1.0) - at(axis, -1.0));
    if gradient.iter().all(|value| value.is_finite()) && length(gradient) > 0.0 {
        normalize(gradient)
    } else {
        [0.0; 3]
    }
}

/// The direction near `base` whose view holds the most layered depth: the
/// spread of log distances over a fan of rays, as in scouting.
fn layered_aim<D: DistanceEstimator>(
    estimator: &D,
    position: [f64; 3],
    base: [f64; 3],
    local_scale: f64,
) -> [f64; 3] {
    let up = perpendicular(base);
    let right = normalize(cross(base, up));
    let reach = FOCUS_REACH * local_scale;
    let mut candidates = vec![base];
    for ring in 1..=AIM_RINGS {
        let tilt = AIM_CONE * ring as f64 / AIM_RINGS as f64;
        for index in 0..AIM_PER_RING {
            let angle = std::f64::consts::TAU * index as f64 / AIM_PER_RING as f64;
            candidates.push(normalize(add(
                scale(base, tilt.cos()),
                scale(
                    add(scale(right, angle.cos()), scale(up, angle.sin())),
                    tilt.sin(),
                ),
            )));
        }
    }
    let mut best = (f64::NEG_INFINITY, base);
    for candidate in candidates {
        let side = normalize(cross(candidate, up));
        let above = cross(side, candidate);
        let depths: Vec<f64> = [(0.0, 0.0), (1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)]
            .iter()
            .map(|(horizontal, vertical)| {
                let direction = normalize(add(
                    candidate,
                    add(
                        scale(side, horizontal * AIM_FAN_ANGLE),
                        scale(above, vertical * AIM_FAN_ANGLE),
                    ),
                ));
                (hit_distance(estimator, position, direction, reach) / local_scale)
                    .max(1.0e-3)
                    .ln()
            })
            .collect();
        let mean = depths.iter().sum::<f64>() / depths.len() as f64;
        let layering = (depths
            .iter()
            .map(|depth| (depth - mean).powi(2))
            .sum::<f64>()
            / depths.len() as f64)
            .sqrt();
        let score = layering + AIM_KEEP * dot(candidate, base);
        if score > best.0 {
            best = (score, candidate);
        }
    }
    best.1
}

/// Distance a ray travels before coming within a small angle of a surface,
/// capped at the probe reach.
fn free_distance<D: DistanceEstimator>(
    estimator: &D,
    origin: [f64; 3],
    direction: [f64; 3],
    clearance: f64,
) -> f64 {
    let reach = PROBE_REACH * clearance;
    let mut travel = clearance * STEP_SAFETY;
    for _ in 0..PROBE_STEPS {
        let distance = estimator.distance_estimate(add(origin, scale(direction, travel)));
        if !distance.is_finite() || distance < 0.03 * travel {
            return travel;
        }
        travel += distance * STEP_SAFETY;
        if travel >= reach {
            return reach;
        }
    }
    travel
}

/// Sphere-traces to the first surface along a ray.
fn hit_distance<D: DistanceEstimator>(
    estimator: &D,
    origin: [f64; 3],
    direction: [f64; 3],
    reach: f64,
) -> f64 {
    let mut travel = 0.0;
    for _ in 0..200 {
        let distance = estimator.distance_estimate(add(origin, scale(direction, travel)));
        if !distance.is_finite() {
            return reach;
        }
        if distance < 1.0e-3 * travel.max(reach * 1.0e-3) {
            return travel;
        }
        travel += distance * STEP_SAFETY;
        if travel >= reach {
            return reach;
        }
    }
    travel
}

fn fibonacci_sphere(count: usize) -> Vec<[f64; 3]> {
    let golden_angle = std::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
    (0..count)
        .map(|index| {
            let height = 1.0 - 2.0 * (index as f64 + 0.5) / count as f64;
            let radius = (1.0 - height * height).sqrt();
            let angle = golden_angle * index as f64;
            [radius * angle.cos(), radius * angle.sin(), height]
        })
        .collect()
}

fn catmull_rom(a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3], amount: f64) -> [f64; 3] {
    let squared = amount * amount;
    let cubed = squared * amount;
    [0, 1, 2].map(|axis| {
        0.5 * (2.0 * b[axis]
            + (c[axis] - a[axis]) * amount
            + (2.0 * a[axis] - 5.0 * b[axis] + 4.0 * c[axis] - d[axis]) * squared
            + (3.0 * b[axis] - a[axis] - 3.0 * c[axis] + d[axis]) * cubed)
    })
}

fn rotate_about(value: [f64; 3], axis: [f64; 3], radians: f64) -> [f64; 3] {
    let (sine, cosine) = radians.sin_cos();
    add(
        add(scale(value, cosine), scale(cross(axis, value), sine)),
        scale(axis, dot(axis, value) * (1.0 - cosine)),
    )
}

fn perpendicular(direction: [f64; 3]) -> [f64; 3] {
    let reference = if direction[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let right = normalize(cross(direction, reference));
    normalize(cross(right, direction))
}

fn smoothstep(value: f64) -> f64 {
    let clamped = value.clamp(0.0, 1.0);
    clamped * clamped * (3.0 - 2.0 * clamped)
}

struct Random {
    state: u64,
}

impl Random {
    fn new(seed: u32) -> Self {
        Self {
            state: u64::from(seed).wrapping_add(0x9e37_79b9_7f4a_7c15),
        }
    }

    fn next_f64(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let value = self.state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        (value >> 11) as f64 * (1.0 / ((1_u64 << 53) as f64))
    }

    fn unit_vector(&mut self) -> [f64; 3] {
        loop {
            let value = [
                self.next_f64() * 2.0 - 1.0,
                self.next_f64() * 2.0 - 1.0,
                self.next_f64() * 2.0 - 1.0,
            ];
            let length_squared = dot(value, value);
            if length_squared > 0.05 && length_squared <= 1.0 {
                return normalize(value);
            }
        }
    }
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

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let magnitude = length(a);
    if magnitude > 0.0 {
        scale(a, magnitude.recip())
    } else {
        [0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DslFractalConfig, MandelboxConfig};

    fn planned(direction: FlythroughDirection) -> (FlythroughPath, MandelboxConfig) {
        let fractal = MandelboxConfig::default();
        let mut path = FlythroughPath::new(20.0, direction, 2.0e-3, 14.0, 6.5);
        path.roll_degrees = 90.0;
        path.plan(&fractal, 7)
            .expect("the Mandelbox has open routes");
        (path, fractal)
    }

    #[test]
    fn walk_stays_in_open_space_and_spans_the_requested_scales() {
        let (path, fractal) = planned(FlythroughDirection::Out);
        let summary = path.summary().unwrap();
        assert!(summary.zoom_e_folds > 5.0, "{summary:?}");
        assert!((0.0..=1.0).contains(&summary.enclosure));

        let mut previous: Option<FlythroughSample> = None;
        for frame in 0..=400 {
            let sample = path.sample(f64::from(frame) * 0.05).unwrap();
            let clearance = fractal.distance_estimate(sample.position);
            assert!(clearance > 0.0, "frame {frame} is inside the surface");
            assert!((length(sample.forward) - 1.0).abs() < 1.0e-9);
            assert!((length(sample.up) - 1.0).abs() < 1.0e-9);
            assert!(dot(sample.forward, sample.up).abs() < 1.0e-9);
            assert!(sample.focus > 0.0 && sample.scale > 0.0);
            if let Some(previous) = previous {
                // No frame jumps farther than a few local scales.
                let step = length(sub(sample.position, previous.position));
                assert!(
                    step < 3.0 * sample.scale.max(previous.scale),
                    "frame {frame}"
                );
                assert!(dot(sample.forward, previous.forward) > 0.9, "frame {frame}");
            }
            previous = Some(sample);
        }

        let first = path.sample(0.0).unwrap();
        let last = path.sample(20.0).unwrap();
        assert!(first.scale < 0.02 && last.scale > 1.0);
        assert!(length(last.position) > 13.0);
        // Outside, the camera looks back at the shape.
        assert!(dot(last.forward, normalize(scale(last.position, -1.0))) > 0.95);
    }

    #[test]
    fn dive_is_the_pull_out_reversed_and_planning_is_deterministic() {
        let (out, _) = planned(FlythroughDirection::Out);
        let (dive, _) = planned(FlythroughDirection::In);
        for time in [0.0, 3.7, 11.0, 20.0] {
            let a = out.sample(time).unwrap();
            let b = dive.sample(20.0 - time).unwrap();
            assert!(length(sub(a.position, b.position)) < 1.0e-12);
            assert!(length(sub(a.forward, b.forward)) < 1.0e-12);
        }
    }

    #[test]
    fn rejects_invalid_parameters_and_unplanned_paths() {
        let path = FlythroughPath::new(10.0, FlythroughDirection::Out, 1.0e-3, 14.0, 6.5);
        assert!(path.validate().is_err());
        assert!(path.sample(0.0).is_err());
        for broken in [
            FlythroughPath::new(0.0, FlythroughDirection::Out, 1.0e-3, 14.0, 6.5),
            FlythroughPath::new(10.0, FlythroughDirection::Out, 1.0, 14.0, 6.5),
            FlythroughPath::new(10.0, FlythroughDirection::Out, 1.0e-3, 6.0, 6.5),
        ] {
            assert!(broken.validate_parameters().is_err());
        }
    }

    #[test]
    fn plans_through_generated_shapes() {
        let fractal = DslFractalConfig::default();
        let mut path = FlythroughPath::new(12.0, FlythroughDirection::Out, 5.0e-3, 14.0, 6.5);
        path.plan(&fractal, 3).unwrap();
        assert!(path.summary().unwrap().travel_scales > 5.0);
    }
}
