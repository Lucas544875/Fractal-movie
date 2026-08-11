use anyhow::{Result, bail};

use crate::{DistanceEstimator, HighPrecisionDistanceEstimator, Qf32, QfVec3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathTarget {
    pub point: QfVec3,
    pub view_direction: [f64; 3],
}

/// Tuning values for the reusable CPU distance-estimator target search.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetSearchConfig {
    pub bound_radius: f64,
    pub hit_epsilon: f64,
    pub max_steps: u32,
    pub attempts: u32,
    pub aim_jitter: f64,
}

impl TargetSearchConfig {
    pub fn validate(&self) -> Result<()> {
        if !self.bound_radius.is_finite() || self.bound_radius <= 0.0 {
            bail!("target search bound_radius must be finite and greater than zero");
        }
        if !self.hit_epsilon.is_finite()
            || self.hit_epsilon <= 0.0
            || self.hit_epsilon >= self.bound_radius
        {
            bail!(
                "target search hit_epsilon must be finite, greater than zero, and less than bound_radius"
            );
        }
        if self.max_steps == 0 || self.max_steps > 10_000 {
            bail!("target search max_steps must be in 1..=10000");
        }
        if self.attempts == 0 || self.attempts > 4_096 {
            bail!("target search attempts must be in 1..=4096");
        }
        if !self.aim_jitter.is_finite() || !(0.0..=1.5).contains(&self.aim_jitter) {
            bail!("target search aim_jitter must be finite and in 0.0..=1.5");
        }
        Ok(())
    }
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

    /// Casts every configured probe and keeps the strongest scenic candidate.
    /// The score favors reachable recesses, readable surface angles, and
    /// front-lit detail instead of accepting the first random hit.
    #[must_use]
    pub fn pick_best(&self, seed: u32) -> Option<PathTarget> {
        let mut random = SeededRandom::new(seed);
        let probe_radius = self.config.bound_radius * 2.4;
        let mut best: Option<(PathTarget, f64)> = None;

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
            let radius = length(point);
            if radius < self.config.bound_radius * 0.20 || radius > self.config.bound_radius * 1.4 {
                continue;
            }

            let normal_epsilon =
                (self.config.hit_epsilon * 8.0).max(self.config.bound_radius * 1.0e-6);
            let normal = self
                .surface_normal(point, normal_epsilon)
                .unwrap_or_else(|| scale(aim, -1.0));
            let facing = dot(normal, scale(aim, -1.0)).abs().clamp(0.0, 1.0);
            let readable_angle = (1.0 - (facing - 0.72).abs() / 0.72).clamp(0.0, 1.0);
            let lighting = dot(normal, self.light_direction).max(0.0);
            let recess = (1.0 - radius / (self.config.bound_radius * 1.4)).clamp(0.0, 1.0);
            let score = recess * 0.55 + readable_angle * 0.30 + lighting * 0.15;
            let target = PathTarget {
                point: QfVec3::from_f64(point),
                view_direction: aim,
            };
            if best
                .as_ref()
                .is_none_or(|(_, best_score)| score > *best_score)
            {
                best = Some((target, score));
            }
        }

        best.map(|(target, _)| target)
    }

    /// Estimates the outward local surface normal from the CPU DE. This is
    /// reusable by camera planners that need a local tangent plane.
    #[must_use]
    pub fn surface_normal(&self, point: [f64; 3], epsilon: f64) -> Option<[f64; 3]> {
        if !epsilon.is_finite() || epsilon <= 0.0 {
            return None;
        }
        let sample = |offset: [f64; 3]| self.estimator.distance_estimate(add(point, offset));
        let gradient = [
            sample([epsilon, 0.0, 0.0]) - sample([-epsilon, 0.0, 0.0]),
            sample([0.0, epsilon, 0.0]) - sample([0.0, -epsilon, 0.0]),
            sample([0.0, 0.0, epsilon]) - sample([0.0, 0.0, -epsilon]),
        ];
        let magnitude = length(gradient);
        (magnitude.is_finite() && magnitude > 1.0e-15).then(|| scale(gradient, magnitude.recip()))
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
    pub overview_distance: Qf32,
    pub minimum_distance: Qf32,
    pub overview_duration: f64,
    pub dive_duration: f64,
}

