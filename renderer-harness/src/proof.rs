//! The proof: the film's final quality is decided, its render time is
//! estimated against the budget, and an excerpt of every cut is rendered at
//! that quality and judged the way a viewer will receive it.

use std::{collections::BTreeMap, fs, ops::Range};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::{LoadedScene, Renderer, load_scene};
use serde::{Deserialize, Serialize};

use crate::{
    budget::Budget,
    film::{Encoding, FilmWriter, Rate, decode_frames},
    gpu::{GpuOptions, SceneProbe, apply_settings, frame_config},
    look::{GradedManifest, LOOK_FILE},
    metrics::{Frame, difference, flicker, flicker_luma},
    production::Production,
    quality::{self, Decision, Estimate, FrameLimits, Settings},
    sheet::{self, Cell},
    shotlist::{CompiledCut, Manifest, SHOTLIST_FILE},
    stage::{Stage, State},
    tuning::ProofRules,
};

pub const CLIP_FILE: &str = "proof/proof.mp4";
/// The clip as a delivery platform would re-encode it.
pub const DELIVERY_FILE: &str = "proof/proof-delivery.mp4";
/// Stills taken from the delivery encode, for review.
pub const SHEET_FILE: &str = "proof/sheet.png";
pub const REPORT_FILE: &str = "proof/report.json";
pub const FINAL_MANIFEST_FILE: &str = "compiled/final/manifest.json";

/// A cut with its final quality, ready for production rendering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FinalCut {
    pub id: u32,
    /// Scene file relative to the production directory.
    pub scene: String,
    pub frame_count: u32,
    /// Estimated GPU seconds to render the whole cut.
    pub estimated_gpu_seconds: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FinalManifest {
    pub look_hash: String,
    pub shotlist_hash: String,
    pub cuts: Vec<FinalCut>,
    pub estimated_gpu_seconds: f64,
}

