//! Image descriptors for the novelty gate. A new film must not look like a
//! finished one, so scouted thumbnails are compared against the keyframes of
//! every film in the ledger.
//!
//! The descriptors are hand-built and dependency-free. They are a coarse
//! screen: the formula-family ban, the axis rules, and the critic carry most
//! of the weight.

use serde::{Deserialize, Serialize};

use crate::metrics::{Frame, LEGIBILITY_WIDTH};

/// Successive halvings whose variance differences form the scale spectrum.
const SCALE_BANDS: usize = 4;
const ORIENTATION_BINS: usize = 8;
/// Side of the coarse luminance layout grid.
const LAYOUT_CELLS: usize = 6;
/// Luminance deviation between layout cells below which a frame is treated
/// as having no composition to compare.
const MIN_LAYOUT_DEVIATION: f32 = 0.05;
const HUE_BINS: usize = 12;
const MIN_HUE_SATURATION: f32 = 0.25;
const MIN_HUE_VALUE: f32 = 0.10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    /// Colour-independent form: scale spectrum, edge orientations, and the
    /// coarse layout of light and dark. Comparable between a grey scouting
    /// thumbnail and a graded keyframe.
    pub structure: Vec<f32>,
    /// Hue histogram followed by mean saturation and mean luminance.
    pub colour: Vec<f32>,
}

/// Describes a frame at thumbnail size.
#[must_use]
pub fn describe(frame: &Frame) -> Descriptor {
    let frame = frame.fit_width(LEGIBILITY_WIDTH);
    let mut structure = scale_spectrum(&frame);
    structure.extend(orientation_histogram(&frame));
    structure.extend(layout(&frame));
    Descriptor {
        structure,
        colour: colour(&frame),
    }
}

/// Distance between the forms of two frames, in `0..=1`.
#[must_use]
pub fn structure_distance(a: &Descriptor, b: &Descriptor) -> f32 {
    let split_a = a
        .structure
        .split_at((SCALE_BANDS + 1).min(a.structure.len()));
    let split_b = b
        .structure
        .split_at((SCALE_BANDS + 1).min(b.structure.len()));
    let (orientation_a, layout_a) = split_a.1.split_at(ORIENTATION_BINS.min(split_a.1.len()));
    let (orientation_b, layout_b) = split_b.1.split_at(ORIENTATION_BINS.min(split_b.1.len()));
    // Histograms sum to one, so half their L1 distance lies in 0..=1. The
    // layout is standardised; its mean absolute difference rarely exceeds 2.
    let spectrum = l1(split_a.0, split_b.0) / 2.0;
    let orientation = l1(orientation_a, orientation_b) / 2.0;
    let layout = (l1(layout_a, layout_b) / layout_a.len().max(1) as f32 / 2.0).min(1.0);
    (spectrum + orientation + layout) / 3.0
}

/// Smallest structure distance from `candidate` to any of `others`.
#[must_use]
pub fn nearest_structure_distance<'a>(
    candidate: &Descriptor,
    others: impl IntoIterator<Item = &'a Descriptor>,
) -> Option<f32> {
    others
        .into_iter()
        .map(|other| structure_distance(candidate, other))
        .min_by(f32::total_cmp)
}

fn l1(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum()
}

fn variance(values: &[f32]) -> f32 {
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f32>()
        / values.len() as f32
}

/// Share of luminance variance at each scale, finest first, followed by the
/// share left at the coarsest level.
fn scale_spectrum(frame: &Frame) -> Vec<f32> {
    let mut level = frame.clone();
    let mut variances = vec![variance(&level.luma())];
    for _ in 0..SCALE_BANDS {
        level = level.reduced(2);
        variances.push(variance(&level.luma()));
    }
    let mut spectrum: Vec<f32> = variances
        .windows(2)
        .map(|pair| (pair[0] - pair[1]).max(0.0))
        .collect();
    spectrum.push(variances[SCALE_BANDS]);
    normalise(&mut spectrum);
    spectrum
}