impl ExponentialDivePath {
    pub fn validate(&self) -> Result<()> {
        if !self.overview_distance.is_finite() || self.overview_distance <= Qf32::ZERO {
            bail!("overview_distance must be finite and greater than zero");
        }
        if !self.minimum_distance.is_finite() || self.minimum_distance <= Qf32::ZERO {
            bail!("minimum_distance must be finite and greater than zero");
        }
        if self.minimum_distance > self.overview_distance {
            bail!("minimum_distance must not exceed overview_distance");
        }
        if !self.overview_duration.is_finite() || self.overview_duration < 0.0 {
            bail!("overview_duration must be finite and non-negative");
        }
        if !self.dive_duration.is_finite() || self.dive_duration <= 0.0 {
            bail!("dive_duration must be finite and greater than zero");
        }
        Ok(())
    }

    #[must_use]
    pub fn distance_at(&self, time_seconds: f64) -> f64 {
        self.distance_qf_at(time_seconds).to_f64()
    }

    /// High-precision coordinate distance for camera/path composition.
    /// Exact path endpoints retain every quad-float limb; only the smooth
    /// transcendental interpolation factor is evaluated as `f64`.
    #[must_use]
    pub fn distance_qf_at(&self, time_seconds: f64) -> Qf32 {
        if time_seconds <= self.overview_duration {
            return self.overview_distance;
        }
        let progress =
            ((time_seconds - self.overview_duration) / self.dive_duration).clamp(0.0, 1.0);
        if progress >= 1.0 {
            return self.minimum_distance;
        }
        let overview = self.overview_distance.to_f64();
        let minimum = self.minimum_distance.to_f64();
        Qf32::from_f64(overview * (minimum / overview).powf(progress))
    }
}

/// A constant-radius orbit that keeps the base camera target centered.
///
/// Camera positions lie on the intersection of a sphere and a cone around
/// `axis`. A 90-degree cone angle produces a great circle; other angles
/// produce small circles whose camera-to-target sight lines sweep a cone.
#[derive(Clone, Copy, Debug)]
pub struct TargetOrbitPath {
    pub radius: Qf32,
    pub duration: f64,
    pub revolutions: f64,
    pub axis: [f64; 3],
    pub cone_angle_degrees: f64,
    pub start_angle_degrees: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TargetOrbitSample {
    pub position: QfVec3,
    pub up: [f32; 3],
    pub camera_distance: Qf32,
}

impl TargetOrbitPath {
    pub fn validate(&self) -> Result<()> {
        if !self.radius.is_finite() || self.radius <= Qf32::ZERO {
            bail!("target orbit radius must be finite and greater than zero");
        }
        if !self.duration.is_finite() || self.duration <= 0.0 {
            bail!("target orbit duration must be finite and greater than zero");
        }
        if !self.revolutions.is_finite() || self.revolutions == 0.0 {
            bail!("target orbit revolutions must be finite and non-zero");
        }
        let axis_length = length(self.axis);
        if self.axis.iter().any(|component| !component.is_finite())
            || !axis_length.is_finite()
            || axis_length < 1.0e-12
        {
            bail!("target orbit axis must be finite and non-zero");
        }
        if !self.cone_angle_degrees.is_finite()
            || !(0.0..180.0).contains(&self.cone_angle_degrees)
            || self.cone_angle_degrees == 0.0
        {
            bail!(
                "target orbit cone_angle_degrees must be finite and strictly between 0.0 and 180.0"
            );
        }
        if !self.start_angle_degrees.is_finite() {
            bail!("target orbit start_angle_degrees must be finite");
        }
        Ok(())
    }