impl FinalManifest {
    /// Loads the manifest and refuses it if the look or shot list changed
    /// since the proof that wrote it.
    pub fn load_current(production: &Production) -> Result<Self> {
        let path = production.path(FINAL_MANIFEST_FILE);
        let text = fs::read_to_string(&path)
            .context("the film has not been proofed; run `proof` first")?;
        let manifest: Self =
            serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        let graded = GradedManifest::load_current(production)?;
        if graded.look_hash != manifest.look_hash || graded.shotlist_hash != manifest.shotlist_hash
        {
            bail!("the look or shot list changed after the proof; run `proof` again");
        }
        Ok(manifest)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CutProof {
    pub id: u32,
    pub location: String,
    pub settings: Settings,
    pub estimate: Estimate,
    pub estimated_gpu_seconds: f64,
    /// Mean deviation of each excerpt frame from its neighbours' average.
    pub flicker: f32,
    /// Structural similarity of the excerpt after delivery compression.
    pub delivery_ssim: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProofReport {
    pub clip: &'static str,
    pub delivery_clip: &'static str,
    pub sheet: &'static str,
    /// One quality decision per location, made on its longest cut.
    pub decisions: BTreeMap<String, Decision>,
    pub cuts: Vec<CutProof>,
    pub estimated_gpu_seconds: f64,
    pub allowed_gpu_seconds: f64,
    pub budget_problems: Vec<String>,
    pub compression_problems: Vec<String>,
    pub flicker_problems: Vec<String>,
    pub gpu_seconds: f64,
    pub design_remaining_gpu_seconds: f64,
}

/// Decides quality, estimates time, renders the proof clip, and judges the
/// measured gates of the proof stage.
pub fn run(
    production: &Production,
    state: &mut State,
    budget: &mut Budget,
    gpu: &GpuOptions,
) -> Result<ProofReport> {
    if state.stage != Stage::Proof {
        bail!(
            "the proof is made in stage proof; the production is in stage {}",
            state.stage.as_str()
        );
    }
    let tuning = production.load_tuning()?;
    let graded = GradedManifest::load_current(production)?;
    let manifest = Manifest::load_current(production)?;
    if budget.design_remaining_seconds() <= 0.0 {
        bail!("the design GPU budget of this production is exhausted");
    }
    let duty_cycle = gpu.duty_cycle_percent.unwrap_or(budget.max_gpu_duty_cycle);
    budget.check_duty_cycle(duty_cycle)?;
    let gpu = GpuOptions {
        duty_cycle_percent: Some(duty_cycle),
        ..gpu.clone()
    };
    // The previous proof is kept, traced to the files that made it.
    production.archive("proof")?;
    production.snapshot_inputs("proof", &[SHOTLIST_FILE, LOOK_FILE])?;
    state.reset_gates();
    let _ = fs::remove_file(production.path(FINAL_MANIFEST_FILE));

    let format = manifest.format;
    let limits = FrameLimits {
        mean_frame_seconds: budget.mean_frame_seconds(f64::from(format.fps)),
        max_frame_seconds: budget.max_frame_seconds,
    };
    let scene_of = |id: u32| -> Result<LoadedScene> {
        let entry = graded
            .cuts
            .iter()
            .find(|entry| entry.id == id)
            .with_context(|| format!("cut {id} has no graded scene"))?;
        load_scene(production.path(&entry.scene))
    };
    let mut gpu_seconds = 0.0;

    // Quality is a property of a place and its look, so it is decided once
    // per location, on the cut that shows the most of it.
    let mut decisions = BTreeMap::new();
    let mut estimates: BTreeMap<u32, Estimate> = BTreeMap::new();
    for (location, cut_id) in representatives(&manifest.cuts) {
        let scene = scene_of(cut_id)?;
        let mut probe = SceneProbe::new(&scene, &gpu, tuning.proof.region_fraction)?;
        let frames = probe.probe_frames();
        let requested = probe.requested();
        let decision = quality::decide(&mut probe, &frames, &requested, &limits, &tuning.quality)
            .with_context(|| format!("could not decide the quality of {location}"))?;
        gpu_seconds += decision.gpu_seconds_spent;
        estimates.insert(cut_id, decision.estimate.clone());
        decisions.insert(location, decision);
    }

    let mut cuts = Vec::with_capacity(manifest.cuts.len());
    let mut finals = Vec::with_capacity(manifest.cuts.len());
    fs::create_dir_all(production.path("compiled/final"))
        .context("could not create the final directory")?;
    for cut in &manifest.cuts {
        let settings = decisions[&cut.location].settings;
        let mut scene = scene_of(cut.id)?;
        let estimate = match estimates.remove(&cut.id) {
            Some(estimate) => estimate,
            None => {
                let mut probe = SceneProbe::new(&scene, &gpu, tuning.proof.region_fraction)?;
                let frames = probe.probe_frames();
                let (estimate, spent) = quality::measure(&mut probe, &frames, &settings)?;
                gpu_seconds += spent;
                estimate
            }
        };
        let estimated_gpu_seconds = estimate.mean_frame_seconds * f64::from(cut.frame_count);

        apply_settings(&mut scene.config, &settings);
        scene
            .config
            .validate()
            .with_context(|| format!("the decided quality is not renderable on cut {}", cut.id))?;
        let relative = format!("compiled/final/cut-{:02}.yaml", cut.id);
        fs::write(production.path(&relative), scene.to_yaml()?)
            .with_context(|| format!("could not write {relative}"))?;
        finals.push((
            FinalCut {
                id: cut.id,
                scene: relative,
                frame_count: cut.frame_count,
                estimated_gpu_seconds,
            },
            scene,
        ));
        cuts.push(CutProof {
            id: cut.id,
            location: cut.location.clone(),
            settings,
            estimate,
            estimated_gpu_seconds,
            flicker: 0.0,
            delivery_ssim: 0.0,
        });
    }

    let estimated_total: f64 = cuts.iter().map(|cut| cut.estimated_gpu_seconds).sum();
    let allowed = budget.production_limit_seconds();
    let budget_problems = budget_problems(&cuts, estimated_total, allowed, &limits);

    // The excerpt of every cut, rendered at the decided quality.
    let width = tuning.proof.clip_width.min(format.width);
    let height =
        (u64::from(format.height) * u64::from(width) / u64::from(format.width)) as u32 & !1;
    fs::create_dir_all(production.path("proof")).context("could not create the proof directory")?;
    let encoding = Encoding {
        width,
        height,
        fps: format.fps,
        rate: Rate::Crf(16),
        preset: "medium",
    };
    let mut clip = FilmWriter::create(&production.path(CLIP_FILE), &encoding)?;
    let mut delivery = FilmWriter::create(
        &production.path(DELIVERY_FILE),
        &Encoding {
            rate: Rate::Kbps(delivery_kbps(width, height, &tuning.proof)),
            ..encoding.clone()
        },
    )?;

    let mut renderer: Option<Renderer> = None;
    // Frames kept for comparison with the delivery encode: clip frame index,
    // cut position, and the frame at half size.
    let mut kept: Vec<(usize, usize, Frame)> = Vec::new();
    // Per cut, the clip frames of each of its excerpts.
    let mut excerpts: Vec<Vec<Range<usize>>> = Vec::with_capacity(finals.len());
    let mut clip_frames = 0usize;
    for (position, (final_cut, scene)) in finals.iter().enumerate() {
        let mut base = scene.config.clone();
        base.render.width = width;
        base.render.height = height;
        match &mut renderer {
            Some(renderer) => pollster::block_on(renderer.replace_fractal(base.clone()))
                .with_context(|| format!("could not compile cut {}", final_cut.id))?,
            None => renderer = Some(gpu.renderer(base.clone())?),
        }
        let renderer = renderer.as_ref().expect("renderer was just initialised");

        let windows = if f64::from(final_cut.frame_count)
            > tuning.shotlist.max_cut_seconds * f64::from(format.fps) + 1.0
        {
            spread_excerpts(
                final_cut.frame_count,
                format.fps,
                tuning.proof.clip_seconds_per_cut,
                tuning.proof.long_cut_excerpts,
            )
        } else {
            vec![excerpt(
                final_cut.frame_count,
                format.fps,
                tuning.proof.clip_seconds_per_cut,
            )]
        };
        let mut shown = Vec::with_capacity(windows.len());
        let (mut flicker_sum, mut flicker_count) = (0.0f64, 0u32);
        for window in windows {
            let start = clip_frames;
            // Flicker compares neighbours, never frames across a jump.
            let mut history: Vec<Vec<f32>> = Vec::with_capacity(2);
            for index in window {
                let (config, time_seconds) = frame_config(&base, scene.animation.as_ref(), index)?;
                let image = renderer
                    .render_frame_with_config(&config, index, time_seconds)
                    .with_context(|| {
                        format!("could not render cut {} frame {index}", final_cut.id)
                    })?;
                gpu_seconds += image.gpu_time().as_secs_f64();
                clip.write_frame(image.pixels())?;
                delivery.write_frame(image.pixels())?;

                let half =
                    Frame::from_rgba8(image.width(), image.height(), image.pixels())?.reduced(2);
                let luma = flicker_luma(&half);
                if let [before, middle] = history.as_slice() {
                    flicker_sum += f64::from(flicker(before, middle, &luma));
                    flicker_count += 1;
                }
                if history.len() == 2 {
                    history.remove(0);
                }
                history.push(luma);
                if clip_frames.is_multiple_of(tuning.proof.compare_every as usize) {
                    kept.push((clip_frames, position, half));
                }
                clip_frames += 1;
            }
            shown.push(start..clip_frames);
        }
        cuts[position].flicker = if flicker_count == 0 {
            0.0
        } else {
            (flicker_sum / f64::from(flicker_count)) as f32
        };
        excerpts.push(shown);
    }
    clip.finish()?;
    delivery.finish()?;

    // Judge the clip as it arrives after delivery compression.
    let mut ssim_sums = vec![(0.0f64, 0u32); cuts.len()];
    let mut stills: Vec<(String, Vec<u8>)> = Vec::new();
    let mut next_kept = 0usize;
    let decoded = decode_frames(
        &production.path(DELIVERY_FILE),
        width,
        height,
        |index, rgba| {
            if let Some((kept_index, position, reference)) = kept.get(next_kept)
                && *kept_index == index
            {
                let received = Frame::from_rgba8(width, height, rgba)?.reduced(2);
                let (sum, count) = &mut ssim_sums[*position];
                *sum += f64::from(difference(reference, &received)?.ssim);
                *count += 1;
                next_kept += 1;
            }
            for (position, windows) in excerpts.iter().enumerate() {
                if still_frames(windows).contains(&index) {
                    let shown = stills
                        .iter()
                        .filter(|(label, _)| label.starts_with(&format!("{}-", cuts[position].id)))
                        .count();
                    // Halved, so the sheet stays small enough to look at.
                    stills.push((
                        format!("{}-{}", cuts[position].id, shown + 1),
                        sheet::halve(rgba, width, height),
                    ));
                }
            }
            Ok(())
        },
    )?;
    if decoded != clip_frames {
        bail!(
            "the delivery encode holds {decoded} frames, not the {clip_frames} that were rendered"
        );
    }
    for (cut, (sum, count)) in cuts.iter_mut().zip(&ssim_sums) {
        cut.delivery_ssim = if *count == 0 {
            1.0
        } else {
            (sum / f64::from(*count)) as f32
        };
    }
    let cells: Vec<Cell<'_>> = stills
        .iter()
        .map(|(label, rgba)| Cell { label, rgba })
        .collect();
    let columns = excerpts
        .iter()
        .map(|windows| still_frames(windows).len())
        .max()
        .unwrap_or(3);
    sheet::compose(&cells, columns, width as usize / 2, height as usize / 2)?
        .save_png(&production.path(SHEET_FILE))?;

    let compression_problems: Vec<String> = cuts
        .iter()
        .filter(|cut| cut.delivery_ssim < tuning.proof.min_delivery_ssim)
        .map(|cut| {
            format!(
                "cut {} keeps a similarity of {:.3} after delivery compression; at least {} is needed",
                cut.id, cut.delivery_ssim, tuning.proof.min_delivery_ssim
            )
        })
        .collect();
    let flicker_problems: Vec<String> = cuts
        .iter()
        .filter(|cut| cut.flicker > tuning.legibility.max_flicker)
        .map(|cut| {
            format!(
                "cut {} shimmers in motion (flicker {:.4}; at most {})",
                cut.id, cut.flicker, tuning.legibility.max_flicker
            )
        })
        .collect();

    let record = |state: &mut State, gate: &str, problems: &[String], passed: String| {
        state.record_gate(
            gate,
            problems.is_empty(),
            if problems.is_empty() {
                passed
            } else {
                problems.join("; ")
            },
        )
    };
    record(
        state,
        "budget",
        &budget_problems,
        format!("estimated {estimated_total:.0} GPU s of {allowed:.0} allowed"),
    )?;
    record(
        state,
        "compression",
        &compression_problems,
        "every cut survives delivery compression".to_owned(),
    )?;
    record(
        state,
        "flicker",
        &flicker_problems,
        "no cut shimmers in motion".to_owned(),
    )?;

    let final_manifest = FinalManifest {
        look_hash: graded.look_hash,
        shotlist_hash: graded.shotlist_hash,
        cuts: finals.into_iter().map(|(cut, _)| cut).collect(),
        estimated_gpu_seconds: estimated_total,
    };
    write_json(production, FINAL_MANIFEST_FILE, &final_manifest)?;
    budget.spent.design_gpu_seconds += gpu_seconds;
    production.save_budget(budget)?;

    let report = ProofReport {
        clip: CLIP_FILE,
        delivery_clip: DELIVERY_FILE,
        sheet: SHEET_FILE,
        decisions,
        cuts,
        estimated_gpu_seconds: estimated_total,
        allowed_gpu_seconds: allowed,
        budget_problems,
        compression_problems,
        flicker_problems,
        gpu_seconds,
        design_remaining_gpu_seconds: budget.design_remaining_seconds(),
    };
    write_json(production, REPORT_FILE, &report)?;
    Ok(report)
}

fn write_json<T: Serialize>(production: &Production, relative: &str, value: &T) -> Result<()> {
    let mut json = serde_json::to_string_pretty(value)
        .with_context(|| format!("could not serialize {relative}"))?;
    json.push('\n');
    fs::write(production.path(relative), json)
        .with_context(|| format!("could not write {relative}"))
}

/// The longest cut of every location, by location.
fn representatives(cuts: &[CompiledCut]) -> BTreeMap<String, u32> {
    let mut longest: BTreeMap<String, (f64, u32)> = BTreeMap::new();
    for cut in cuts {
        let entry = longest
            .entry(cut.location.clone())
            .or_insert((f64::NEG_INFINITY, cut.id));
        // Strictly longer, so the earliest of equally long cuts is kept.
        if cut.duration_seconds > entry.0 {
            *entry = (cut.duration_seconds, cut.id);
        }
    }
    longest
        .into_iter()
        .map(|(location, (_, id))| (location, id))
        .collect()
}

/// Frames of a cut shown in the proof clip: a window around its middle.
fn excerpt(frame_count: u32, fps: u32, seconds: f64) -> Range<u32> {
    let wanted = ((seconds * f64::from(fps)).round() as u32).clamp(1, frame_count);
    let start = (frame_count - wanted) / 2;
    start..start + wanted
}

/// Frames of a long cut shown in the proof clip: `count` windows of
/// `seconds` each, spread evenly from its first frame to its last.
fn spread_excerpts(frame_count: u32, fps: u32, seconds: f64, count: u32) -> Vec<Range<u32>> {
    let wanted = ((seconds * f64::from(fps)).round() as u32).clamp(1, frame_count);
    let span = frame_count - wanted;
    let count = count.max(2);
    (0..count)
        .map(|step| {
            let start = (f64::from(span) * f64::from(step) / f64::from(count - 1)).round() as u32;
            start..start + wanted
        })
        .collect()
}

/// Clip frames shown on the sheet: the first, middle, and last frame of a
/// single excerpt, or the middle frame of each of several.
fn still_frames(windows: &[Range<usize>]) -> Vec<usize> {
    match windows {
        [window] => vec![
            window.start,
            window.start + window.len() / 2,
            window.end - 1,
        ],
        windows => windows
            .iter()
            .map(|window| window.start + window.len() / 2)
            .collect(),
    }
}

/// Bitrate a delivery platform is assumed to give a stream of this size.
fn delivery_kbps(width: u32, height: u32, rules: &ProofRules) -> u32 {
    let share = f64::from(width) * f64::from(height) / (1920.0 * 1080.0);
    ((f64::from(rules.delivery_kbps_1080p) * share).round() as u32).max(1)
}

fn budget_problems(
    cuts: &[CutProof],
    estimated_total: f64,
    allowed: f64,
    limits: &FrameLimits,
) -> Vec<String> {
    let mut problems = Vec::new();
    if estimated_total > allowed {
        let slowest = cuts
            .iter()
            .max_by(|a, b| a.estimated_gpu_seconds.total_cmp(&b.estimated_gpu_seconds))
            .map_or(0, |cut| cut.id);
        problems.push(format!(
            "the film is estimated at {estimated_total:.0} GPU s, {:.0}% of the {allowed:.0} s allowed; cut {slowest} costs the most",
            estimated_total / allowed * 100.0
        ));
    }
    for cut in cuts {
        if cut.estimate.max_frame_seconds > limits.max_frame_seconds {
            problems.push(format!(
                "cut {} has frames estimated at {:.1} s; at most {:.1} s is allowed",
                cut.id, cut.estimate.max_frame_seconds, limits.max_frame_seconds
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        axes::{Motion, ScaleRegime},
        quality::FrameTime,
        shotlist::{Role, Transition},
    };

    fn cut(id: u32, location: &str, duration_seconds: f64) -> CompiledCut {
        CompiledCut {
            id,
            role: Role::Detail,
            location: location.to_owned(),
            motion: Motion::Orbit,
            scale_regime: ScaleRegime::Mid,
            duration_seconds,
            frame_count: (duration_seconds * 30.0) as u32 + 1,
            transition_out: Transition::Cut,
            scene: String::new(),
        }
    }

    fn proofed(id: u32, mean: f64, max: f64, frames: u32) -> CutProof {
        CutProof {
            id,
            location: "loc-0001".to_owned(),
            settings: Settings {
                samples_per_pixel: 8,
                ambient_occlusion_steps: 0,
                soft_shadow_steps: 0,
                reflection_steps: 0,
                depth_of_field: false,
                pixel_epsilon_multiplier: 1.0,
            },
            estimate: Estimate {
                mean_frame_seconds: mean,
                max_frame_seconds: max,
                frames: vec![FrameTime {
                    frame: 0,
                    seconds: mean,
                }],
            },
            estimated_gpu_seconds: mean * f64::from(frames),
            flicker: 0.0,
            delivery_ssim: 1.0,
        }
    }

    #[test]
    fn each_location_is_represented_by_its_longest_cut() {
        let cuts = [
            cut(1, "loc-0002", 8.0),
            cut(2, "loc-0001", 6.0),
            cut(3, "loc-0002", 12.0),
            cut(4, "loc-0001", 6.0),
        ];
        let chosen = representatives(&cuts);
        assert_eq!(chosen["loc-0002"], 3);
        // Equally long cuts resolve to the earlier one.
        assert_eq!(chosen["loc-0001"], 2);
        assert_eq!(chosen.len(), 2);
    }

    #[test]
    fn excerpts_are_centred_and_never_exceed_the_cut() {
        assert_eq!(excerpt(301, 30, 1.0), 135..165);
        assert_eq!(excerpt(20, 30, 1.0), 0..20);
        assert_eq!(excerpt(1, 30, 1.0), 0..1);
        assert_eq!(excerpt(100, 60, 0.5), 35..65);
        assert_eq!(
            spread_excerpts(2701, 30, 1.0, 5),
            vec![0..30, 668..698, 1336..1366, 2003..2033, 2671..2701]
        );
        let single = vec![Range { start: 10, end: 40 }];
        assert_eq!(still_frames(&single), vec![10, 25, 39]);
        assert_eq!(still_frames(&[0..30, 30..60]), vec![15, 45]);
    }

    #[test]
    fn delivery_bitrate_scales_with_the_raster() {
        let rules = ProofRules::default();
        assert_eq!(delivery_kbps(1920, 1080, &rules), 8000);
        assert_eq!(delivery_kbps(960, 540, &rules), 2000);
        assert_eq!(delivery_kbps(2, 2, &rules), 1);
    }

    #[test]
    fn budget_problems_name_the_overrun_and_slow_frames() {
        let limits = FrameLimits {
            mean_frame_seconds: 4.0,
            max_frame_seconds: 12.0,
        };
        let cuts = [proofed(1, 2.0, 3.0, 100), proofed(2, 5.0, 14.0, 200)];
        let total = 1200.0;
        assert!(
            budget_problems(&cuts, total, 1500.0, &limits)
                .iter()
                .all(|problem| problem.contains("14.0 s"))
        );
        assert_eq!(
            budget_problems(&cuts[..1], 200.0, 1500.0, &limits),
            Vec::<String>::new()
        );

        let problems = budget_problems(&cuts, total, 1000.0, &limits).join("\n");
        assert!(
            problems.contains("120% of the 1000 s allowed"),
            "{problems}"
        );
        assert!(problems.contains("cut 2 costs the most"), "{problems}");
        assert!(
            problems.contains("cut 2 has frames estimated at 14.0 s"),
            "{problems}"
        );
    }
}
