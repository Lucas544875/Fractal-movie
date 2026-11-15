//! Image measurements. Legibility metrics deliberately never reward detail:
//! they describe how readable a frame is once it is small and compressed.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Width legibility is judged at. A frame that does not read at thumbnail
/// size is cluttered regardless of how it looks at full resolution.
pub const LEGIBILITY_WIDTH: u32 = 320;

const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// Side of the blocks used for local contrast and for SSIM windows.
const BLOCK: usize = 8;
/// Factor by which the low-frequency reference is downsampled.
const LOW_PASS_FACTOR: usize = 4;
/// Block luminance deviation below which a block counts as empty.
const FLAT_BLOCK_DEVIATION: f32 = 0.02;
const CLIP_HIGH: f32 = 0.98;
const CLIP_LOW: f32 = 0.02;
/// Pixels below these values carry no usable hue.
const MIN_HUE_SATURATION: f32 = 0.25;
const MIN_HUE_VALUE: f32 = 0.10;
const HUE_BINS: usize = 12;
/// Share of the coloured pixels a hue bin needs to count as a cluster.
const MIN_HUE_SHARE: f32 = 0.10;
/// Share of the frame that must be coloured before hues are counted at all.
const MIN_COLOURED_SHARE: f32 = 0.02;

/// A display-referred RGB image with components in `0..=1`.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<[f32; 3]>,
}