    pub(crate) fn sample(
        &self,
        target: QfVec3,
        reference_position: QfVec3,
        time_seconds: f64,
    ) -> Result<TargetOrbitSample> {
        self.validate()?;
        if !target.is_finite() || !reference_position.is_finite() {
            bail!("target orbit requires finite target and reference camera coordinates");
        }

        let axis = normalize(self.axis);
        let reference_direction = (reference_position - target)
            .normalized_to_f32()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "target orbit reference camera position must differ from the target"
                )
            })?
            .map(f64::from);
        let projected_reference = add(
            reference_direction,
            scale(axis, -dot(reference_direction, axis)),
        );
        let basis_u = if length(projected_reference) >= 1.0e-12 {
            normalize(projected_reference)
        } else {
            perpendicular(axis)
        };
        let basis_v = normalize(cross(axis, basis_u));

        let progress = (time_seconds / self.duration).clamp(0.0, 1.0);
        let completed_turns = (self.revolutions * progress).rem_euclid(1.0);
        let azimuth = self
            .start_angle_degrees
            .to_radians()
            .rem_euclid(std::f64::consts::TAU)
            + std::f64::consts::TAU * completed_turns;
        let cone_angle = self.cone_angle_degrees.to_radians();
        let ring_direction = add(scale(basis_u, azimuth.cos()), scale(basis_v, azimuth.sin()));
        let radial_direction = normalize(add(
            scale(axis, cone_angle.cos()),
            scale(ring_direction, cone_angle.sin()),
        ));
        let up = normalize(add(
            axis,
            scale(radial_direction, -dot(axis, radial_direction)),
        ));

        Ok(TargetOrbitSample {
            position: target + QfVec3::from_f64(radial_direction) * self.radius,
            up: up.map(|component| component as f32),
            camera_distance: self.radius,
        })
    }
}

/// Repeatedly dives toward independently selected surface features. Targets
/// are planned once from the CPU DE, then reused for every animation frame.
#[derive(Clone, Debug)]
pub struct MultiTargetDivePath {
    pub overview_distance: Qf32,
    pub minimum_distance: Qf32,
    pub overview_duration: f64,
    pub dive_duration: f64,
    pub transition_duration: f64,
    pub search: TargetSearchConfig,
    pub(crate) targets: Vec<PathTarget>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MultiTargetDiveSample {
    pub target: PathTarget,
    pub distance: Qf32,
    pub fade_to_black: f32,
}

impl MultiTargetDivePath {
    #[must_use]
    pub fn new(
        overview_distance: Qf32,
        minimum_distance: Qf32,
        overview_duration: f64,
        dive_duration: f64,
        transition_duration: f64,
        search: TargetSearchConfig,
    ) -> Self {
        Self {
            overview_distance,
            minimum_distance,
            overview_duration,
            dive_duration,
            transition_duration,
            search,
            targets: Vec::new(),
        }
    }

    pub(crate) fn validate_parameters(&self) -> Result<()> {
        ExponentialDivePath {
            overview_distance: self.overview_distance,
            minimum_distance: self.minimum_distance,
            overview_duration: self.overview_duration,
            dive_duration: self.dive_duration,
        }
        .validate()?;
        if !self.transition_duration.is_finite() || self.transition_duration <= 0.0 {
            bail!("transition_duration must be finite and greater than zero");
        }
        self.search.validate()?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_parameters()?;
        if self.targets.is_empty() {
            bail!("multi-target dive has not planned any targets");
        }
        if self.targets.iter().any(|target| {
            !target.point.is_finite()
                || !target.view_direction.iter().all(|value| value.is_finite())
                || (length(target.view_direction) - 1.0).abs() > 1.0e-8
        }) {
            bail!("multi-target dive contains an invalid planned target");
        }
        Ok(())
    }

    #[must_use]
    pub fn target_count(&self) -> usize {
        self.targets.len()
    }

    #[must_use]
    pub fn cycle_duration(&self) -> f64 {
        self.overview_duration + self.dive_duration + self.transition_duration
    }

