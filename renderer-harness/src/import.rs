//! Registers films made before the harness existed. Without them the ledger
//! would start empty and the diversity rules would have nothing to steer
//! away from.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::load_scene;

use crate::{
    axes::{Axes, classify_family},
    classify,
    gpu::{GpuOptions, key_frames, render_thumbnails},
    ledger::{Ledger, LedgerEntry},
    novelty,
    production::validate_id,
    rng::Rng,
    shape::{self, Survey},
    sheet,
    tuning::Classification,
};

#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub id: String,
    pub title: String,
    /// ISO 8601 calendar date the film was finished.
    pub date: String,
    /// GPU minutes the film took, when known.
    pub gpu_minutes: Option<f64>,
    pub gpu: GpuOptions,
}

/// Classifies a finished film from its scene file, renders its keyframes,
/// and appends it to the ledger.
pub fn import(
    scene_path: &Path,
    ledger_path: &Path,
    ledger: &mut Ledger,
    options: &ImportOptions,
    rules: &Classification,
) -> Result<LedgerEntry> {
    validate_id(&options.id)?;
    validate_date(&options.date)?;
    if options.title.trim().is_empty() {
        bail!("an imported film needs a title");
    }
    if ledger.entries().iter().any(|entry| entry.id == options.id) {
        bail!("the ledger already contains film id {:?}", options.id);
    }
    let scene = load_scene(scene_path)?;
    let Some(animation) = &scene.animation else {
        bail!(
            "{} has no animation; only films can be imported",
            scene_path.display()
        );
    };

    let frames = key_frames(&scene);
    let (thumbnails, _) = render_thumbnails(&scene, &frames, &options.gpu)?;
    let images = thumbnails
        .iter()
        .map(|thumbnail| thumbnail.frame())
        .collect::<Result<Vec<_>>>()?;

    // The middle keyframe stands for the film's typical framing.
    let middle = &thumbnails[thumbnails.len() / 2].config;
    let position = middle.camera.position.to_f64();
    let target = middle.camera.target.to_f64();
    let view = [
        target[0] - position[0],
        target[1] - position[1],
        target[2] - position[2],
    ];
    // A shape the survey cannot measure is treated like a space-filling one,
    // which only ever yields interior regimes.
    let survey = shape::survey(&mut Rng::new(0), &scene.config.fractal).unwrap_or(Survey {
        bounded: false,
        radius: f64::INFINITY,
    });
    let duration_seconds = f64::from(animation.frame_count) / f64::from(animation.fps);
    let axes = Axes {
        formula_family: classify_family(&scene.config.fractal),
        scale_regime: classify::scale_regime(
            shape::clearance(&scene.config.fractal, position),
            position,
            &survey,
            rules,
        ),
        motion: classify::motion(&animation.path),
        palette: classify::palette(&images, rules),
        light: classify::light(
            middle.light.direction.map(f64::from),
            view,
            middle.camera.up.map(f64::from),
            rules,
        ),
        // A film without cuts is paced by its single shot.
        pacing: classify::pacing(duration_seconds, rules),
    };

    let keyframe_directory = ledger_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("keyframes")
        .join(&options.id);
    fs::create_dir_all(&keyframe_directory)
        .with_context(|| format!("could not create {}", keyframe_directory.display()))?;
    let mut keyframes = Vec::with_capacity(thumbnails.len());
    for thumbnail in &thumbnails {
        let path = keyframe_directory.join(format!("frame-{:06}.png", thumbnail.frame_index));
        sheet::save_png(
            &path,
            thumbnail.image.width(),
            thumbnail.image.height(),
            thumbnail.image.pixels(),
        )?;
        keyframes.push(path.to_string_lossy().into_owned());
    }

    let entry = LedgerEntry {
        id: options.id.clone(),
        date: options.date.clone(),
        title: options.title.trim().to_owned(),
        axes,
        descriptors: images.iter().map(novelty::describe).collect(),
        keyframes,
        duration_seconds,
        gpu_minutes: options.gpu_minutes,
        compromised: false,
    };
    ledger.append(ledger_path, entry.clone())?;
    Ok(entry)
}

/// Accepts `YYYY-MM-DD` with plausible month and day numbers.
fn validate_date(date: &str) -> Result<()> {
    let parts: Vec<&str> = date.split('-').collect();
    let well_formed = parts.len() == 3
        && [4, 2, 2].iter().zip(&parts).all(|(length, part)| {
            part.len() == *length && part.bytes().all(|byte| byte.is_ascii_digit())
        })
        && (1..=12).contains(&parts[1].parse::<u32>().unwrap_or(0))
        && (1..=31).contains(&parts[2].parse::<u32>().unwrap_or(0));
    if !well_formed {
        bail!("date {date:?} must be written as YYYY-MM-DD");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(id: &str, title: &str, date: &str) -> ImportOptions {
        ImportOptions {
            id: id.to_owned(),
            title: title.to_owned(),
            date: date.to_owned(),
            gpu_minutes: None,
            gpu: GpuOptions::default(),
        }
    }

    #[test]
    fn dates_must_be_iso_calendar_dates() {
        for valid in ["2026-11-15", "2026-01-31"] {
            validate_date(valid).unwrap();
        }
        for invalid in [
            "2026-13-01",
            "2026-00-10",
            "2026-1-5",
            "26-11-15",
            "2026/11/15",
            "",
        ] {
            assert!(validate_date(invalid).is_err(), "{invalid:?} was accepted");
        }
    }

    #[test]
    fn requests_are_validated_before_anything_is_rendered() {
        let ledger_path = Path::new("/nonexistent/ledger.jsonl");
        let scene = Path::new("/nonexistent/scene.yaml");
        let mut ledger = Ledger::default();
        let mut attempt = |options: ImportOptions| {
            format!(
                "{:#}",
                import(
                    scene,
                    ledger_path,
                    &mut ledger,
                    &options,
                    &Classification::default()
                )
                .unwrap_err()
            )
        };
        assert!(attempt(options("Bad Id", "t", "2026-11-15")).contains("production id"));
        assert!(attempt(options("film", "t", "yesterday")).contains("YYYY-MM-DD"));
        assert!(attempt(options("film", "  ", "2026-11-15")).contains("title"));
        assert!(attempt(options("film", "t", "2026-11-15")).contains("scene.yaml"));
    }

    #[test]
    fn stills_are_not_films() {
        let scene = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../scenes/examples/mandelbulb.yaml"
        );
        let error = import(
            Path::new(scene),
            Path::new("/nonexistent/ledger.jsonl"),
            &mut Ledger::default(),
            &options("film", "t", "2026-11-15"),
            &Classification::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("no animation"));
    }
}
