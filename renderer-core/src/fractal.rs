use crate::{DslFractalConfig, MandelboxConfig, OrbitTransform, Qf32, QfVec3};

/// CPU-side counterpart of a shader distance estimator.
///
/// Camera target search is generic over this contract, so future fractals can
/// reuse the portfolio's path-selection algorithm without depending on WGSL.
pub trait DistanceEstimator {
    fn distance_estimate(&self, point: [f64; 3]) -> f64;
}

/// High-precision counterpart used to refine path targets beyond `f64`
/// coordinate resolution.
pub trait HighPrecisionDistanceEstimator {
    fn distance_estimate_qf(&self, point: QfVec3) -> Qf32;
}

impl DistanceEstimator for MandelboxConfig {
    fn distance_estimate(&self, point: [f64; 3]) -> f64 {
        let mut z = point;
        let mut derivative = 1.0_f64;
        let scale = f64::from(self.scale);
        let min_radius_squared = f64::from(self.min_radius_squared);
        let fixed_radius_squared = f64::from(self.fixed_radius_squared);
        let fold_limit = f64::from(self.fold_limit);

        for _ in 0..self.iterations {
            for component in &mut z {
                *component = component.clamp(-fold_limit, fold_limit) * 2.0 - *component;
            }
            let radius_squared = dot(z, z);
            if radius_squared < min_radius_squared {
                let factor = fixed_radius_squared / min_radius_squared;
                z = scale_vector(z, factor);
                derivative *= factor;
            } else if radius_squared < fixed_radius_squared {
                let factor = fixed_radius_squared / radius_squared;
                z = scale_vector(z, factor);
                derivative *= factor;
            }
            z = add(scale_vector(z, scale), point);
            derivative = derivative * scale.abs() + 1.0;
        }

        dot(z, z).sqrt() / derivative.abs()
    }
}

impl DistanceEstimator for DslFractalConfig {
    fn distance_estimate(&self, point: [f64; 3]) -> f64 {
        let mut z = point;
        let mut derivative = 1.0_f64;

        for iteration in 0..self.iterations {
            let scheduled_iteration = self
                .orbit_period
                .map_or(iteration, |period| iteration % period);
            for transform in &self.orbit {
                match transform {
                    OrbitTransform::AmazingSurfFold {
                        start_iteration,
                        stop_iteration,
                        limits,
                        minimum_radius_squared,
                        scale,
                        rotation_degrees,
                    } => {
                        if scheduled_iteration < *start_iteration
                            || scheduled_iteration >= *stop_iteration
                        {
                            continue;
                        }
                        for axis in 0..2 {
                            let limit = f64::from(limits[axis]);
                            z[axis] = (z[axis] + limit).abs() - (z[axis] - limit).abs() - z[axis];
                        }
                        let divisor =
                            dot(z, z).clamp(f64::from(*minimum_radius_squared).max(1.0e-24), 1.0);
                        let multiplier = f64::from(*scale) / divisor;
                        z = scale_vector(z, multiplier);
                        derivative = derivative * multiplier.abs() + 1.0;
                        for (axis, degrees) in [
                            ([1.0, 0.0, 0.0], rotation_degrees[0]),
                            ([0.0, 1.0, 0.0], rotation_degrees[1]),
                            ([0.0, 0.0, 1.0], rotation_degrees[2]),
                        ] {
                            z = rotate(z, axis, f64::from(degrees).to_radians());
                        }
                    }
                    OrbitTransform::MandelboxJuliaFold {
                        start_iteration,
                        stop_iteration,
                        fold_limit,
                        min_radius_squared,
                        fixed_radius_squared,
                        scale,
                        constant,
                        rotation_degrees,
                    } => {
                        if scheduled_iteration < *start_iteration
                            || scheduled_iteration >= *stop_iteration
                        {
                            continue;
                        }
                        let limit = f64::from(*fold_limit);
                        for component in &mut z {
                            *component = component.clamp(-limit, limit) * 2.0 - *component;
                        }
                        let radius_squared = dot(z, z);
                        let minimum = f64::from(*min_radius_squared);
                        let fixed = f64::from(*fixed_radius_squared);
                        if radius_squared < minimum {
                            let factor = fixed / minimum;
                            z = scale_vector(z, factor);
                            derivative *= factor;
                        } else if radius_squared < fixed {
                            let factor = fixed / radius_squared.max(1.0e-24);
                            z = scale_vector(z, factor);
                            derivative *= factor;
                        }
                        for (axis, degrees) in [
                            ([1.0, 0.0, 0.0], rotation_degrees[0]),
                            ([0.0, 1.0, 0.0], rotation_degrees[1]),
                            ([0.0, 0.0, 1.0], rotation_degrees[2]),
                        ] {
                            z = rotate(z, axis, f64::from(degrees).to_radians());
                        }
                        let scale = f64::from(*scale);
                        z = add(scale_vector(z, scale), constant.map(f64::from));
                        derivative = derivative * scale.abs() + 1.0;
                    }
                    OrbitTransform::BoxFold { limit } => {
                        let limit = f64::from(*limit);
                        for component in &mut z {
                            *component = component.clamp(-limit, limit) * 2.0 - *component;
                        }
                    }
                    OrbitTransform::SphereFold {
                        min_radius_squared,
                        fixed_radius_squared,
                    } => {
                        let minimum = f64::from(*min_radius_squared);
                        let fixed = f64::from(*fixed_radius_squared);
                        let radius_squared = dot(z, z);
                        if radius_squared < minimum {
                            let factor = fixed / minimum;
                            z = scale_vector(z, factor);
                            derivative *= factor;
                        } else if radius_squared < fixed {
                            let factor = fixed / radius_squared.max(1.0e-24);
                            z = scale_vector(z, factor);
                            derivative *= factor;
                        }
                    }
                    OrbitTransform::ScaleAddPoint { scale } => {
                        let scale = f64::from(*scale);
                        z = add(scale_vector(z, scale), point);
                        derivative = derivative * scale.abs() + 1.0;
                    }
                    OrbitTransform::ScaleAddConstant { scale, constant } => {
                        let scale = f64::from(*scale);
                        z = add(scale_vector(z, scale), constant.map(f64::from));
                        derivative = derivative * scale.abs() + 1.0;
                    }
                    OrbitTransform::Rotate { axis, degrees } => {
                        z = rotate(z, axis.map(f64::from), f64::from(*degrees).to_radians());
                    }
                    OrbitTransform::Translate { offset } => {
                        z = add(z, offset.map(f64::from));
                    }
                }
            }
            if self
                .bailout
                .is_some_and(|bailout| dot(z, z) > f64::from(bailout).powi(2))
            {
                break;
            }
        }

        dot(z, z).sqrt() / derivative.abs().max(1.0e-24)
    }
}

