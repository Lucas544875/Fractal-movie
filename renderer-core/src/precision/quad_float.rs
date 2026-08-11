use std::{
    cmp::Ordering,
    error::Error,
    fmt,
    ops::{Add, Div, Mul, Neg, Sub},
    str::FromStr,
};

/// A fixed four-component floating-point expansion.
///
/// Components are stored from most to least significant. The type retains
/// roughly 90 useful significand bits while keeping the same exponent range
/// as `f32`, which mirrors the representation available in WGSL.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Qf32 {
    limbs: [f32; 4],
}

impl Qf32 {
    pub const ZERO: Self = Self::from_f32(0.0);
    pub const ONE: Self = Self::from_f32(1.0);
    pub const TWO: Self = Self::from_f32(2.0);
    pub const TEN: Self = Self::from_f32(10.0);

    #[must_use]
    pub const fn from_f32(value: f32) -> Self {
        Self {
            limbs: [value, 0.0, 0.0, 0.0],
        }
    }

    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        if !value.is_finite() {
            return Self::from_f32(value as f32);
        }
        let first = value as f32;
        let remainder = value - f64::from(first);
        let second = remainder as f32;
        let remainder = remainder - f64::from(second);
        let third = remainder as f32;
        let fourth = (remainder - f64::from(third)) as f32;
        Self::from_terms([first, second, third, fourth])
    }

    /// Constructs a normalized expansion from high-to-low components.
    #[must_use]
    pub fn from_limbs(limbs: [f32; 4]) -> Self {
        Self::from_terms(limbs)
    }

    #[must_use]
    pub const fn limbs(self) -> [f32; 4] {
        self.limbs
    }

    #[must_use]
    pub const fn leading(self) -> f32 {
        self.limbs[0]
    }

    #[must_use]
    pub fn to_f32(self) -> f32 {
        self.limbs.into_iter().sum()
    }

    #[must_use]
    pub fn to_f64(self) -> f64 {
        self.limbs.into_iter().map(f64::from).sum()
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.limbs.iter().all(|limb| limb.is_finite())
    }

    #[must_use]
    pub fn is_zero(self) -> bool {
        self.limbs.iter().all(|limb| *limb == 0.0)
    }

    #[must_use]
    pub fn is_sign_negative(self) -> bool {
        self.limbs
            .iter()
            .copied()
            .find(|limb| *limb != 0.0)
            .is_some_and(f32::is_sign_negative)
    }

    #[must_use]
    pub fn abs(self) -> Self {
        if self.is_sign_negative() { -self } else { self }
    }

    #[must_use]
    pub fn square(self) -> Self {
        self * self
    }

    #[must_use]
    pub fn sqrt(self) -> Self {
        if self.is_zero() {
            return Self::ZERO;
        }
        if self.is_sign_negative() || !self.is_finite() {
            return Self::from_f32(f32::NAN);
        }

        let mut estimate = Self::from_f32(self.to_f64().sqrt() as f32);
        for _ in 0..3 {
            estimate = (estimate + self / estimate) * Self::from_f32(0.5);
        }
        estimate
    }

    fn from_terms<const N: usize>(terms: [f32; N]) -> Self {
        if let Some(non_finite) = terms.iter().find(|term| !term.is_finite()) {
            return Self::from_f32(*non_finite);
        }

        let mut ordered = terms
            .into_iter()
            .filter(|term| *term != 0.0)
            .collect::<Vec<_>>();
        if ordered.is_empty() {
            return Self::ZERO;
        }
        ordered.sort_by(|left, right| left.abs().total_cmp(&right.abs()));

        let mut expansion = Vec::with_capacity(ordered.len() * 2);
        for term in ordered {
            expansion = grow_expansion(&expansion, term);
        }
        let compressed = compress_expansion(&expansion);
        let mut limbs = [0.0; 4];
        for (destination, source) in limbs
            .iter_mut()
            .zip(compressed.iter().rev().take(4).copied())
        {
            *destination = source;
        }
        Self { limbs }
    }

    fn multiply(self, rhs: Self) -> Self {
        let mut terms = [0.0; 32];
        let mut index = 0;
        for left in self.limbs {
            for right in rhs.limbs {
                let (product, error) = two_product(left, right);
                terms[index] = product;
                terms[index + 1] = error;
                index += 2;
            }
        }
        Self::from_terms(terms)
    }

    fn divide(self, rhs: Self) -> Self {
        if rhs.is_zero() {
            return Self::from_f32(self.to_f32() / 0.0);
        }
        let divisor = rhs.leading();
        let mut remainder = self;
        let mut quotient = [0.0; 5];
        for component in &mut quotient {
            *component = remainder.leading() / divisor;
            remainder = remainder - rhs * Self::from_f32(*component);
        }
        Self::from_terms(quotient)
    }
}

impl Default for Qf32 {
    fn default() -> Self {
        Self::ZERO
    }
}