    pub(crate) fn plan<D: DistanceEstimator>(
        &mut self,
        estimator: &D,
        light_direction: [f64; 3],
        seed: u32,
        target_count: usize,
    ) -> Result<()> {
        self.search.validate()?;
        if target_count == 0 {
            bail!("multi-target dive requires at least one planned target");
        }
        let picker = TargetPicker::new(estimator, self.search, light_direction);
        let mut targets = Vec::with_capacity(target_count);
        for index in 0..target_count {
            let target_seed =
                seed.wrapping_add((index as u32).wrapping_add(1).wrapping_mul(0x9e37_79b9));
            let target = picker.pick_best(target_seed).ok_or_else(|| {
                anyhow::anyhow!(
                    "target search found no usable surface for dive target {}",
                    index + 1
                )
            })?;
            targets.push(target);
        }
        self.targets = targets;
        Ok(())
    }

    pub(crate) fn sample(&self, time_seconds: f64) -> Result<MultiTargetDiveSample> {
        self.validate()?;
        let cycle_duration = self.cycle_duration();
        let cycle = (time_seconds.max(0.0) / cycle_duration).floor() as usize;
        let local_time = time_seconds.max(0.0) - cycle as f64 * cycle_duration;
        let current_index = cycle.min(self.targets.len() - 1);
        let dive_end = self.overview_duration + self.dive_duration;

        if local_time < dive_end {
            let curve = ExponentialDivePath {
                overview_distance: self.overview_distance,
                minimum_distance: self.minimum_distance,
                overview_duration: self.overview_duration,
                dive_duration: self.dive_duration,
            };
            return Ok(MultiTargetDiveSample {
                target: self.targets[current_index],
                distance: curve.distance_qf_at(local_time),
                fade_to_black: 0.0,
            });
        }

        let transition = ((local_time - dive_end) / self.transition_duration).clamp(0.0, 1.0);
        let fade = if transition < 0.5 {
            smoothstep(transition * 2.0)
        } else {
            smoothstep((1.0 - transition) * 2.0)
        };
        let use_next_target = transition >= 0.5;
        let target_index = if use_next_target {
            (current_index + 1).min(self.targets.len() - 1)
        } else {
            current_index
        };
        Ok(MultiTargetDiveSample {
            target: self.targets[target_index],
            distance: if use_next_target {
                self.overview_distance
            } else {
                self.minimum_distance
            },
            fade_to_black: fade as f32,
        })
    }
}

/// A top-down camera move in the tangent plane of an automatically selected
/// surface. This is intended for broad, approximately planar fractal features.
#[derive(Clone, Debug)]
pub struct SurfaceFlyoverPath {
    pub camera_height: f64,
    pub travel_distance: f64,
    pub duration: f64,
    pub look_ahead: f64,
    pub travel_direction: [f64; 3],
    pub normal_epsilon: f64,
    pub search: TargetSearchConfig,
    pub(crate) anchor: Option<PathTarget>,
    pub(crate) normal: [f64; 3],
    pub(crate) tangent: [f64; 3],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SurfaceFlyoverSample {
    pub position: QfVec3,
    pub target: QfVec3,
    pub up: [f32; 3],
    pub camera_distance: Qf32,
}

impl SurfaceFlyoverPath {
    #[must_use]
    pub fn new(
        camera_height: f64,
        travel_distance: f64,
        duration: f64,
        look_ahead: f64,
        travel_direction: [f64; 3],
        normal_epsilon: f64,
        search: TargetSearchConfig,
    ) -> Self {
        Self {
            camera_height,
            travel_distance,
            duration,
            look_ahead,
            travel_direction,
            normal_epsilon,
            search,
            anchor: None,
            normal: [0.0, 0.0, 1.0],
            tangent: [1.0, 0.0, 0.0],
        }
    }