/// Gradient-magnitude-weighted histogram of edge directions, modulo 180°.
fn orientation_histogram(frame: &Frame) -> Vec<f32> {
    let (width, height) = (frame.width(), frame.height());
    let luma = frame.luma();
    let mut bins = vec![0.0f32; ORIENTATION_BINS];
    for y in 1..height.saturating_sub(1) {
        for x in 1..width.saturating_sub(1) {
            let dx = luma[y * width + x + 1] - luma[y * width + x - 1];
            let dy = luma[(y + 1) * width + x] - luma[(y - 1) * width + x];
            let magnitude = dx.hypot(dy);
            if magnitude <= 0.0 {
                continue;
            }
            let angle = dy.atan2(dx).rem_euclid(std::f32::consts::PI);
            let bin = ((angle / std::f32::consts::PI * ORIENTATION_BINS as f32) as usize)
                .min(ORIENTATION_BINS - 1);
            bins[bin] += magnitude;
        }
    }
    normalise(&mut bins);
    bins
}

/// Mean luminance on a coarse grid, standardised so exposure and contrast do
/// not count as a different composition.
fn layout(frame: &Frame) -> Vec<f32> {
    let (width, height) = (frame.width(), frame.height());
    let luma = frame.luma();
    let mut cells = vec![0.0f32; LAYOUT_CELLS * LAYOUT_CELLS];
    let mut counts = vec![0u32; LAYOUT_CELLS * LAYOUT_CELLS];
    for y in 0..height {
        for x in 0..width {
            let cell = (y * LAYOUT_CELLS / height) * LAYOUT_CELLS + x * LAYOUT_CELLS / width;
            cells[cell] += luma[y * width + x];
            counts[cell] += 1;
        }
    }
    for (cell, count) in cells.iter_mut().zip(&counts) {
        *cell /= (*count).max(1) as f32;
    }
    let mean = cells.iter().sum::<f32>() / cells.len() as f32;
    // Without a floor, a frame with no real layout would have its residual
    // noise scaled up to look like a strong composition.
    let deviation = variance(&cells).sqrt().max(MIN_LAYOUT_DEVIATION);
    cells.iter().map(|cell| (cell - mean) / deviation).collect()
}

fn colour(frame: &Frame) -> Vec<f32> {
    let mut hues = vec![0.0f32; HUE_BINS];
    let (mut saturation_sum, mut luminance_sum) = (0.0f32, 0.0f32);
    for pixel in frame.pixels() {
        let max = pixel[0].max(pixel[1]).max(pixel[2]);
        let min = pixel[0].min(pixel[1]).min(pixel[2]);
        let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
        saturation_sum += saturation;
        luminance_sum += 0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2];
        if let Some(hue) = hue_degrees(*pixel) {
            let bin = ((hue / 360.0 * HUE_BINS as f32) as usize).min(HUE_BINS - 1);
            hues[bin] += saturation * max;
        }
    }
    normalise(&mut hues);
    let count = frame.pixels().len() as f32;
    hues.extend([saturation_sum / count, luminance_sum / count]);
    hues
}

/// Hue in degrees, or `None` for pixels too grey or dark to carry one.
#[must_use]
pub fn hue_degrees(pixel: [f32; 3]) -> Option<f32> {
    let max = pixel[0].max(pixel[1]).max(pixel[2]);
    let min = pixel[0].min(pixel[1]).min(pixel[2]);
    let chroma = max - min;
    if max < MIN_HUE_VALUE || chroma / max < MIN_HUE_SATURATION {
        return None;
    }
    let sextant = if max == pixel[0] {
        ((pixel[1] - pixel[2]) / chroma).rem_euclid(6.0)
    } else if max == pixel[1] {
        (pixel[2] - pixel[0]) / chroma + 2.0
    } else {
        (pixel[0] - pixel[1]) / chroma + 4.0
    };
    Some(sextant * 60.0)
}