impl Add for Qf32 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::from_terms([
            self.limbs[0],
            self.limbs[1],
            self.limbs[2],
            self.limbs[3],
            rhs.limbs[0],
            rhs.limbs[1],
            rhs.limbs[2],
            rhs.limbs[3],
        ])
    }
}

impl Sub for Qf32 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        self + -rhs
    }
}

impl Mul for Qf32 {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self::Output {
        self.multiply(rhs)
    }
}

impl Div for Qf32 {
    type Output = Self;

    fn div(self, rhs: Self) -> Self::Output {
        self.divide(rhs)
    }
}

impl Neg for Qf32 {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self {
            limbs: self.limbs.map(|limb| -limb),
        }
    }
}

impl PartialOrd for Qf32 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if !self.is_finite() || !other.is_finite() {
            return None;
        }
        let difference = *self - *other;
        if difference.is_zero() {
            Some(Ordering::Equal)
        } else if difference.is_sign_negative() {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Greater)
        }
    }
}

impl From<f32> for Qf32 {
    fn from(value: f32) -> Self {
        Self::from_f32(value)
    }
}

impl From<f64> for Qf32 {
    fn from(value: f64) -> Self {
        Self::from_f64(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QfParseError(String);

impl fmt::Display for QfParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for QfParseError {}

impl FromStr for Qf32 {
    type Err = QfParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.is_empty() || text.trim() != text {
            return Err(QfParseError(
                "quad-float decimal must not be empty or contain surrounding whitespace".into(),
            ));
        }
        let (negative, unsigned) = match text.as_bytes().first() {
            Some(b'-') => (true, &text[1..]),
            Some(b'+') => (false, &text[1..]),
            _ => (false, text),
        };
        let (mantissa, exponent) = split_exponent(unsigned)?;
        let mut point_seen = false;
        let mut digit_seen = false;
        let mut digit_count = 0_u32;
        let mut fractional_digits = 0_i32;
        let mut value = Self::ZERO;

        for byte in mantissa.bytes() {
            match byte {
                b'.' if !point_seen => point_seen = true,
                b'0'..=b'9' => {
                    digit_seen = true;
                    digit_count += 1;
                    if digit_count > 35 {
                        return Err(QfParseError(
                            "quad-float decimal mantissa must not exceed 35 digits".into(),
                        ));
                    }
                    let digit = Self::from_f32(f32::from(byte - b'0'));
                    value = value * Self::TEN + digit;
                    if point_seen {
                        fractional_digits += 1;
                    }
                }
                _ => return Err(QfParseError(format!("invalid quad-float decimal '{text}'"))),
            }
        }
        if !digit_seen {
            return Err(QfParseError(format!("invalid quad-float decimal '{text}'")));
        }

        let effective_exponent = exponent.checked_sub(fractional_digits).ok_or_else(|| {
            QfParseError("quad-float decimal exponent is outside the supported range".into())
        })?;
        let exponent_magnitude = effective_exponent.unsigned_abs();
        if exponent_magnitude > 64 {
            return Err(QfParseError(
                "quad-float decimal exponent magnitude must not exceed 64".into(),
            ));
        }
        for _ in 0..exponent_magnitude {
            value = if effective_exponent >= 0 {
                value * Self::TEN
            } else {
                value / Self::TEN
            };
        }
        if negative {
            value = -value;
        }
        if !value.is_finite() {
            return Err(QfParseError(
                "quad-float decimal is outside the finite f32 exponent range".into(),
            ));
        }
        Ok(value)
    }
}

fn split_exponent(text: &str) -> Result<(&str, i32), QfParseError> {
    let Some(index) = text.find(['e', 'E']) else {
        return Ok((text, 0));
    };
    if text[index + 1..].contains(['e', 'E']) {
        return Err(QfParseError(
            "quad-float decimal has multiple exponents".into(),
        ));
    }
    let exponent = text[index + 1..]
        .parse::<i32>()
        .map_err(|_| QfParseError("quad-float decimal has an invalid exponent".into()))?;
    Ok((&text[..index], exponent))
}

fn two_sum(left: f32, right: f32) -> (f32, f32) {
    let sum = left + right;
    let right_virtual = sum - left;
    let left_virtual = sum - right_virtual;
    let right_roundoff = right - right_virtual;
    let left_roundoff = left - left_virtual;
    (sum, left_roundoff + right_roundoff)
}

fn quick_two_sum(left: f32, right: f32) -> (f32, f32) {
    let sum = left + right;
    (sum, right - (sum - left))
}

fn two_product(left: f32, right: f32) -> (f32, f32) {
    let product = left * right;
    let exact = f64::from(left) * f64::from(right);
    (product, (exact - f64::from(product)) as f32)
}

/// Adds one scalar to an increasing-magnitude, non-overlapping expansion.
fn grow_expansion(expansion: &[f32], scalar: f32) -> Vec<f32> {
    let mut result = Vec::with_capacity(expansion.len() + 1);
    let mut accumulator = scalar;
    for component in expansion {
        let (sum, error) = two_sum(accumulator, *component);
        if error != 0.0 {
            result.push(error);
        }
        accumulator = sum;
    }
    if accumulator != 0.0 || result.is_empty() {
        result.push(accumulator);
    }
    result
}

/// Compresses an exact expansion following Shewchuk's two-pass algorithm.
fn compress_expansion(expansion: &[f32]) -> Vec<f32> {
    if expansion.len() <= 1 {
        return expansion.to_vec();
    }
    let mut intermediate = vec![0.0; expansion.len()];
    let mut accumulator = expansion[expansion.len() - 1];
    let mut bottom = expansion.len() - 1;

    for component in expansion[..expansion.len() - 1].iter().rev() {
        let (sum, error) = quick_two_sum(accumulator, *component);
        if error != 0.0 {
            intermediate[bottom] = sum;
            bottom -= 1;
            accumulator = error;
        } else {
            accumulator = sum;
        }
    }

    let mut result = Vec::with_capacity(expansion.len() - bottom);
    for component in &intermediate[bottom + 1..] {
        let (sum, error) = quick_two_sum(*component, accumulator);
        if error != 0.0 {
            result.push(error);
        }
        accumulator = sum;
    }
    result.push(accumulator);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rug::Float;

    const ORACLE_BITS: u32 = 160;

    fn oracle(value: Qf32) -> Float {
        let mut result = Float::with_val(ORACLE_BITS, 0);
        for limb in value.limbs() {
            result += limb;
        }
        result
    }

    fn oracle_decimal(value: &str) -> Float {
        Float::with_val(
            ORACLE_BITS,
            Float::parse(value).expect("oracle decimal must parse"),
        )
    }

    fn assert_relative_error(actual: Qf32, expected: &Float, maximum: f64) {
        let mut error = oracle(actual) - expected;
        error.abs_mut();
        let mut scale = expected.clone();
        scale.abs_mut();
        scale *= maximum;
        assert!(
            error < scale,
            "quad-float error {} exceeds {}",
            error,
            scale
        );
    }

    #[test]
    fn retains_values_far_below_one_f32_ulp() {
        let one = Qf32::ONE;
        let tiny = Qf32::from_str("1e-20").expect("decimal must parse");
        let recovered = (one + tiny) - one;
        assert!((recovered.to_f64() - 1.0e-20).abs() < 1.0e-26);
        assert_ne!(one + tiny, one);
    }

    #[test]
    fn decimal_parser_retains_four_limb_precision() {
        let value = Qf32::from_str("1.0000000000000000000000000001")
            .expect("high-precision decimal must parse");
        let residual = value - Qf32::ONE;
        assert!(residual > Qf32::ZERO);
        assert!((residual.to_f64() - 1.0e-28).abs() < 1.0e-34);
    }

    #[test]
    fn multiplication_retains_a_second_order_residual() {
        let epsilon = Qf32::from_f32(2.0_f32.powi(-30));
        let product = (Qf32::ONE + epsilon) * (Qf32::ONE - epsilon);
        let residual = product - Qf32::ONE;
        assert!(residual < Qf32::ZERO);
        assert!((residual.to_f64() + 2.0_f64.powi(-60)).abs() < 1.0e-25);
    }

    #[test]
    fn division_and_sqrt_recover_their_operands() {
        let third = Qf32::ONE / Qf32::from_f32(3.0);
        let recovered = third * Qf32::from_f32(3.0);
        assert!((recovered - Qf32::ONE).abs() < Qf32::from_str("1e-25").unwrap());

        let root = Qf32::from_f32(2.0).sqrt();
        assert!((root.square() - Qf32::from_f32(2.0)).abs() < Qf32::from_str("1e-24").unwrap());
    }

    #[test]
    fn rejects_invalid_decimal_syntax() {
        for text in ["", " 1", "1 ", ".", "1e", "1e2e3", "nan"] {
            assert!(Qf32::from_str(text).is_err(), "'{text}' must fail");
        }
    }

    #[test]
    fn arithmetic_tracks_a_160_bit_oracle() {
        let a_text = "1.2345678901234567890123456789";
        let b_text = "0.9876543210987654321098765432";
        let a = Qf32::from_str(a_text).unwrap();
        let b = Qf32::from_str(b_text).unwrap();
        let oracle_a = oracle_decimal(a_text);
        let oracle_b = oracle_decimal(b_text);

        let expected_sum = Float::with_val(ORACLE_BITS, &oracle_a + &oracle_b);
        assert_relative_error(a + b, &expected_sum, 1.0e-24);

        let expected_product = Float::with_val(ORACLE_BITS, &oracle_a * &oracle_b);
        assert_relative_error(a * b, &expected_product, 1.0e-23);

        let expected_quotient = Float::with_val(ORACLE_BITS, &oracle_a / &oracle_b);
        assert_relative_error(a / b, &expected_quotient, 1.0e-22);
    }
}
