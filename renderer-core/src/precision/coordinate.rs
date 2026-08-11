use std::ops::{Add, Div, Mul, Neg, Sub};

use super::Qf32;

/// Three-dimensional coordinate whose components retain quad-float precision.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QfVec3 {
    pub x: Qf32,
    pub y: Qf32,
    pub z: Qf32,
}

impl QfVec3 {
    pub const ZERO: Self = Self::splat(Qf32::ZERO);

    #[must_use]
    pub const fn new(x: Qf32, y: Qf32, z: Qf32) -> Self {
        Self { x, y, z }
    }

    #[must_use]
    pub const fn splat(value: Qf32) -> Self {
        Self::new(value, value, value)
    }

    #[must_use]
    pub fn from_f32(value: [f32; 3]) -> Self {
        Self::new(value[0].into(), value[1].into(), value[2].into())
    }

    #[must_use]
    pub fn from_f64(value: [f64; 3]) -> Self {
        Self::new(value[0].into(), value[1].into(), value[2].into())
    }

    #[must_use]
    pub fn to_f32(self) -> [f32; 3] {
        [self.x.to_f32(), self.y.to_f32(), self.z.to_f32()]
    }

    #[must_use]
    pub fn to_f64(self) -> [f64; 3] {
        [self.x.to_f64(), self.y.to_f64(), self.z.to_f64()]
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.components()
            .iter()
            .all(|component| component.is_finite())
    }

    #[must_use]
    pub const fn components(self) -> [Qf32; 3] {
        [self.x, self.y, self.z]
    }

    #[must_use]
    pub fn dot(self, rhs: Self) -> Qf32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    #[must_use]
    pub fn length_squared(self) -> Qf32 {
        self.dot(self)
    }

    #[must_use]
    pub fn normalized(self) -> Option<Self> {
        let maximum = self
            .components()
            .into_iter()
            .map(Qf32::abs)
            .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))?;
        if maximum.is_zero() || !maximum.is_finite() {
            return None;
        }
        let scaled = self / maximum;
        let length = scaled.length_squared().sqrt();
        if length.is_zero() || !length.is_finite() {
            return None;
        }
        Some(scaled / length)
    }

    #[must_use]
    pub fn normalized_to_f32(self) -> Option<[f32; 3]> {
        self.normalized().map(Self::to_f32)
    }
}

impl Add for QfVec3 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl Sub for QfVec3 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl Mul<Qf32> for QfVec3 {
    type Output = Self;

    fn mul(self, rhs: Qf32) -> Self::Output {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

impl Div<Qf32> for QfVec3 {
    type Output = Self;

    fn div(self, rhs: Qf32) -> Self::Output {
        Self::new(self.x / rhs, self.y / rhs, self.z / rhs)
    }
}

impl Neg for QfVec3 {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self::new(-self.x, -self.y, -self.z)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn normalizes_tiny_high_precision_offsets() {
        let vector = QfVec3::new(
            Qf32::from_str("1e-20").unwrap(),
            Qf32::from_str("-2e-20").unwrap(),
            Qf32::from_str("2e-20").unwrap(),
        );
        let normalized = vector.normalized_to_f32().expect("vector is non-zero");
        assert!((normalized[0] - 1.0 / 3.0).abs() < 1.0e-6);
        assert!((normalized[1] + 2.0 / 3.0).abs() < 1.0e-6);
        assert!((normalized[2] - 2.0 / 3.0).abs() < 1.0e-6);
    }
}