    pub(crate) fn validate_parameters(&self) -> Result<()> {
        if !self.camera_height.is_finite() || self.camera_height <= 0.0 {
            bail!("surface flyover camera_height must be finite and greater than zero");
        }
        if !self.travel_distance.is_finite() || self.travel_distance <= 0.0 {
            bail!("surface flyover travel_distance must be finite and greater than zero");
        }
        if !self.duration.is_finite() || self.duration <= 0.0 {
            bail!("surface flyover duration must be finite and greater than zero");
        }
        if !self.look_ahead.is_finite() || self.look_ahead < 0.0 {
            bail!("surface flyover look_ahead must be finite and non-negative");
        }
        if !self.normal_epsilon.is_finite() || self.normal_epsilon <= 0.0 {
            bail!("surface flyover normal_epsilon must be finite and greater than zero");
        }
        if self
            .travel_direction
            .iter()
            .any(|component| !component.is_finite())
            || length(self.travel_direction) < 1.0e-12
        {
            bail!("surface flyover travel_direction must be finite and non-zero");
        }
        self.search.validate()?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_parameters()?;
        let Some(anchor) = self.anchor else {
            bail!("surface flyover has not planned a surface anchor");
        };
        if !anchor.point.is_finite()
            || self.normal.iter().any(|value| !value.is_finite())
            || self.tangent.iter().any(|value| !value.is_finite())
            || (length(self.normal) - 1.0).abs() > 1.0e-8
            || (length(self.tangent) - 1.0).abs() > 1.0e-8
            || dot(self.normal, self.tangent).abs() > 1.0e-8
        {
            bail!("surface flyover contains an invalid planned tangent plane");
        }
        Ok(())
    }

    pub(crate) fn plan<D: DistanceEstimator>(
        &mut self,
        estimator: &D,
        light_direction: [f64; 3],
        seed: u32,
    ) -> Result<()> {
        self.search.validate()?;
        let picker = TargetPicker::new(estimator, self.search, light_direction);
        let anchor = picker
            .pick_best(seed.wrapping_add(0x517c_c1b7))
            .ok_or_else(|| anyhow::anyhow!("target search found no usable flyover surface"))?;
        let point = anchor.point.to_f64();
        let mut normal = picker
            .surface_normal(point, self.normal_epsilon)
            .unwrap_or_else(|| scale(anchor.view_direction, -1.0));
        if dot(normal, anchor.view_direction) > 0.0 {
            normal = scale(normal, -1.0);
        }
        let direction = normalize(self.travel_direction);
        let mut tangent = add(direction, scale(normal, -dot(direction, normal)));
        if length(tangent) < 1.0e-10 {
            let fallback_axis = if normal[2].abs() < 0.9 {
                [0.0, 0.0, 1.0]
            } else {
                [0.0, 1.0, 0.0]
            };
            tangent = cross(normal, fallback_axis);
        }
        tangent = normalize(tangent);
        let bitangent = normalize(cross(normal, tangent));
        let probe_offset = self.camera_height.max(self.normal_epsilon * 64.0);
        let probe_distance = probe_offset * 3.0;
        let sample_count = 12_u32;
        let mut best_tangent = tangent;
        let mut best_score = f64::NEG_INFINITY;
        let mut best_visible_samples = 0_u32;
        for direction_index in 0..16_u32 {
            let angle = f64::from(direction_index) * std::f64::consts::TAU / 16.0;
            let candidate_tangent = normalize(add(
                scale(tangent, angle.cos()),
                scale(bitangent, angle.sin()),
            ));
            let mut visible_samples = 0_u32;
            for sample_index in 1..=sample_count {
                let distance = (self.travel_distance + self.look_ahead) * f64::from(sample_index)
                    / f64::from(sample_count);
                let plane_point = add(point, scale(candidate_tangent, distance));
                let probe_origin = add(plane_point, scale(normal, probe_offset));
                let Some(hit) = picker.raymarch(probe_origin, scale(normal, -1.0), probe_distance)
                else {
                    continue;
                };
                let hit_normal = picker
                    .surface_normal(hit, self.normal_epsilon)
                    .unwrap_or(normal);
                if dot(normal, hit_normal).abs() >= 0.55 {
                    visible_samples += 1;
                }
            }
            let direction_preference = dot(candidate_tangent, tangent);
            let score = f64::from(visible_samples) + direction_preference * 0.05;
            if score > best_score {
                best_score = score;
                best_visible_samples = visible_samples;
                best_tangent = candidate_tangent;
            }
        }
        if best_visible_samples < sample_count / 2 {
            bail!(
                "selected flyover surface stays visible for only {best_visible_samples}/{sample_count} path probes; reduce travel_distance or choose another seed"
            );
        }
        self.anchor = Some(anchor);
        self.normal = normalize(normal);
        self.tangent = best_tangent;
        Ok(())
    }