impl Frame {
    pub fn new(width: usize, height: usize, pixels: Vec<[f32; 3]>) -> Result<Self> {
        if width == 0 || height == 0 || pixels.len() != width * height {
            bail!(
                "frame of {width}x{height} cannot hold {} pixels",
                pixels.len()
            );
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Wraps tightly packed RGBA8 as produced by the renderer.
    pub fn from_rgba8(width: u32, height: u32, rgba: &[u8]) -> Result<Self> {
        let (width, height) = (width as usize, height as usize);
        if rgba.len() != width * height * 4 {
            bail!(
                "RGBA buffer of {} bytes does not match {width}x{height}",
                rgba.len()
            );
        }
        let pixels = rgba
            .chunks_exact(4)
            .map(|pixel| {
                [
                    f32::from(pixel[0]) / 255.0,
                    f32::from(pixel[1]) / 255.0,
                    f32::from(pixel[2]) / 255.0,
                ]
            })
            .collect();
        Self::new(width, height, pixels)
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    #[must_use]
    pub fn pixels(&self) -> &[[f32; 3]] {
        &self.pixels
    }

    /// Rec. 709 luminance of every pixel, in row order.
    #[must_use]
    pub fn luma(&self) -> Vec<f32> {
        self.pixels
            .iter()
            .map(|pixel| LUMA[0] * pixel[0] + LUMA[1] * pixel[1] + LUMA[2] * pixel[2])
            .collect()
    }

    /// Area-averages the image down by an integer factor. Trailing rows and
    /// columns that do not fill a whole cell are dropped.
    #[must_use]
    pub fn reduced(&self, factor: usize) -> Self {
        let factor = factor.max(1);
        if factor == 1 || self.width < factor || self.height < factor {
            return self.clone();
        }
        let (width, height) = (self.width / factor, self.height / factor);
        let scale = 1.0 / (factor * factor) as f32;
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let mut sum = [0.0f32; 3];
                for row in 0..factor {
                    let start = (y * factor + row) * self.width + x * factor;
                    for pixel in &self.pixels[start..start + factor] {
                        for channel in 0..3 {
                            sum[channel] += pixel[channel];
                        }
                    }
                }
                pixels.push(sum.map(|value| value * scale));
            }
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Reduces the image so that it is at most `max_width` wide.
    #[must_use]
    pub fn fit_width(&self, max_width: u32) -> Self {
        self.reduced(self.width.div_ceil(max_width.max(1) as usize))
    }
}

/// Legibility measurements of one frame at thumbnail size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Legibility {
    /// Share of luminance variance carried by detail finer than four pixels.
    pub high_frequency_ratio: f32,
    /// Share of the frame that is background or visually flat.
    pub negative_space_ratio: f32,
    /// Number of distinct hue clusters among the coloured pixels.
    pub hue_clusters: u32,
    pub clipped_high_ratio: f32,
    pub clipped_low_ratio: f32,
    pub mean_luminance: f32,
}

/// Measures a frame after reducing it to [`LEGIBILITY_WIDTH`].
#[must_use]
pub fn legibility(frame: &Frame) -> Legibility {
    let frame = frame.fit_width(LEGIBILITY_WIDTH);
    let luma = frame.luma();
    let count = luma.len() as f32;
    let mean = luma.iter().sum::<f32>() / count;

    Legibility {
        high_frequency_ratio: high_frequency_ratio(&luma, frame.width, frame.height, mean),
        negative_space_ratio: negative_space_ratio(&luma, frame.width, frame.height),
        hue_clusters: hue_clusters(&frame.pixels),
        clipped_high_ratio: luma.iter().filter(|value| **value >= CLIP_HIGH).count() as f32 / count,
        clipped_low_ratio: luma.iter().filter(|value| **value <= CLIP_LOW).count() as f32 / count,
        mean_luminance: mean,
    }
}

/// Compares luminance against a blurred copy of itself. The residual is the
/// detail a viewer cannot resolve at thumbnail size and a codec will smear.
fn high_frequency_ratio(luma: &[f32], width: usize, height: usize, mean: f32) -> f32 {
    let total: f32 = luma.iter().map(|value| (value - mean).powi(2)).sum();
    // A flat frame has no detail of any kind.
    if total <= f32::EPSILON * luma.len() as f32 {
        return 0.0;
    }
    let low = low_pass(luma, width, height);
    let residual: f32 = luma
        .iter()
        .zip(&low)
        .map(|(value, low)| (value - low).powi(2))
        .sum();
    (residual / total).clamp(0.0, 1.0)
}

/// Box-downsamples by [`LOW_PASS_FACTOR`] and bilinearly resamples back.
fn low_pass(luma: &[f32], width: usize, height: usize) -> Vec<f32> {
    let cells_x = width.div_ceil(LOW_PASS_FACTOR);
    let cells_y = height.div_ceil(LOW_PASS_FACTOR);
    let mut cells = vec![0.0f32; cells_x * cells_y];
    let mut counts = vec![0u32; cells_x * cells_y];
    for y in 0..height {
        for x in 0..width {
            let cell = (y / LOW_PASS_FACTOR) * cells_x + x / LOW_PASS_FACTOR;
            cells[cell] += luma[y * width + x];
            counts[cell] += 1;
        }
    }
    for (cell, count) in cells.iter_mut().zip(&counts) {
        *cell /= *count as f32;
    }

    let sample = |x: usize, y: usize| cells[y.min(cells_y - 1) * cells_x + x.min(cells_x - 1)];
    let mut low = Vec::with_capacity(luma.len());
    for y in 0..height {
        // Cell centres sit at half-cell offsets in pixel space.
        let fy = ((y as f32 + 0.5) / LOW_PASS_FACTOR as f32 - 0.5).max(0.0);
        let (y0, ty) = (fy as usize, fy.fract());
        for x in 0..width {
            let fx = ((x as f32 + 0.5) / LOW_PASS_FACTOR as f32 - 0.5).max(0.0);
            let (x0, tx) = (fx as usize, fx.fract());
            let top = sample(x0, y0) * (1.0 - tx) + sample(x0 + 1, y0) * tx;
            let bottom = sample(x0, y0 + 1) * (1.0 - tx) + sample(x0 + 1, y0 + 1) * tx;
            low.push(top * (1.0 - ty) + bottom * ty);
        }
    }
    low
}

fn negative_space_ratio(luma: &[f32], width: usize, height: usize) -> f32 {
    let (mut flat, mut blocks) = (0u32, 0u32);
    for block_y in (0..height).step_by(BLOCK) {
        for block_x in (0..width).step_by(BLOCK) {
            let (mut sum, mut sum_squares, mut count) = (0.0f32, 0.0f32, 0.0f32);
            for y in block_y..(block_y + BLOCK).min(height) {
                for x in block_x..(block_x + BLOCK).min(width) {
                    let value = luma[y * width + x];
                    sum += value;
                    sum_squares += value * value;
                    count += 1.0;
                }
            }
            let mean = sum / count;
            let deviation = (sum_squares / count - mean * mean).max(0.0).sqrt();
            blocks += 1;
            if deviation < FLAT_BLOCK_DEVIATION {
                flat += 1;
            }
        }
    }
    flat as f32 / blocks as f32
}

/// Counts runs of adjacent, well-populated hue bins around the colour wheel.
fn hue_clusters(pixels: &[[f32; 3]]) -> u32 {
    let mut bins = [0u32; HUE_BINS];
    let mut coloured = 0u32;
    for pixel in pixels {
        let max = pixel[0].max(pixel[1]).max(pixel[2]);
        let min = pixel[0].min(pixel[1]).min(pixel[2]);
        let chroma = max - min;
        if max < MIN_HUE_VALUE || chroma / max < MIN_HUE_SATURATION {
            continue;
        }
        let hue = if max == pixel[0] {
            ((pixel[1] - pixel[2]) / chroma).rem_euclid(6.0)
        } else if max == pixel[1] {
            (pixel[2] - pixel[0]) / chroma + 2.0
        } else {
            (pixel[0] - pixel[1]) / chroma + 4.0
        };
        let bin = ((hue / 6.0 * HUE_BINS as f32) as usize).min(HUE_BINS - 1);
        bins[bin] += 1;
        coloured += 1;
    }
    if (coloured as f32) < MIN_COLOURED_SHARE * pixels.len() as f32 {
        return 0;
    }

    let populated = bins.map(|count| count as f32 >= MIN_HUE_SHARE * coloured as f32);
    if populated.iter().all(|bin| *bin) {
        return 1;
    }
    // A run starts wherever a populated bin follows an unpopulated one; the
    // wheel is circular, so bin 0 follows the last bin.
    (0..HUE_BINS)
        .filter(|&bin| populated[bin] && !populated[(bin + HUE_BINS - 1) % HUE_BINS])
        .count() as u32
}

/// How far a rendered variant is from its reference.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Difference {
    /// Mean structural similarity of luminance; 1 is identical.
    pub ssim: f32,
    /// Mean absolute RGB difference.
    pub colour: f32,
}

impl Difference {
    /// Single figure the quality ladder thresholds: structural loss plus
    /// colour shift. Zero means the variant is indistinguishable.
    #[must_use]
    pub fn score(self) -> f32 {
        (1.0 - self.ssim).max(0.0) + self.colour
    }
}

/// Compares two frames of identical size.
pub fn difference(reference: &Frame, variant: &Frame) -> Result<Difference> {
    if reference.width != variant.width || reference.height != variant.height {
        bail!(
            "cannot compare a {}x{} frame with a {}x{} frame",
            reference.width,
            reference.height,
            variant.width,
            variant.height
        );
    }
    let colour = reference
        .pixels
        .iter()
        .zip(&variant.pixels)
        .map(|(a, b)| {
            (0..3)
                .map(|channel| (a[channel] - b[channel]).abs())
                .sum::<f32>()
                / 3.0
        })
        .sum::<f32>()
        / reference.pixels.len() as f32;

    Ok(Difference {
        ssim: ssim(
            &reference.luma(),
            &variant.luma(),
            reference.width,
            reference.height,
        ),
        colour,
    })
}

fn ssim(a: &[f32], b: &[f32], width: usize, height: usize) -> f32 {
    const C1: f32 = 0.01 * 0.01;
    const C2: f32 = 0.03 * 0.03;
    let stride = BLOCK / 2;
    let window_x = BLOCK.min(width);
    let window_y = BLOCK.min(height);
    let count = (window_x * window_y) as f32;

    let (mut total, mut windows) = (0.0f32, 0u32);
    for top in (0..=height - window_y).step_by(stride) {
        for left in (0..=width - window_x).step_by(stride) {
            let (mut sum_a, mut sum_b) = (0.0f32, 0.0f32);
            let (mut sum_aa, mut sum_bb, mut sum_ab) = (0.0f32, 0.0f32, 0.0f32);
            for y in top..top + window_y {
                for x in left..left + window_x {
                    let (va, vb) = (a[y * width + x], b[y * width + x]);
                    sum_a += va;
                    sum_b += vb;
                    sum_aa += va * va;
                    sum_bb += vb * vb;
                    sum_ab += va * vb;
                }
            }
            let (mean_a, mean_b) = (sum_a / count, sum_b / count);
            let variance_a = (sum_aa / count - mean_a * mean_a).max(0.0);
            let variance_b = (sum_bb / count - mean_b * mean_b).max(0.0);
            let covariance = sum_ab / count - mean_a * mean_b;
            total += ((2.0 * mean_a * mean_b + C1) * (2.0 * covariance + C2))
                / ((mean_a * mean_a + mean_b * mean_b + C1) * (variance_a + variance_b + C2));
            windows += 1;
        }
    }
    total / windows as f32
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::rng::Rng;

    pub(crate) fn frame(
        width: usize,
        height: usize,
        pixel: impl Fn(usize, usize) -> [f32; 3],
    ) -> Frame {
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| pixel(x, y))
            .collect();
        Frame::new(width, height, pixels).unwrap()
    }

