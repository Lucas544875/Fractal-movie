use crate::{DistanceEstimator, HighPrecisionDistanceEstimator, Qf32, QfVec3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathTarget {
    pub point: QfVec3,
    pub view_direction: [f64; 3],
}

/// Tuning values for the reusable CPU distance-estimator target search.
#[derive(Clone, Copy, Debug)]
pub struct TargetSearchConfig {
    pub bound_radius: f64,
    pub hit_epsilon: f64,
    pub max_steps: u32,
    pub attempts: u32,
    pub aim_jitter: f64,
}

/// Selects camera path targets by probing a distance estimator from outside
/// its bounding sphere. This is the Rust counterpart of `pickTarget()` and
/// `pickOriginGapDir()` in the portfolio Mandelbox page.
pub struct TargetPicker<'a, D> {
    estimator: &'a D,
    config: TargetSearchConfig,
    light_direction: [f64; 3],
}

impl<'a, D: DistanceEstimator> TargetPicker<'a, D> {
    #[must_use]
    pub fn new(estimator: &'a D, config: TargetSearchConfig, light_direction: [f64; 3]) -> Self {
        Self {
            estimator,
            config,
            light_direction: normalize(light_direction),
        }
    }

    /// Searches around a preferred approach direction and returns the hit
    /// closest to the fractal origin, matching the page's opening shot logic.
    #[must_use]
    pub fn pick_origin_gap(&self, seed: u32, approach_direction: [f64; 3]) -> Option<PathTarget> {
        let mut random = SeededRandom::new(seed);
        let probe_radius = self.config.bound_radius * 2.4;
        let base_direction = normalize(approach_direction);
        let mut best: Option<(PathTarget, f64)> = None;

        for _ in 0..self.config.attempts {
            let jitter = random.unit_vector();
            let outward = normalize(add(base_direction, scale(jitter, self.config.aim_jitter)));
            let probe_origin = scale(outward, probe_radius);
            let aim = scale(outward, -1.0);
            if dot(self.light_direction, aim) >= 0.0 {
                continue;
            }
            let Some(point) = self.raymarch(probe_origin, aim, probe_radius * 2.5) else {
                continue;
            };
            let distance_from_origin = length(point);
            if best
                .as_ref()
                .is_none_or(|(_, best_distance)| distance_from_origin < *best_distance)
            {
                best = Some((
                    PathTarget {
                        point: QfVec3::from_f64(point),
                        view_direction: aim,
                    },
                    distance_from_origin,
                ));
            }
        }

        best.map(|(target, _)| target)
    }

    /// Selects a general target from a random exterior direction.
    #[must_use]
    pub fn pick_random(&self, seed: u32) -> Option<PathTarget> {
        let mut random = SeededRandom::new(seed);
        let probe_radius = self.config.bound_radius * 2.4;
        for _ in 0..self.config.attempts {
            let outward = random.unit_vector();
            let probe_origin = scale(outward, probe_radius);
            let aim = normalize(add(
                scale(outward, -1.0),
                scale(random.unit_vector(), self.config.aim_jitter),
            ));
            if dot(self.light_direction, aim) >= 0.0 {
                continue;
            }
            let Some(point) = self.raymarch(probe_origin, aim, probe_radius * 2.5) else {
                continue;
            };
            let distance_from_origin = length(point);
            if distance_from_origin >= self.config.bound_radius * 0.25
                && distance_from_origin <= self.config.bound_radius * 1.4
            {
                return Some(PathTarget {
                    point: QfVec3::from_f64(point),
                    view_direction: aim,
                });
            }
        }
        None
    }

    fn raymarch(
        &self,
        ray_origin: [f64; 3],
        ray_direction: [f64; 3],
        max_distance: f64,
    ) -> Option<[f64; 3]> {
        let mut travel = 0.0;
        for _ in 0..self.config.max_steps {
            let point = add(ray_origin, scale(ray_direction, travel));
            let distance = self.estimator.distance_estimate(point);
            if !distance.is_finite() {
                return None;
            }
            if distance < self.config.hit_epsilon {
                return Some(point);
            }
            travel += distance;
            if travel > max_distance {
                return None;
            }
        }
        None
    }
}