impl HighPrecisionDistanceEstimator for MandelboxConfig {
    fn distance_estimate_qf(&self, point: QfVec3) -> Qf32 {
        let mut z = point;
        let mut log_derivative = 0.0_f64;
        let scale = Qf32::from_f32(self.scale);
        let min_radius_squared = Qf32::from_f32(self.min_radius_squared);
        let fixed_radius_squared = Qf32::from_f32(self.fixed_radius_squared);

        for _ in 0..self.iterations {
            z = QfVec3::new(
                box_fold_qf(z.x, self.fold_limit),
                box_fold_qf(z.y, self.fold_limit),
                box_fold_qf(z.z, self.fold_limit),
            );
            let radius_squared = z.length_squared();
            if radius_squared < min_radius_squared {
                let factor = fixed_radius_squared / min_radius_squared;
                z = z * factor;
                log_derivative += factor.to_f64().ln();
            } else if radius_squared < fixed_radius_squared {
                let factor = fixed_radius_squared / radius_squared;
                z = z * factor;
                log_derivative += factor.to_f64().ln();
            }
            z = z * scale + point;
            let scaled_log_derivative = log_derivative + f64::from(self.scale.abs()).ln();
            log_derivative = scaled_log_derivative + (-scaled_log_derivative).exp().ln_1p();
        }

        let radius = z.length_squared().sqrt().to_f64();
        if radius == 0.0 {
            Qf32::ZERO
        } else {
            Qf32::from_f64((radius.ln() - log_derivative).exp())
        }
    }
}

fn box_fold_qf(value: Qf32, limit: f32) -> Qf32 {
    let upper = Qf32::from_f32(limit);
    let lower = Qf32::from_f32(-limit);
    if value > upper {
        Qf32::from_f32(2.0 * limit) - value
    } else if value < lower {
        Qf32::from_f32(-2.0 * limit) - value
    } else {
        value
    }
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale_vector(value: [f64; 3], scale: f64) -> [f64; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
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

fn rotate(value: [f64; 3], axis_value: [f64; 3], radians: f64) -> [f64; 3] {
    let axis_length_squared = dot(axis_value, axis_value);
    if axis_length_squared < 1.0e-24 {
        return value;
    }
    let inverse_length = axis_length_squared.sqrt().recip();
    let axis = scale_vector(axis_value, inverse_length);
    let cosine = radians.cos();
    let sine = radians.sin();
    add(
        add(
            scale_vector(value, cosine),
            scale_vector(cross(axis, value), sine),
        ),
        scale_vector(axis, dot(axis, value) * (1.0 - cosine)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mandelbox_estimator_is_symmetric() {
        let fractal = MandelboxConfig::default();
        let positive = fractal.distance_estimate([3.0, 1.25, 0.5]);
        let negative = fractal.distance_estimate([-3.0, -1.25, -0.5]);
        assert!((positive - negative).abs() < 1.0e-12);
        assert!(positive.is_finite() && positive > 0.0);
    }

    #[test]
    fn quad_estimator_matches_f64_at_overview_scale() {
        let fractal = MandelboxConfig::default();
        let point = [3.0, 1.25, 0.5];
        let ordinary = fractal.distance_estimate(point);
        let precise = fractal
            .distance_estimate_qf(QfVec3::from_f64(point))
            .to_f64();
        assert!((ordinary - precise).abs() < ordinary * 2.0e-6);
    }

    #[test]
    fn default_dsl_estimator_matches_the_builtin_mandelbox() {
        let built_in = MandelboxConfig::default();
        let generated = DslFractalConfig::default();
        for point in [[3.0, 1.25, 0.5], [-2.7, 0.4, 1.1], [0.3, -1.9, 2.4]] {
            let expected = built_in.distance_estimate(point);
            let actual = generated.distance_estimate(point);
            assert!((actual - expected).abs() < expected.abs().max(1.0) * 1.0e-12);
        }
    }

    #[test]
    fn transformed_dsl_estimator_remains_finite() {
        let mut generated = DslFractalConfig::default();
        generated.orbit.insert(
            0,
            OrbitTransform::Rotate {
                axis: [0.3, 0.8, 1.0],
                degrees: 7.5,
            },
        );
        let distance = generated.distance_estimate([4.0, 2.0, 1.0]);
        assert!(distance.is_finite() && distance > 0.0);
    }
}