    fn grey(value: f32) -> [f32; 3] {
        [value; 3]
    }

    fn noise(width: usize, height: usize, seed: u64) -> Frame {
        let mut rng = Rng::new(seed);
        let values: Vec<f32> = (0..width * height).map(|_| rng.unit() as f32).collect();
        frame(width, height, |x, y| grey(values[y * width + x]))
    }

    #[test]
    fn construction_rejects_mismatched_buffers() {
        assert!(Frame::new(2, 2, vec![[0.0; 3]; 3]).is_err());
        assert!(Frame::new(0, 2, Vec::new()).is_err());
        assert!(Frame::from_rgba8(2, 1, &[0; 7]).is_err());
        let frame = Frame::from_rgba8(2, 1, &[255, 0, 0, 255, 0, 51, 0, 255]).unwrap();
        assert_eq!(frame.pixels()[0], [1.0, 0.0, 0.0]);
        assert_eq!(frame.pixels()[1], [0.0, 0.2, 0.0]);
    }

    #[test]
    fn reduction_averages_whole_cells() {
        let source = frame(5, 4, |x, _| grey(if x < 2 { 0.0 } else { 1.0 }));
        let reduced = source.reduced(2);
        assert_eq!((reduced.width(), reduced.height()), (2, 2));
        assert_eq!(reduced.pixels()[0], grey(0.0));
        assert_eq!(reduced.pixels()[1], grey(1.0));

        assert_eq!(noise(1920, 8, 1).fit_width(320).width(), 320);
        assert_eq!(noise(1000, 8, 1).fit_width(320).width(), 250);
        assert_eq!(noise(200, 8, 1).fit_width(320).width(), 200);
    }

