//! Derives axis values from artifacts. A film's axes are measured from what
//! was made, so a brief cannot claim diversity that the images do not show.

use fractal_renderer_core::AnimationPath;

use crate::{
    axes::{Light, Motion, Pacing, Palette, PaletteHue, PaletteKey, ScaleRegime},
    metrics::Frame,
    novelty::hue_degrees,
    shape::Survey,
    tuning::Classification,
};

/// Classifies the dominant hue and brightness of a film's frames.
#[must_use]
pub fn palette(frames: &[Frame], rules: &Classification) -> Palette {
    let mut weights = [0.0f32; PaletteHue::ALL.len()];
    let (mut coloured, mut pixels, mut luminance) = (0usize, 0usize, 0.0f32);
    for frame in frames {
        pixels += frame.pixels().len();
        luminance += frame.luma().iter().sum::<f32>();
        for pixel in frame.pixels() {
            let Some(hue) = hue_degrees(*pixel) else {
                continue;
            };
            let max = pixel[0].max(pixel[1]).max(pixel[2]);
            let min = pixel[0].min(pixel[1]).min(pixel[2]);
            // Vivid, bright pixels define a palette more than murky ones.
            weights[hue_class(hue) as usize] += max - min;
            coloured += 1;
        }
    }
    let pixels = pixels.max(1) as f32;
    let primary_hue = if (coloured as f32) < rules.min_coloured_share * pixels {
        PaletteHue::Neutral
    } else {
        let strongest = weights
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(index, _)| index);
        PaletteHue::ALL[strongest]
    };
    let mean_luminance = luminance / pixels;
    let key = if mean_luminance < rules.low_key_luminance {
        PaletteKey::Low
    } else if mean_luminance < rules.high_key_luminance {
        PaletteKey::Mid
    } else {
        PaletteKey::High
    };
    Palette { primary_hue, key }
}

fn hue_class(degrees: f32) -> PaletteHue {
    match degrees {
        hue if !(20.0..345.0).contains(&hue) => PaletteHue::Red,
        hue if hue < 65.0 => PaletteHue::Amber,
        hue if hue < 160.0 => PaletteHue::Green,
        hue if hue < 200.0 => PaletteHue::Teal,
        hue if hue < 255.0 => PaletteHue::Blue,
        _ => PaletteHue::Violet,
    }
}

/// Classifies the key light from its direction relative to the view.
/// `light` points from the surface towards the light; `view` is the camera's
/// forward direction.
#[must_use]
pub fn light(light: [f64; 3], view: [f64; 3], up: [f64; 3], rules: &Classification) -> Light {
    let light = normalize(light);
    let towards_camera = -dot(light, normalize(view));
    if towards_camera > rules.frontal_cosine {
        Light::Frontal
    } else if towards_camera < rules.backlit_cosine {
        Light::Backlit
    } else if dot(light, normalize(up)) > rules.top_cosine {
        Light::Top
    } else {
        Light::Raking
    }
}

#[must_use]
pub fn pacing(mean_cut_seconds: f64, rules: &Classification) -> Pacing {
    if mean_cut_seconds >= rules.slow_cut_seconds {
        Pacing::Slow
    } else if mean_cut_seconds >= rules.medium_cut_seconds {
        Pacing::Medium
    } else {
        Pacing::Fast
    }
}

/// Classifies a camera position. A close-up is recognised by how little
/// room the camera has; an overview by the camera standing outside the
/// shape. Clearance alone would not do for the latter: distance estimates
/// are lower bounds and understate how far away a surface is.
#[must_use]
pub fn scale_regime(
    clearance: f64,
    position: [f64; 3],
    survey: &Survey,
    rules: &Classification,
) -> ScaleRegime {
    if clearance / survey.working_radius() < rules.deep_clearance {
        ScaleRegime::Deep
    } else if survey.bounded
        && dot(position, position).sqrt() >= rules.overview_radii * survey.radius
    {
        ScaleRegime::Overview
    } else {
        ScaleRegime::Mid
    }
}