impl<D: DistanceEstimator + HighPrecisionDistanceEstimator> TargetPicker<'_, D> {
    /// Continues sphere tracing from an already located exterior target using
    /// quad-float coordinates. The direction remains normalized `f64`; its
    /// product with each quad-float step is accumulated without rounding the
    /// absolute point back to `f64` or `f32`.
    #[must_use]
    pub fn refine(
        &self,
        target: PathTarget,
        hit_epsilon: Qf32,
        max_steps: u32,
    ) -> Option<PathTarget> {
        if hit_epsilon <= Qf32::ZERO || !hit_epsilon.is_finite() {
            return None;
        }
        let direction = QfVec3::from_f64(target.view_direction);
        // The coarse f64 march stops within its epsilon and can be just inside
        // the unsigned Mandelbox DE. Step back along the incoming ray before
        // continuing so refinement always approaches from the exterior.
        let retreat = Qf32::from_f64((self.config.hit_epsilon * 32.0).max(1.0e-5));
        let mut point = target.point - direction * retreat;
        for _ in 0..max_steps {
            let distance = self.estimator.distance_estimate_qf(point);
            if !distance.is_finite() || distance < Qf32::ZERO {
                return None;
            }
            if distance < hit_epsilon {
                return Some(PathTarget { point, ..target });
            }
            point = point + direction * (distance * Qf32::from_f32(0.9));
        }
        None
    }
}

/// Reusable overview-then-exponential-dive timing curve from the portfolio.
#[derive(Clone, Copy, Debug)]
pub struct ExponentialDivePath {
    pub overview_distance: f64,
    pub minimum_distance: f64,
    pub overview_duration: f64,
    pub dive_duration: f64,
}

impl ExponentialDivePath {
    #[must_use]
    pub fn distance_at(&self, time_seconds: f64) -> f64 {
        if time_seconds <= self.overview_duration {
            return self.overview_distance;
        }
        let progress =
            ((time_seconds - self.overview_duration) / self.dive_duration).clamp(0.0, 1.0);
        self.overview_distance * (self.minimum_distance / self.overview_distance).powf(progress)
    }

    /// High-precision coordinate distance for camera/path composition.
    #[must_use]
    pub fn distance_qf_at(&self, time_seconds: f64) -> Qf32 {
        Qf32::from_f64(self.distance_at(time_seconds))
    }
}

struct SeededRandom {
    state: u64,
}

impl SeededRandom {
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
        for _ in 0..20 {
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
        [0.0, 1.0, 0.0]
    }
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(value: [f64; 3], factor: f64) -> [f64; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(value: [f64; 3]) -> f64 {
    dot(value, value).sqrt()
}

fn normalize(value: [f64; 3]) -> [f64; 3] {
    let magnitude = length(value);
    if !magnitude.is_finite() || magnitude < 1.0e-15 {
        return [0.0, 0.0, -1.0];
    }
    scale(value, 1.0 / magnitude)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MandelboxConfig;

    #[test]
    fn target_search_is_deterministic_and_hits_the_surface() {
        let fractal = MandelboxConfig::default();
        let picker = TargetPicker::new(
            &fractal,
            TargetSearchConfig {
                bound_radius: fractal.bound_radius,
                hit_epsilon: 1.0e-6,
                max_steps: 800,
                attempts: 96,
                aim_jitter: 0.35,
            },
            [2.0, 1.0, 1.0],
        );
        let first = picker
            .pick_origin_gap(12_345, [1.0, 0.0, 0.0])
            .expect("the reference Mandelbox must have a reachable opening");
        let second = picker
            .pick_origin_gap(12_345, [1.0, 0.0, 0.0])
            .expect("the same seed must remain reachable");
        assert_eq!(first, second);
        assert!(fractal.distance_estimate(first.point.to_f64()) < 1.0e-6);
        assert!((length(first.view_direction) - 1.0).abs() < 1.0e-12);
    }

    #[test]
    fn dive_distance_uses_exponential_interpolation() {
        let path = ExponentialDivePath {
            overview_distance: 10.0,
            minimum_distance: 0.1,
            overview_duration: 2.0,
            dive_duration: 4.0,
        };
        assert_eq!(path.distance_at(1.0), 10.0);
        assert!((path.distance_at(4.0) - 1.0).abs() < 1.0e-12);
        assert!((path.distance_at(10.0) - 0.1).abs() < 1.0e-12);
    }

    #[test]
    fn quad_refinement_reaches_below_f64_target_epsilon() {
        let fractal = MandelboxConfig::default();
        let picker = TargetPicker::new(
            &fractal,
            TargetSearchConfig {
                bound_radius: fractal.bound_radius,
                hit_epsilon: 1.0e-8,
                max_steps: 1_200,
                attempts: 96,
                aim_jitter: 0.35,
            },
            [2.0, 1.0, 1.0],
        );
        let target = PathTarget {
            point: QfVec3::from_f64([10.0, 0.0, 0.0]),
            view_direction: [-1.0, 0.0, 0.0],
        };
        let epsilon = Qf32::from_f64(1.0e-20);
        let refined = picker
            .refine(target, epsilon, 512)
            .expect("quad target must converge");
        assert!(fractal.distance_estimate_qf(refined.point) < epsilon);
    }
}
