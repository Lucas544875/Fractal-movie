use crate::MandelboxConfig;

/// CPU-side counterpart of a shader distance estimator.
///
/// Camera target search is generic over this contract, so future fractals can
/// reuse the portfolio's path-selection algorithm without depending on WGSL.
pub trait DistanceEstimator {
    fn distance_estimate(&self, point: [f64; 3]) -> f64;
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

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale_vector(value: [f64; 3], scale: f64) -> [f64; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
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
}