    #[test]
    fn noise_is_high_frequency_and_broad_shapes_are_not() {
        let cluttered = legibility(&noise(320, 180, 3));
        let gradient = legibility(&frame(320, 180, |x, _| grey(x as f32 / 319.0)));
        let flat = legibility(&frame(320, 180, |_, _| grey(0.4)));

        assert!(cluttered.high_frequency_ratio > 0.8, "{cluttered:?}");
        assert!(gradient.high_frequency_ratio < 0.01, "{gradient:?}");
        assert_eq!(flat.high_frequency_ratio, 0.0);
    }

    #[test]
    fn negative_space_counts_flat_blocks() {
        // Left half flat, right half noisy.
        let texture = noise(320, 176, 5);
        let half = frame(320, 176, |x, y| {
            if x < 160 {
                grey(0.1)
            } else {
                texture.pixels()[y * 320 + x]
            }
        });
        let measured = legibility(&half);
        assert!(
            (measured.negative_space_ratio - 0.5).abs() < 0.01,
            "{measured:?}"
        );
        assert_eq!(legibility(&noise(320, 176, 5)).negative_space_ratio, 0.0);
    }

    #[test]
    fn hue_clusters_count_separated_colours() {
        let red = [0.9, 0.1, 0.1];
        let teal = [0.1, 0.8, 0.8];
        let blue = [0.1, 0.1, 0.9];
        let one = legibility(&frame(64, 64, |_, _| red));
        let two = legibility(&frame(64, 64, |x, _| if x < 32 { red } else { teal }));
        let three = legibility(&frame(63, 64, |x, _| match x / 21 {
            0 => red,
            1 => teal,
            _ => blue,
        }));
        let greyscale = legibility(&noise(64, 64, 9));

        assert_eq!(one.hue_clusters, 1);
        assert_eq!(two.hue_clusters, 2);
        assert_eq!(three.hue_clusters, 3);
        assert_eq!(greyscale.hue_clusters, 0);
    }

