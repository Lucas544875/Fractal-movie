//! Calibration of the legibility thresholds against films people judged.
//!
//! The frames of a reference film are measured exactly as the harness
//! measures its own, and the spread of every measure is reported. A threshold
//! that would fail a film people accept is wrong, whatever it was meant to
//! catch.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::metrics::{Frame, flicker, flicker_luma, legibility};

/// Percentiles reported for every measure.
const PERCENTILES: [f64; 7] = [0.0, 0.05, 0.25, 0.5, 0.75, 0.95, 1.0];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Spread {
    pub min: f32,
    pub p05: f32,
    pub p25: f32,
    pub median: f32,
    pub p75: f32,
    pub p95: f32,
    pub max: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Calibration {
    pub frames: usize,
    /// Frames measured for legibility, every `stride`-th one.
    pub measured: usize,
    pub high_frequency_ratio: Spread,
    pub negative_space_ratio: Spread,
    pub hue_clusters: Spread,
    pub clipped_high_ratio: Spread,
    pub clipped_low_ratio: Spread,
    pub mean_luminance: Spread,
    /// Over every run of three consecutive frames.
    pub flicker: Spread,
}

fn spread(mut values: Vec<f32>) -> Spread {
    values.sort_by(f32::total_cmp);
    let at = |share: f64| values[((values.len() - 1) as f64 * share).round() as usize];
    let [min, p05, p25, median, p75, p95, max] = PERCENTILES.map(at);
    Spread {
        min,
        p05,
        p25,
        median,
        p75,
        p95,
        max,
    }
}

fn load(path: &Path) -> Result<Frame> {
    let image = image::open(path)
        .with_context(|| format!("could not read {}", path.display()))?
        .into_rgba8();
    Frame::from_rgba8(image.width(), image.height(), image.as_raw())
}

/// Measures consecutive frames, as PNG files in name order. `skip` frames
/// are left out at each end, for fades.
pub fn calibrate(directory: &Path, stride: usize, skip: usize) -> Result<Calibration> {
    if stride == 0 {
        bail!("stride must be at least 1");
    }
    let mut files: Vec<_> = fs::read_dir(directory)
        .with_context(|| format!("could not list {}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
        .collect();
    files.sort();
    if files.len() < 2 * skip + 3 {
        bail!(
            "{} holds {} frames; at least {} are needed",
            directory.display(),
            files.len(),
            2 * skip + 3
        );
    }
    let files = &files[skip..files.len() - skip];

    let mut measures: [Vec<f32>; 6] = Default::default();
    let mut flickers = Vec::with_capacity(files.len());
    let mut window: Vec<Vec<f32>> = Vec::with_capacity(3);
    for (index, file) in files.iter().enumerate() {
        let frame = load(file)?;
        if index % stride == 0 {
            let measured = legibility(&frame);
            for (list, value) in measures.iter_mut().zip([
                measured.high_frequency_ratio,
                measured.negative_space_ratio,
                measured.hue_clusters as f32,
                measured.clipped_high_ratio,
                measured.clipped_low_ratio,
                measured.mean_luminance,
            ]) {
                list.push(value);
            }
        }
        window.push(flicker_luma(&frame));
        if window.len() == 3 {
            flickers.push(flicker(&window[0], &window[1], &window[2]));
            window.remove(0);
        }
    }

    let measured = measures[0].len();
    let [high, negative, hues, clipped_high, clipped_low, luminance] = measures.map(spread);
    Ok(Calibration {
        frames: files.len(),
        measured,
        high_frequency_ratio: high,
        negative_space_ratio: negative,
        hue_clusters: hues,
        clipped_high_ratio: clipped_high,
        clipped_low_ratio: clipped_low,
        mean_luminance: luminance,
        flicker: spread(flickers),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_reports_ordered_percentiles() {
        let measured = spread((0..=100).map(|value| value as f32).collect());
        assert_eq!(measured.min, 0.0);
        assert_eq!(measured.p05, 5.0);
        assert_eq!(measured.median, 50.0);
        assert_eq!(measured.p95, 95.0);
        assert_eq!(measured.max, 100.0);
    }

    #[test]
    fn measures_a_directory_of_frames_and_skips_the_ends() {
        let directory =
            std::env::temp_dir().join(format!("fractal-calibrate-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        for index in 0..8_u8 {
            // A steady gradient: no flicker, no clipping in the middle.
            let image = image::RgbaImage::from_fn(64, 36, |x, _| {
                let value = 40 + (x as u8) * 2;
                image::Rgba([value, value, value, 255])
            });
            image
                .save(directory.join(format!("frame_{index:03}.png")))
                .unwrap();
        }
        let calibration = calibrate(&directory, 2, 1).unwrap();
        fs::remove_dir_all(&directory).unwrap();
        assert_eq!(calibration.frames, 6);
        assert_eq!(calibration.measured, 3);
        assert_eq!(calibration.flicker.max, 0.0);
        assert_eq!(calibration.clipped_high_ratio.max, 0.0);
        assert!(calibrate(Path::new("/nonexistent"), 1, 0).is_err());
    }
}