fn normalise(values: &mut [f32]) {
    let total: f32 = values.iter().sum();
    if total > 0.0 {
        for value in values {
            *value /= total;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{metrics::tests::frame, rng::Rng};

    fn grey(value: f32) -> [f32; 3] {
        [value; 3]
    }

    fn stripes(vertical: bool, period: usize) -> Frame {
        frame(320, 180, |x, y| {
            let coordinate = if vertical { x } else { y };
            grey(if (coordinate / period).is_multiple_of(2) {
                0.2
            } else {
                0.8
            })
        })
    }

    fn noise(seed: u64) -> Frame {
        let mut rng = Rng::new(seed);
        let values: Vec<f32> = (0..320 * 180).map(|_| rng.unit() as f32).collect();
        frame(320, 180, |x, y| grey(values[y * 320 + x]))
    }

    #[test]
    fn descriptor_has_a_fixed_layout() {
        let descriptor = describe(&noise(1));
        assert_eq!(
            descriptor.structure.len(),
            SCALE_BANDS + 1 + ORIENTATION_BINS + LAYOUT_CELLS * LAYOUT_CELLS
        );
        assert_eq!(descriptor.colour.len(), HUE_BINS + 2);
        assert!(descriptor.structure.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn identical_frames_are_at_distance_zero() {
        let descriptor = describe(&stripes(true, 16));
        assert_eq!(structure_distance(&descriptor, &descriptor), 0.0);
    }

    #[test]
    fn exposure_and_tint_do_not_change_structure() {
        // Stripes that are brighter on the left: texture plus a composition.
        let value = |x: usize| {
            let stripe = if (x / 16).is_multiple_of(2) {
                0.25
            } else {
                1.0
            };
            stripe * if x < 160 { 0.9 } else { 0.35 }
        };
        let base = frame(320, 180, |x, _| grey(value(x)));
        let graded = frame(320, 180, |x, _| {
            let value = value(x) * 0.6;
            [value, value * 0.7, value * 0.4]
        });
        let distance = structure_distance(&describe(&base), &describe(&graded));
        assert!(
            distance < crate::tuning::Diversity::default().min_structure_distance / 4.0,
            "{distance}"
        );
        assert_ne!(describe(&base).colour, describe(&graded).colour);
    }

    #[test]
    fn orientation_scale_and_layout_each_separate_frames() {
        let vertical = describe(&stripes(true, 16));
        let horizontal = describe(&stripes(false, 16));
        let fine = describe(&stripes(true, 2));
        let left_heavy = describe(&frame(320, 180, |x, _| {
            grey(if x < 100 { 0.9 } else { 0.1 })
        }));
        let right_heavy = describe(&frame(320, 180, |x, _| {
            grey(if x > 220 { 0.9 } else { 0.1 })
        }));

        assert!(structure_distance(&vertical, &horizontal) > 0.25);
        assert!(structure_distance(&vertical, &fine) > 0.15);
        assert!(structure_distance(&left_heavy, &right_heavy) > 0.15);
    }

    #[test]
    fn independent_noise_frames_look_alike() {
        // Same statistics, different pixels: structurally the same picture.
        let distance = structure_distance(&describe(&noise(1)), &describe(&noise(2)));
        assert!(
            distance < crate::tuning::Diversity::default().min_structure_distance,
            "{distance}"
        );
    }

    #[test]
    fn nearest_distance_picks_the_closest_reference() {
        let candidate = describe(&stripes(true, 16));
        let references = [describe(&stripes(false, 16)), describe(&stripes(true, 16))];
        assert_eq!(
            nearest_structure_distance(&candidate, &references),
            Some(0.0)
        );
        assert_eq!(nearest_structure_distance(&candidate, &[]), None);
    }

    #[test]
    fn hue_is_reported_in_degrees() {
        let close =
            |pixel: [f32; 3], expected: f32| (hue_degrees(pixel).unwrap() - expected).abs() < 0.5;
        assert!(close([0.9, 0.1, 0.1], 0.0));
        assert!(close([0.1, 0.9, 0.1], 120.0));
        assert!(close([0.1, 0.1, 0.9], 240.0));
        assert!(close([0.9, 0.1, 0.5], 330.0));
        assert_eq!(hue_degrees([0.5, 0.5, 0.5]), None);
        assert_eq!(hue_degrees([0.05, 0.0, 0.0]), None);
    }
}