    #[test]
    fn neighbouring_hues_form_one_cluster_across_the_wheel_seam() {
        // Magenta-red and orange-red sit on either side of hue zero.
        let seam = legibility(&frame(64, 64, |x, _| {
            if x < 32 {
                [0.9, 0.1, 0.25]
            } else {
                [0.9, 0.25, 0.1]
            }
        }));
        assert_eq!(seam.hue_clusters, 1);
    }

    #[test]
    fn clipping_is_reported_per_end() {
        let measured = legibility(&frame(100, 8, |x, _| {
            grey(match x {
                0..10 => 1.0,
                10..40 => 0.0,
                _ => 0.5,
            })
        }));
        assert!((measured.clipped_high_ratio - 0.1).abs() < 1e-6);
        assert!((measured.clipped_low_ratio - 0.3).abs() < 1e-6);
    }

    #[test]
    fn identical_frames_have_no_difference() {
        let image = noise(64, 48, 11);
        let same = difference(&image, &image).unwrap();
        assert!((same.ssim - 1.0).abs() < 1e-5);
        assert_eq!(same.colour, 0.0);
        assert!(same.score() < 1e-5);
    }

    #[test]
    fn difference_grows_with_the_size_of_the_change() {
        let base = frame(64, 48, |x, y| {
            grey(((x / 8 + y / 8) % 2) as f32 * 0.6 + 0.2)
        });
        let texture = noise(64, 48, 13);
        let perturbed = |amount: f32| {
            frame(64, 48, |x, y| {
                let value = base.pixels()[y * 64 + x][0];
                grey((value + (texture.pixels()[y * 64 + x][0] - 0.5) * amount).clamp(0.0, 1.0))
            })
        };
        let slight = difference(&base, &perturbed(0.02)).unwrap().score();
        let heavy = difference(&base, &perturbed(0.5)).unwrap().score();
        assert!(slight < 0.02, "{slight}");
        assert!(heavy > 0.3, "{heavy}");
    }

    #[test]
    fn colour_shift_is_seen_even_when_structure_is_kept() {
        let base = frame(32, 32, |x, _| [x as f32 / 31.0, 0.3, 0.3]);
        let shifted = frame(32, 32, |x, _| [x as f32 / 31.0, 0.3, 0.6]);
        let measured = difference(&base, &shifted).unwrap();
        assert!((measured.colour - 0.1).abs() < 1e-4, "{measured:?}");
        assert!(measured.score() >= 0.1);
    }

    #[test]
    fn frames_of_different_sizes_cannot_be_compared() {
        assert!(difference(&noise(8, 8, 1), &noise(8, 9, 1)).is_err());
    }

    #[test]
    fn tiny_frames_are_measured_without_panicking() {
        let tiny = noise(3, 2, 1);
        let _ = legibility(&tiny);
        assert!(difference(&tiny, &tiny).unwrap().ssim > 0.999);
    }
}