    pub(crate) fn sample(&self, time_seconds: f64) -> Result<SurfaceFlyoverSample> {
        self.validate()?;
        let anchor = self.anchor.expect("validated flyover anchor");
        let progress = smoothstep((time_seconds / self.duration).clamp(0.0, 1.0));
        let surface = add(
            anchor.point.to_f64(),
            scale(self.tangent, self.travel_distance * progress),
        );
        let position = add(surface, scale(self.normal, self.camera_height));
        let target = add(surface, scale(self.tangent, self.look_ahead));
        let camera_distance = length(add(target, scale(position, -1.0)));
        Ok(SurfaceFlyoverSample {
            position: QfVec3::from_f64(position),
            target: QfVec3::from_f64(target),
            up: self.tangent.map(|value| value as f32),
            camera_distance: Qf32::from_f64(camera_distance),
        })
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

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
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

fn perpendicular(axis: [f64; 3]) -> [f64; 3] {
    let cardinal = if axis[0].abs() <= axis[1].abs() && axis[0].abs() <= axis[2].abs() {
        [1.0, 0.0, 0.0]
    } else if axis[1].abs() <= axis[2].abs() {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    normalize(cross(axis, cardinal))
}

fn smoothstep(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
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
    fn best_target_search_scores_all_rays_deterministically() {
        let fractal = MandelboxConfig::default();
        let picker = TargetPicker::new(
            &fractal,
            TargetSearchConfig {
                bound_radius: fractal.bound_radius,
                hit_epsilon: 1.0e-6,
                max_steps: 800,
                attempts: 96,
                aim_jitter: 0.25,
            },
            [2.0, 1.0, 1.0],
        );
        let first = picker
            .pick_best(81_311)
            .expect("the reference Mandelbox must offer a scenic target");
        let second = picker
            .pick_best(81_311)
            .expect("the same search must remain reproducible");
        assert_eq!(first, second);
        assert!(fractal.distance_estimate(first.point.to_f64()) < 1.0e-6);
    }

    #[test]
    fn multi_target_dive_plans_once_and_fades_between_targets() {
        let fractal = MandelboxConfig::default();
        let mut path = MultiTargetDivePath::new(
            Qf32::from_f32(8.0),
            Qf32::from_f32(0.003),
            1.0,
            4.0,
            1.0,
            TargetSearchConfig {
                bound_radius: fractal.bound_radius,
                hit_epsilon: 1.0e-6,
                max_steps: 800,
                attempts: 64,
                aim_jitter: 0.25,
            },
        );
        path.plan(&fractal, [2.0, 1.0, 1.0], 12_345, 3)
            .expect("targets must be planned");
        path.validate().unwrap();
        assert_eq!(path.target_count(), 3);

        let first = path.sample(0.0).unwrap();
        let black = path.sample(5.5).unwrap();
        let second = path.sample(6.0).unwrap();
        assert_eq!(first.distance, Qf32::from_f32(8.0));
        assert_eq!(black.fade_to_black, 1.0);
        assert_ne!(first.target, second.target);
        assert_eq!(second.distance, Qf32::from_f32(8.0));
    }

    #[test]
    fn surface_flyover_moves_in_the_planned_tangent_plane() {
        let fractal = MandelboxConfig::default();
        let mut path = SurfaceFlyoverPath::new(
            0.8,
            2.5,
            8.0,
            0.25,
            [0.0, 0.0, 1.0],
            2.0e-4,
            TargetSearchConfig {
                bound_radius: fractal.bound_radius,
                hit_epsilon: 1.0e-6,
                max_steps: 800,
                attempts: 64,
                aim_jitter: 0.25,
            },
        );
        path.plan(&fractal, [2.0, 1.0, 1.0], 45_678)
            .expect("flyover plane must be planned");
        path.validate().unwrap();

        let start = path.sample(0.0).unwrap();
        let end = path.sample(8.0).unwrap();
        let displacement = add(end.position.to_f64(), scale(start.position.to_f64(), -1.0));
        assert!((length(displacement) - 2.5).abs() < 1.0e-8);
        assert!(dot(displacement, path.normal).abs() < 1.0e-8);
        assert!((dot(path.normal, path.tangent)).abs() < 1.0e-10);
    }

    #[test]
    fn target_orbit_supports_great_and_small_circles() {
        let target = QfVec3::from_f64([1.0, 2.0, 3.0]);
        let reference = QfVec3::from_f64([5.0, 2.0, 3.0]);
        let great_circle = TargetOrbitPath {
            radius: Qf32::from_f64(4.0),
            duration: 8.0,
            revolutions: 1.0,
            axis: [0.0, 0.0, 1.0],
            cone_angle_degrees: 90.0,
            start_angle_degrees: 0.0,
        };
        let start = great_circle.sample(target, reference, 0.0).unwrap();
        let quarter = great_circle.sample(target, reference, 2.0).unwrap();
        let start_offset = add(start.position.to_f64(), scale(target.to_f64(), -1.0));
        assert!((length(start_offset) - 4.0).abs() < 1.0e-12);
        assert!((start_offset[0] - 4.0).abs() < 1.0e-12);
        assert!(start_offset[1].abs() < 1.0e-12);
        assert!(start_offset[2].abs() < 1.0e-12);
        let quarter_offset = add(quarter.position.to_f64(), scale(target.to_f64(), -1.0));
        assert!(quarter_offset[0].abs() < 1.0e-12);
        assert!((quarter_offset[1] - 4.0).abs() < 1.0e-12);
        assert!(quarter_offset[2].abs() < 1.0e-12);

        let small_circle = TargetOrbitPath {
            cone_angle_degrees: 30.0,
            ..great_circle
        };
        for time in [0.0, 2.0, 4.0, 6.0, 8.0] {
            let sample = small_circle.sample(target, reference, time).unwrap();
            let offset = add(sample.position.to_f64(), scale(target.to_f64(), -1.0));
            assert!((length(offset) - 4.0).abs() < 1.0e-12);
            assert!(
                (dot(normalize(offset), [0.0, 0.0, 1.0]) - 30_f64.to_radians().cos()).abs()
                    < 1.0e-12
            );
            assert!(dot(sample.up.map(f64::from), normalize(offset)).abs() < 1.0e-6);
        }
    }

    #[test]
    fn target_orbit_rejects_degenerate_parameters() {
        let mut path = TargetOrbitPath {
            radius: Qf32::ONE,
            duration: 1.0,
            revolutions: 1.0,
            axis: [0.0, 0.0, 1.0],
            cone_angle_degrees: 45.0,
            start_angle_degrees: 0.0,
        };
        path.axis = [0.0; 3];
        assert!(path.validate().is_err());
        path.axis = [f64::MAX; 3];
        assert!(path.validate().is_err());
        path.axis = [0.0, 0.0, 1.0];
        path.cone_angle_degrees = 0.0;
        assert!(path.validate().is_err());
        path.cone_angle_degrees = 180.0;
        assert!(path.validate().is_err());
    }

    #[test]
    fn dive_distance_uses_exponential_interpolation() {
        let path = ExponentialDivePath {
            overview_distance: Qf32::from_f32(10.0),
            minimum_distance: Qf32::from_f32(0.1),
            overview_duration: 2.0,
            dive_duration: 4.0,
        };
        path.validate().unwrap();
        assert_eq!(path.distance_at(1.0), 10.0);
        assert!((path.distance_at(4.0) - 1.0).abs() < 1.0e-7);
        assert!((path.distance_at(10.0) - 0.1).abs() < 1.0e-7);
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
