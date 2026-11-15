//! The animatic: every frame of the film at thumbnail size, cut together
//! with its transitions. It is the main thing reviewed at the shot-list
//! stage, because pacing and contrast between cuts only show in motion.

use std::fs;

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{Renderer, load_scene};
use serde::Serialize;

use crate::{
    budget::Budget,
    film::{Compositor, Encoding, FilmWriter},
    gpu::{GpuOptions, apply_settings, frame_config},
    metrics::{Frame, LEGIBILITY_WIDTH, Legibility, legibility},
    production::Production,
    quality::Settings,
    sheet::{self, Cell},
    shotlist::Manifest,
    stage::{Stage, State},
    tuning::LegibilityLimits,
};

pub const ANIMATIC_FILE: &str = "animatic/animatic.mp4";
pub const SHEET_FILE: &str = "animatic/sheet.png";
pub const REPORT_FILE: &str = "animatic/report.json";

/// Gates that must have passed before an animatic is worth rendering.
const PREREQUISITES: [&str; 3] = ["schema", "route-safety", "in-film-variety"];
/// Legibility is measured on this many frames per second of film.
const MEASURES_PER_SECOND: u32 = 2;

#[derive(Clone, Debug, Serialize)]
pub struct CutMeasurement {
    pub id: u32,
    /// Legibility averaged over the cut; hue clusters report the maximum.
    pub legibility: Legibility,
    /// Mean deviation of each frame from the average of its neighbours.
    /// Smooth camera motion scores near zero; shimmering detail does not.
    pub flicker: f32,
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AnimaticReport {
    pub video: &'static str,
    pub sheet: &'static str,
    pub frames: usize,
    pub duration_seconds: f64,
    pub cuts: Vec<CutMeasurement>,
    pub legible: bool,
    pub gpu_seconds: f64,
    pub design_remaining_gpu_seconds: f64,
}

/// Renders the animatic and judges the legibility gate.
pub fn run(
    production: &Production,
    state: &mut State,
    budget: &mut Budget,
    gpu: &GpuOptions,
) -> Result<AnimaticReport> {
    if state.stage != Stage::Shotlist {
        bail!(
            "the animatic is rendered in stage shotlist; the production is in stage {}",
            state.stage.as_str()
        );
    }
    let unmet: Vec<&str> = PREREQUISITES
        .into_iter()
        .filter(|gate| !state.gates.get(*gate).is_some_and(|record| record.passed))
        .collect();
    if !unmet.is_empty() {
        bail!(
            "the shot list must pass `shotlist check` first; gates not passed: {}",
            unmet.join(", ")
        );
    }
    let manifest = Manifest::load_current(production)?;
    let limits = production.load_tuning()?.legibility;
    if budget.design_remaining_seconds() <= 0.0 {
        bail!("the design GPU budget of this production is exhausted");
    }
    let duty_cycle = gpu.duty_cycle_percent.unwrap_or(budget.max_gpu_duty_cycle);
    budget.check_duty_cycle(duty_cycle)?;
    let gpu = GpuOptions {
        duty_cycle_percent: Some(duty_cycle),
        ..gpu.clone()
    };

    let format = manifest.format;
    let width = LEGIBILITY_WIDTH;
    // Even height keeps the encoder's chroma subsampling aligned.
    let height =
        (u64::from(format.height) * u64::from(width) / u64::from(format.width)) as u32 & !1;
    let settings = Settings {
        samples_per_pixel: 1,
        ambient_occlusion_steps: 0,
        soft_shadow_steps: 0,
        reflection_steps: 0,
        depth_of_field: false,
        pixel_epsilon_multiplier: 1.0,
    };
    let measure_every = (format.fps / MEASURES_PER_SECOND).max(1);

    let mut writer = FilmWriter::create(
        &production.path(ANIMATIC_FILE),
        &Encoding {
            width,
            height,
            fps: format.fps,
            crf: 23,
            preset: "veryfast",
        },
    )?;
    let mut compositor = Compositor::new(manifest.transition_frames as usize, |rgba: &[u8]| {
        writer.write_frame(rgba)
    });

    let mut renderer: Option<Renderer> = None;
    let mut gpu_seconds = 0.0;
    let mut cuts = Vec::with_capacity(manifest.cuts.len());
    // Start, middle, and end of every cut, for the contact sheet.
    let mut stills: Vec<(String, Vec<u8>)> = Vec::new();

    for cut in &manifest.cuts {
        let scene = load_scene(production.path(&cut.scene))?;
        let mut base = scene.config.clone();
        base.render.width = width;
        base.render.height = height;
        match &mut renderer {
            Some(renderer) => pollster::block_on(renderer.replace_fractal(base.clone()))
                .with_context(|| format!("could not compile cut {}", cut.id))?,
            None => renderer = Some(gpu.renderer(base.clone())?),
        }
        let renderer = renderer.as_ref().expect("renderer was just initialised");

        compositor.begin_cut(cut.frame_count as usize, cut.transition_out)?;
        let last = cut.frame_count - 1;
        let mut measured: Vec<Legibility> = Vec::new();
        let mut history: Vec<Vec<f32>> = Vec::with_capacity(2);
        let (mut flicker_sum, mut flicker_count) = (0.0f64, 0u32);

        for index in 0..cut.frame_count {
            let (mut config, time_seconds) = frame_config(&base, scene.animation.as_ref(), index)?;
            apply_settings(&mut config, &settings);
            let image = renderer
                .render_frame_with_config(&config, index, time_seconds)
                .with_context(|| format!("could not render cut {} frame {index}", cut.id))?;
            gpu_seconds += image.gpu_time().as_secs_f64();
            let frame = Frame::from_rgba8(image.width(), image.height(), image.pixels())?;

            if index % measure_every == 0 || index == last {
                measured.push(legibility(&frame));
            }
            // Halving removes single-pixel aliasing that any 1 spp render
            // has, leaving the instability a viewer would actually see.
            let luma = frame.reduced(2).luma();
            if let [before, middle] = history.as_slice() {
                flicker_sum += f64::from(flicker(before, middle, &luma));
                flicker_count += 1;
            }
            if history.len() == 2 {
                history.remove(0);
            }
            history.push(luma);

            if [0, last / 2, last].contains(&index) {
                let position = stills
                    .iter()
                    .filter(|(label, _)| label.starts_with(&format!("{}-", cut.id)))
                    .count()
                    + 1;
                stills.push((format!("{}-{position}", cut.id), image.pixels().to_vec()));
            }
            compositor.push_frame(image.pixels().to_vec())?;
        }

        let mean = mean_legibility(&measured);
        let flicker = if flicker_count == 0 {
            0.0
        } else {
            (flicker_sum / f64::from(flicker_count)) as f32
        };
        cuts.push(CutMeasurement {
            id: cut.id,
            problems: problems(&mean, flicker, &limits),
            legibility: mean,
            flicker,
        });
    }

    let frames = compositor.finish()?;
    writer.finish()?;

    let cells: Vec<Cell<'_>> = stills
        .iter()
        .map(|(label, rgba)| Cell { label, rgba })
        .collect();
    sheet::compose(&cells, 3, width as usize, height as usize)?
        .save_png(&production.path(SHEET_FILE))?;

    let failures: Vec<String> = cuts
        .iter()
        .flat_map(|cut| {
            cut.problems
                .iter()
                .map(move |problem| format!("cut {}: {problem}", cut.id))
        })
        .collect();
    state.record_gate(
        "legibility",
        failures.is_empty(),
        if failures.is_empty() {
            "every cut reads at thumbnail size".to_owned()
        } else {
            failures.join("; ")
        },
    )?;
    budget.spent.design_gpu_seconds += gpu_seconds;
    production.save_budget(budget)?;

    let report = AnimaticReport {
        video: ANIMATIC_FILE,
        sheet: SHEET_FILE,
        frames,
        duration_seconds: frames as f64 / f64::from(format.fps),
        legible: failures.is_empty(),
        cuts,
        gpu_seconds,
        design_remaining_gpu_seconds: budget.design_remaining_seconds(),
    };
    let mut json = serde_json::to_string_pretty(&report).context("could not serialize report")?;
    json.push('\n');
    fs::write(production.path(REPORT_FILE), json).context("could not write the animatic report")?;
    Ok(report)
}

/// Mean absolute deviation of the middle frame from the average of its
/// neighbours.
fn flicker(before: &[f32], middle: &[f32], after: &[f32]) -> f32 {
    let total: f32 = before
        .iter()
        .zip(middle)
        .zip(after)
        .map(|((before, middle), after)| (middle - (before + after) / 2.0).abs())
        .sum();
    total / middle.len().max(1) as f32
}

fn mean_legibility(measured: &[Legibility]) -> Legibility {
    let count = measured.len().max(1) as f32;
    let mean = |field: fn(&Legibility) -> f32| measured.iter().map(field).sum::<f32>() / count;
    Legibility {
        high_frequency_ratio: mean(|value| value.high_frequency_ratio),
        negative_space_ratio: mean(|value| value.negative_space_ratio),
        hue_clusters: measured
            .iter()
            .map(|value| value.hue_clusters)
            .max()
            .unwrap_or(0),
        clipped_high_ratio: mean(|value| value.clipped_high_ratio),
        clipped_low_ratio: mean(|value| value.clipped_low_ratio),
        mean_luminance: mean(|value| value.mean_luminance),
    }
}

fn problems(mean: &Legibility, flicker: f32, limits: &LegibilityLimits) -> Vec<String> {
    let mut problems = Vec::new();
    if mean.mean_luminance < limits.min_mean_luminance {
        problems.push(format!(
            "too dark to read (mean luminance {:.3})",
            mean.mean_luminance
        ));
    }
    if mean.negative_space_ratio > limits.max_negative_space {
        problems.push(format!(
            "almost nothing in frame ({:.0}% of the frame is flat)",
            mean.negative_space_ratio * 100.0
        ));
    }
    if mean.negative_space_ratio < limits.min_negative_space {
        problems.push(format!(
            "detail fills the frame (only {:.0}% of the frame is flat; at least {:.0}% is needed)",
            mean.negative_space_ratio * 100.0,
            limits.min_negative_space * 100.0
        ));
    }
    if mean.high_frequency_ratio > limits.max_high_frequency {
        problems.push(format!(
            "too much fine detail (high-frequency ratio {:.2}; at most {})",
            mean.high_frequency_ratio, limits.max_high_frequency
        ));
    }
    if flicker > limits.max_flicker {
        problems.push(format!(
            "shimmers in motion (flicker {flicker:.4}; at most {})",
            limits.max_flicker
        ));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shotlist::tests::{VALID_SHOTLIST, shotlist_stage, write_shotlist};

    fn readable() -> Legibility {
        Legibility {
            high_frequency_ratio: 0.1,
            negative_space_ratio: 0.4,
            hue_clusters: 1,
            clipped_high_ratio: 0.0,
            clipped_low_ratio: 0.0,
            mean_luminance: 0.4,
        }
    }

    #[test]
    fn flicker_ignores_steady_change_and_sees_instability() {
        let ramp = |value: f32| vec![value; 16];
        // A steady brightening is motion, not flicker.
        assert!(flicker(&ramp(0.2), &ramp(0.3), &ramp(0.4)) < 1.0e-6);
        // One frame jumping out of line is.
        assert!((flicker(&ramp(0.2), &ramp(0.5), &ramp(0.2)) - 0.3).abs() < 1.0e-6);
    }

    #[test]
    fn legibility_is_averaged_with_the_worst_hue_count() {
        let mut busy = readable();
        busy.negative_space_ratio = 0.0;
        busy.hue_clusters = 4;
        let mean = mean_legibility(&[readable(), busy]);
        assert!((mean.negative_space_ratio - 0.2).abs() < 1.0e-6);
        assert_eq!(mean.hue_clusters, 4);
    }

    #[test]
    fn each_problem_is_named() {
        assert!(problems(&readable(), 0.0, &LegibilityLimits::default()).is_empty());
        let check = |edit: fn(&mut Legibility), flicker: f32, expected: &str| {
            let mut measured = readable();
            edit(&mut measured);
            let found = problems(&measured, flicker, &LegibilityLimits::default()).join("\n");
            assert!(found.contains(expected), "{found}");
        };
        check(|value| value.mean_luminance = 0.01, 0.0, "too dark");
        check(
            |value| value.negative_space_ratio = 0.95,
            0.0,
            "almost nothing",
        );
        check(
            |value| value.negative_space_ratio = 0.02,
            0.0,
            "detail fills",
        );
        check(|value| value.high_frequency_ratio = 0.6, 0.0, "fine detail");
        check(
            |_| {},
            LegibilityLimits::default().max_flicker * 2.0,
            "shimmers",
        );
    }

    #[test]
    fn problems_follow_the_limits_given() {
        let strict = LegibilityLimits {
            max_flicker: 0.001,
            max_high_frequency: 0.05,
            ..LegibilityLimits::default()
        };
        let found = problems(&readable(), 0.005, &strict).join("\n");
        assert!(
            found.contains("shimmers") && found.contains("at most 0.001"),
            "{found}"
        );
        assert!(
            found.contains("fine detail") && found.contains("at most 0.05"),
            "{found}"
        );
        assert!(problems(&readable(), 0.005, &LegibilityLimits::default()).is_empty());
    }

    #[test]
    fn an_animatic_needs_a_checked_unchanged_shot_list() {
        let (production, mut state, mut budget, root) = shotlist_stage("animatic");
        let gpu = GpuOptions::default();
        write_shotlist(&production, VALID_SHOTLIST);

        let error = run(&production, &mut state, &mut budget, &gpu).unwrap_err();
        assert!(error.to_string().contains("shotlist check"), "{error:#}");

        crate::shotlist::check(&production, &mut state, &mut budget).unwrap();
        write_shotlist(&production, &format!("{VALID_SHOTLIST}# edited\n"));
        let error = run(&production, &mut state, &mut budget, &gpu).unwrap_err();
        assert!(error.to_string().contains("changed after it was checked"));

        write_shotlist(&production, VALID_SHOTLIST);
        let error = run(
            &production,
            &mut state,
            &mut budget,
            &GpuOptions {
                duty_cycle_percent: Some(99.0),
                ..GpuOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("duty cycle"));
        assert!(!production.path(ANIMATIC_FILE).exists());

        std::fs::remove_dir_all(root).unwrap();
    }
}