#[must_use]
pub fn motion(path: &AnimationPath) -> Motion {
    match path {
        AnimationPath::ExponentialDive(_) => Motion::Dive,
        AnimationPath::TargetOrbit(_) => Motion::Orbit,
        AnimationPath::MultiTargetDive(_) => Motion::MultiDive,
        AnimationPath::SurfaceFlyover(_) => Motion::Flyover,
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let length = dot(a, a).sqrt();
    if length > 0.0 {
        a.map(|value| value / length)
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette(frames: &[Frame]) -> Palette {
        super::palette(frames, &Classification::default())
    }

    fn light(light: [f64; 3], view: [f64; 3], up: [f64; 3]) -> Light {
        super::light(light, view, up, &Classification::default())
    }

    fn pacing(mean_cut_seconds: f64) -> Pacing {
        super::pacing(mean_cut_seconds, &Classification::default())
    }

    fn scale_regime(clearance: f64, position: [f64; 3], survey: &Survey) -> ScaleRegime {
        super::scale_regime(clearance, position, survey, &Classification::default())
    }
    use crate::metrics::tests::frame;

    fn solid(pixel: [f32; 3]) -> Frame {
        frame(32, 32, |_, _| pixel)
    }

    #[test]
    fn palette_names_the_dominant_hue() {
        for (pixel, expected) in [
            ([0.9, 0.1, 0.1], PaletteHue::Red),
            ([0.9, 0.1, 0.2], PaletteHue::Red),
            ([0.9, 0.6, 0.1], PaletteHue::Amber),
            ([0.2, 0.8, 0.2], PaletteHue::Green),
            ([0.1, 0.7, 0.7], PaletteHue::Teal),
            ([0.1, 0.3, 0.9], PaletteHue::Blue),
            ([0.6, 0.1, 0.9], PaletteHue::Violet),
            ([0.5, 0.5, 0.5], PaletteHue::Neutral),
        ] {
            assert_eq!(palette(&[solid(pixel)]).primary_hue, expected, "{pixel:?}");
        }
    }

    #[test]
    fn a_small_accent_does_not_define_the_palette() {
        let mostly_grey = frame(100, 10, |x, _| {
            if x < 5 {
                [0.9, 0.1, 0.1]
            } else {
                [0.4, 0.4, 0.4]
            }
        });
        assert_eq!(palette(&[mostly_grey]).primary_hue, PaletteHue::Neutral);

        // Across frames, the hue with more vivid area wins.
        let teal = solid([0.1, 0.7, 0.7]);
        let dull_red = solid([0.4, 0.28, 0.28]);
        assert_eq!(palette(&[teal, dull_red]).primary_hue, PaletteHue::Teal);
    }

    #[test]
    fn palette_key_follows_mean_luminance() {
        assert_eq!(palette(&[solid([0.1; 3])]).key, PaletteKey::Low);
        assert_eq!(palette(&[solid([0.4; 3])]).key, PaletteKey::Mid);
        assert_eq!(palette(&[solid([0.8; 3])]).key, PaletteKey::High);
        assert_eq!(palette(&[]).primary_hue, PaletteHue::Neutral);
    }

    #[test]
    fn light_is_classified_relative_to_the_view() {
        let view = [0.0, 1.0, 0.0];
        let up = [0.0, 0.0, 1.0];
        assert_eq!(light([0.0, -1.0, 0.1], view, up), Light::Frontal);
        assert_eq!(light([0.0, 1.0, 0.3], view, up), Light::Backlit);
        assert_eq!(light([0.1, -0.2, 1.0], view, up), Light::Top);
        assert_eq!(light([1.0, -0.2, 0.2], view, up), Light::Raking);
        // Length of the inputs must not matter.
        assert_eq!(light([0.0, -5.0, 0.5], [0.0, 3.0, 0.0], up), Light::Frontal);
    }

    #[test]
    fn pacing_follows_mean_cut_length() {
        assert_eq!(pacing(15.0), Pacing::Slow);
        assert_eq!(pacing(12.0), Pacing::Slow);
        assert_eq!(pacing(8.0), Pacing::Medium);
        assert_eq!(pacing(4.0), Pacing::Fast);
    }

    #[test]
    fn scale_regime_follows_relative_clearance() {
        let bounded = Survey {
            bounded: true,
            radius: 2.0,
        };
        let inside = [0.5, 0.0, 0.0];
        let outside = [0.0, 3.5, 0.0];
        assert_eq!(scale_regime(0.05, inside, &bounded), ScaleRegime::Deep);
        assert_eq!(scale_regime(0.5, inside, &bounded), ScaleRegime::Mid);
        assert_eq!(scale_regime(0.5, outside, &bounded), ScaleRegime::Overview);
        // An underestimated clearance does not hide an outside camera, but
        // a camera skimming an outlying surface is still a close-up.
        assert_eq!(scale_regime(0.2, outside, &bounded), ScaleRegime::Overview);
        assert_eq!(scale_regime(0.01, outside, &bounded), ScaleRegime::Deep);

        // A space-filling shape cannot be seen from outside.
        let unbounded = Survey {
            bounded: false,
            radius: 3.0,
        };
        assert_eq!(
            scale_regime(2.5, [9.0, 0.0, 0.0], &unbounded),
            ScaleRegime::Mid
        );
        assert_eq!(
            scale_regime(0.1, [9.0, 0.0, 0.0], &unbounded),
            ScaleRegime::Deep
        );
    }
}
