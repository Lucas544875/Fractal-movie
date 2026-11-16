//! The storyboard: the first and last frame of every beat of the brief,
//! drawn cheaply in neutral grey. It lets the critic judge whether the
//! concept can be told with the vocabulary before any cut is planned, which
//! is where an unfilmable idea is cheapest to drop.

use std::fs;

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{Renderer, load_scene};
use serde::Serialize;

use crate::{
    axes::Motion,
    brief::{self, BRIEF_FILE, PROBE_FORMAT},
    budget::Budget,
    cut,
    gpu::{GpuOptions, apply_settings, frame_config},
    production::Production,
    quality::Settings,
    scout::{self, Index},
    sheet::{self, Cell},
    stage::{Stage, State},
};

pub const SHEET_FILE: &str = "storyboard/sheet.png";
pub const REPORT_FILE: &str = "storyboard/report.json";

/// Gates that must have passed before beats are worth drawing.
const PREREQUISITES: [&str; 4] = ["schema", "mandate", "axis-diff", "beats"];

#[derive(Clone, Debug, Serialize)]
pub struct BeatFrame {
    pub beat: usize,
    pub text: String,
    pub motion: Motion,
    pub location: String,
    pub viewpoint: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StoryboardReport {
    pub sheet: &'static str,
    /// Labels on the sheet read "beat-1" for the first frame of a beat and
    /// "beat-2" for its last.
    pub beats: Vec<BeatFrame>,
    pub gpu_seconds: f64,
}

/// Draws the first and last frame of every beat and records the
/// storyboard gate.
pub fn run(
    production: &Production,
    state: &mut State,
    budget: &mut Budget,
    gpu: &GpuOptions,
) -> Result<StoryboardReport> {
    if state.stage != Stage::Brief {
        bail!(
            "the storyboard is drawn in stage brief; the production is in stage {}",
            state.stage.as_str()
        );
    }
    let unmet: Vec<&str> = PREREQUISITES
        .into_iter()
        .filter(|gate| !state.gates.get(*gate).is_some_and(|record| record.passed))
        .collect();
    if !unmet.is_empty() {
        bail!(
            "the brief must pass `brief check` first; gates not passed: {}",
            unmet.join(", ")
        );
    }
    let brief = brief::load_checked(production)?;
    let tuning = production.load_tuning()?;
    let index = Index::load(production)?;
    if budget.design_remaining_seconds() <= 0.0 {
        bail!("the design GPU budget of this production is exhausted");
    }
    let duty_cycle = gpu.duty_cycle_percent.unwrap_or(budget.max_gpu_duty_cycle);
    budget.check_duty_cycle(duty_cycle)?;
    let gpu = GpuOptions {
        duty_cycle_percent: Some(duty_cycle),
        ..gpu.clone()
    };
    production.archive("storyboard")?;
    production.snapshot_inputs("storyboard", &[BRIEF_FILE])?;
    fs::create_dir_all(production.path("storyboard"))
        .context("could not create the storyboard directory")?;

    let rules = tuning.storyboard;
    let width = rules.width;
    let height = (u64::from(PROBE_FORMAT.height) * u64::from(width) / u64::from(PROBE_FORMAT.width))
        as u32
        & !1;
    let settings = Settings {
        samples_per_pixel: rules.samples_per_pixel,
        ambient_occlusion_steps: 0,
        soft_shadow_steps: 0,
        reflection_steps: 0,
        depth_of_field: false,
        pixel_epsilon_multiplier: 1.0,
    };

    let mut renderer: Option<Renderer> = None;
    let mut gpu_seconds = 0.0;
    let mut cells: Vec<(String, Vec<u8>)> = Vec::new();
    let mut beats = Vec::with_capacity(brief.beats.len());
    for (position, beat) in brief.beats.iter().enumerate() {
        let number = position + 1;
        let record = &index.locations[&beat.location];
        let viewpoint = load_scene(scout::viewpoint_scene_path(
            production,
            &beat.location,
            &beat.viewpoint,
        ))?;
        // The viewpoint keeps its neutral scouting material: the look is
        // not decided yet, and form is what this stage is about.
        let scene = cut::compile(
            &viewpoint,
            &record.survey(),
            &beat.motion,
            rules.beat_seconds,
            &PROBE_FORMAT,
            "beat",
            &tuning,
        )
        .with_context(|| format!("beat {number} cannot be compiled"))?;
        let mut base = scene.config.clone();
        base.render.width = width;
        base.render.height = height;
        match &mut renderer {
            Some(renderer) => pollster::block_on(renderer.replace_fractal(base.clone()))
                .with_context(|| format!("could not compile beat {number}"))?,
            None => renderer = Some(gpu.renderer(base.clone())?),
        }
        let renderer = renderer.as_ref().expect("renderer was just initialised");
        let last = scene
            .animation
            .as_ref()
            .map_or(0, |animation| animation.frame_count - 1);
        for (side, index) in [0, last].into_iter().enumerate() {
            let (mut config, time_seconds) = frame_config(&base, scene.animation.as_ref(), index)?;
            apply_settings(&mut config, &settings);
            let image = renderer
                .render_frame_with_config(&config, index, time_seconds)
                .with_context(|| format!("could not render beat {number}"))?;
            gpu_seconds += image.gpu_time().as_secs_f64();
            cells.push((format!("{number}-{}", side + 1), image.pixels().to_vec()));
        }
        beats.push(BeatFrame {
            beat: number,
            text: beat.text.trim().to_owned(),
            motion: beat.motion.motion(),
            location: beat.location.clone(),
            viewpoint: beat.viewpoint.clone(),
        });
    }

    // One beat per row: its first frame on the left, its last on the right.
    let cells: Vec<Cell<'_>> = cells
        .iter()
        .map(|(label, rgba)| Cell { label, rgba })
        .collect();
    sheet::compose(&cells, 2, width as usize, height as usize)?
        .save_png(&production.path(SHEET_FILE))?;

    let report = StoryboardReport {
        sheet: SHEET_FILE,
        beats,
        gpu_seconds,
    };
    let mut json = serde_json::to_string_pretty(&report).context("could not serialize report")?;
    json.push('\n');
    fs::write(production.path(REPORT_FILE), json)
        .context("could not write the storyboard report")?;
    state.record_gate(
        "storyboard",
        true,
        format!("{} beats drawn; the critic judges them", report.beats.len()),
    )?;
    budget.spent.design_gpu_seconds += gpu_seconds;
    production.save_budget(budget)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{brief::tests::brief_stage, budget::Budget};

    #[test]
    fn the_storyboard_waits_for_a_checked_brief() {
        let (production, mut state, root) = brief_stage("storyboard");
        let mut budget = Budget::default();
        let error = run(&production, &mut state, &mut budget, &GpuOptions::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("brief check"), "{error}");
        assert!(!production.path(SHEET_FILE).exists());

        state.stage = Stage::Shotlist;
        let error = run(&production, &mut state, &mut budget, &GpuOptions::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("stage shotlist"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }
}
