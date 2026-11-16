//! Final assembly. Rendered frames are cut together with their transitions
//! into the delivery file, the file is checked, and the finished film is
//! entered into the ledger so that later films must differ from it.

use std::{
    fs,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::{
    brief,
    budget::Budget,
    film::{Compositor, Encoding, FilmWriter, Rate},
    ledger::{Ledger, LedgerEntry},
    metrics::{Frame, LEGIBILITY_WIDTH},
    novelty,
    produce::frame_path,
    production::Production,
    proof::FinalManifest,
    sheet,
    shotlist::Manifest,
    stage::{Stage, State},
};

pub const QC_FILE: &str = "final/qc.json";
/// Mean luminance below which a rendered frame counts as black.
const BLACK_LUMINANCE: f32 = 0.004;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Qc {
    pub video: String,
    pub frames: usize,
    pub duration_seconds: f64,
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EncodeReport {
    pub qc: Qc,
    /// The ledger entry written when the film passed.
    pub ledger_entry: Option<LedgerEntry>,
}

/// Encodes the film, checks it, and on success records it in the ledger.
pub fn run(
    production: &Production,
    state: &mut State,
    budget: &Budget,
    ledger: &mut Ledger,
    ledger_path: &Path,
) -> Result<EncodeReport> {
    if state.stage != Stage::Encode {
        bail!(
            "the film is encoded in stage encode; the production is in stage {}",
            state.stage.as_str()
        );
    }
    let manifest = Manifest::load_current(production)?;
    let finals = FinalManifest::load_current(production)?;
    let format = manifest.format;
    let relative = format!("final/{}.mp4", production.id());
    let path = production.path(&relative);

    let mut writer = FilmWriter::create(
        &path,
        &Encoding {
            width: format.width,
            height: format.height,
            fps: format.fps,
            // A high-quality master; the platform transcodes from it.
            rate: Rate::Crf(production.load_tuning()?.encode.master_crf),
            preset: "slow",
        },
    )?;
    let mut compositor = Compositor::new(manifest.transition_frames as usize, |rgba: &[u8]| {
        writer.write_frame(rgba)
    });

    let mut problems = Vec::new();
    let mut keyframes: Vec<(u32, Frame)> = Vec::new();
    for (cut, final_cut) in manifest.cuts.iter().zip(&finals.cuts) {
        if cut.id != final_cut.id || cut.frame_count != final_cut.frame_count {
            bail!(
                "the shot list and the proofed cuts disagree about cut {}",
                cut.id
            );
        }
        compositor.begin_cut(cut.frame_count as usize, cut.transition_out)?;
        let (mut black, mut changed) = (0u32, false);
        let mut previous: Option<Vec<u8>> = None;
        for index in 0..cut.frame_count {
            let file = frame_path(production, cut.id, index);
            let image = image::open(&file)
                .with_context(|| format!("could not decode {}", file.display()))?
                .into_rgba8();
            if (image.width(), image.height()) != (format.width, format.height) {
                bail!(
                    "{} is {}x{}, not the film's {}x{}",
                    file.display(),
                    image.width(),
                    image.height(),
                    format.width,
                    format.height
                );
            }
            let rgba = image.into_raw();
            let frame =
                Frame::from_rgba8(format.width, format.height, &rgba)?.fit_width(LEGIBILITY_WIDTH);
            let luma = frame.luma();
            if luma.iter().sum::<f32>() / (luma.len() as f32) < BLACK_LUMINANCE {
                black += 1;
            }
            changed |= previous.as_ref().is_some_and(|previous| *previous != rgba);
            if index == (cut.frame_count - 1) / 2 {
                keyframes.push((cut.id, frame));
            }
            compositor.push_frame(rgba.clone())?;
            previous = Some(rgba);
        }
        if black > 0 {
            problems.push(format!("cut {} has {black} black frames", cut.id));
        }
        // A deliberate hold repeats frames; a cut that never changes is a
        // render that went wrong.
        if cut.frame_count > 1 && !changed {
            problems.push(format!("cut {} never changes", cut.id));
        }
    }
    let frames = compositor.finish()?;
    writer.finish()?;

    let expected = manifest.film_frames() as usize;
    if frames != expected {
        problems.push(format!("the film has {frames} frames, not {expected}"));
    }
    problems.extend(probe_problems(
        &path,
        format.width,
        format.height,
        format.fps,
        expected,
    )?);

    let qc = Qc {
        video: relative,
        frames,
        duration_seconds: frames as f64 / f64::from(format.fps),
        problems,
    };
    let mut json = serde_json::to_string_pretty(&qc).context("could not serialize QC")?;
    json.push('\n');
    fs::write(production.path(QC_FILE), json).context("could not write the QC report")?;
    state.record_gate(
        "qc",
        qc.problems.is_empty(),
        if qc.problems.is_empty() {
            format!(
                "{frames} frames, {:.2} s, checks passed",
                qc.duration_seconds
            )
        } else {
            qc.problems.join("; ")
        },
    )?;
    if !qc.problems.is_empty() {
        return Ok(EncodeReport {
            qc,
            ledger_entry: None,
        });
    }

    let entry = ledger_entry(production, state, budget, ledger_path, &keyframes, &qc)?;
    ledger.append(ledger_path, entry.clone())?;
    state.advance_if_ready()?;
    Ok(EncodeReport {
        qc,
        ledger_entry: Some(entry),
    })
}

fn ledger_entry(
    production: &Production,
    state: &State,
    budget: &Budget,
    ledger_path: &Path,
    keyframes: &[(u32, Frame)],
    qc: &Qc,
) -> Result<LedgerEntry> {
    let directory = ledger_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("keyframes")
        .join(production.id());
    fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let mut paths = Vec::with_capacity(keyframes.len());
    for (cut, frame) in keyframes {
        let path = directory.join(format!("cut-{cut:02}.png"));
        let rgba: Vec<u8> = frame
            .pixels()
            .iter()
            .flat_map(|pixel| {
                let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                [byte(pixel[0]), byte(pixel[1]), byte(pixel[2]), 255]
            })
            .collect();
        sheet::save_png(&path, frame.width() as u32, frame.height() as u32, &rgba)?;
        paths.push(path.to_string_lossy().into_owned());
    }
    Ok(LedgerEntry {
        id: production.id().to_owned(),
        date: today()?,
        title: brief::load(production)?.title.trim().to_owned(),
        axes: brief::load_axes(production)?,
        descriptors: keyframes
            .iter()
            .map(|(_, frame)| novelty::describe(frame))
            .collect(),
        keyframes: paths,
        duration_seconds: qc.duration_seconds,
        gpu_minutes: Some(budget.spent.production_gpu_seconds / 60.0),
        compromised: state.compromised,
    })
}

/// Checks the encoded file against what YouTube's upload guidance asks for
/// and what the shot list promised.
fn probe_problems(
    path: &Path,
    width: u32,
    height: u32,
    fps: u32,
    frames: usize,
) -> Result<Vec<String>> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args([
            "-show_entries",
            "stream=codec_name,pix_fmt,width,height,r_frame_rate,color_space,color_range,nb_read_frames",
        ])
        .args(["-of", "json"])
        .arg(path)
        .output()
        .context("could not start ffprobe; is it installed and on PATH?")?;
    if !output.status.success() {
        bail!(
            "ffprobe could not read {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let probe: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("ffprobe returned invalid JSON")?;
    let stream = &probe["streams"][0];
    let text = |key: &str| stream[key].as_str().unwrap_or("missing").to_owned();
    let number = |key: &str| {
        stream[key]
            .as_u64()
            .map_or("missing".to_owned(), |value| value.to_string())
    };

    let expectations = [
        ("codec", text("codec_name"), "h264".to_owned()),
        ("pixel format", text("pix_fmt"), "yuv420p".to_owned()),
        ("width", number("width"), width.to_string()),
        ("height", number("height"), height.to_string()),
        ("frame rate", text("r_frame_rate"), format!("{fps}/1")),
        ("colour space", text("color_space"), "bt709".to_owned()),
        ("colour range", text("color_range"), "tv".to_owned()),
        ("frame count", text("nb_read_frames"), frames.to_string()),
    ];
    Ok(expectations
        .into_iter()
        .filter(|(_, actual, expected)| actual != expected)
        .map(|(name, actual, expected)| format!("the file's {name} is {actual}, not {expected}"))
        .collect())
}

/// Today's UTC date as `YYYY-MM-DD`.
fn today() -> Result<String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs();
    Ok(civil_date((seconds / 86_400) as i64))
}

/// Converts days since 1970-01-01 to a calendar date in the proleptic
/// Gregorian calendar.
fn civil_date(days: i64) -> String {
    // Shift the epoch to 0000-03-01 so leap days fall at the end of a year.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        axes::{
            Axes, FormulaFamily, Light, Motion, Pacing, Palette, PaletteHue, PaletteKey,
            ScaleRegime,
        },
        cut::Format,
        look::{EffectsRequested, GRADED_MANIFEST_FILE, GradedManifest, LOOK_FILE},
        proof::{FINAL_MANIFEST_FILE, FinalCut},
        shotlist::{CompiledCut, MANIFEST_FILE, Role, SHOTLIST_FILE, Transition, content_hash},
    };
    use std::path::PathBuf;

    const FORMAT: Format = Format {
        width: 1280,
        height: 720,
        fps: 30,
    };

    /// A production in the encode stage with two rendered cuts of ten
    /// frames each, whose brightness comes from `value(cut, frame)`.
    fn rendered(name: &str, value: impl Fn(u32, u32) -> u8) -> (Production, State, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "fractal-harness-encode-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let production =
            Production::create(&root, "film", &Ledger::default(), 1, Budget::default()).unwrap();
        let write = |relative: &str, text: String| {
            let path = production.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };

        write(SHOTLIST_FILE, "shotlist".to_owned());
        write(LOOK_FILE, "look".to_owned());
        write(
            brief::BRIEF_FILE,
            "version: 1\ntitle: Test Film\nsubject: S.\naxes:\n  scale_regime: mid\n  motion: orbit\n  palette: { primary_hue: teal, key: mid }\n  light: top\n  pacing: fast\nlocations: [loc-0001]\narc: A.\nbeats:\n  - { text: The whole shape., location: loc-0001, viewpoint: vp-1, motion: { kind: orbit, revolutions: 0.2 } }\n  - { text: Closer., location: loc-0001, viewpoint: vp-2, motion: { kind: dive, depth: 0.6 } }\n".to_owned(),
        );
        brief::save_axes(
            &production,
            &Axes {
                formula_family: FormulaFamily::JuliaBox,
                scale_regime: ScaleRegime::Mid,
                motion: Motion::Orbit,
                palette: Palette {
                    primary_hue: PaletteHue::Teal,
                    key: PaletteKey::Mid,
                },
                light: Light::Top,
                pacing: Pacing::Fast,
            },
        )
        .unwrap();

        let cuts: Vec<CompiledCut> = (1..=2)
            .map(|id| CompiledCut {
                id,
                role: Role::Detail,
                location: "loc-0001".to_owned(),
                motion: Motion::Orbit,
                scale_regime: ScaleRegime::Mid,
                duration_seconds: 0.3,
                frame_count: 10,
                transition_out: Transition::Cut,
                scene: String::new(),
            })
            .collect();
        let (shotlist_hash, look_hash) = (content_hash("shotlist"), content_hash("look"));
        write(
            MANIFEST_FILE,
            serde_json::to_string(&Manifest {
                shotlist_hash: shotlist_hash.clone(),
                format: FORMAT,
                transition_frames: 15,
                cuts,
            })
            .unwrap(),
        );
        write(
            GRADED_MANIFEST_FILE,
            serde_json::to_string(&GradedManifest {
                look_hash: look_hash.clone(),
                shotlist_hash: shotlist_hash.clone(),
                effects_requested: EffectsRequested::default(),
                cuts: Vec::new(),
            })
            .unwrap(),
        );
        write(
            FINAL_MANIFEST_FILE,
            serde_json::to_string(&FinalManifest {
                look_hash,
                shotlist_hash,
                cuts: (1..=2)
                    .map(|id| FinalCut {
                        id,
                        scene: String::new(),
                        frame_count: 10,
                        estimated_gpu_seconds: 5.0,
                    })
                    .collect(),
                estimated_gpu_seconds: 10.0,
            })
            .unwrap(),
        );

        for cut in 1..=2 {
            fs::create_dir_all(production.path(&format!("frames/cut-{cut:02}"))).unwrap();
            for frame in 0..10 {
                let level = value(cut, frame);
                // A horizontal ramp, so frames are not flat fields.
                let rgba: Vec<u8> = (0..720)
                    .flat_map(|_| 0..1280u32)
                    .flat_map(|x| {
                        let shade = (u32::from(level) * (x + 640) / 1920) as u8;
                        [shade, level / 2, level, 255]
                    })
                    .collect();
                sheet::save_png(&frame_path(&production, cut, frame), 1280, 720, &rgba).unwrap();
            }
        }
        let mut state = production.load_state().unwrap();
        state.stage = Stage::Encode;
        (production, state, root)
    }

    #[test]
    fn a_rendered_film_is_encoded_checked_and_entered_in_the_ledger() {
        let (production, mut state, root) =
            rendered("pass", |cut, frame| (60 + cut * 40 + frame * 4) as u8);
        let ledger_path = root.join("portfolio/ledger.jsonl");
        let mut ledger = Ledger::default();
        let mut budget = Budget::default();
        budget.spent.production_gpu_seconds = 600.0;

        let report = run(&production, &mut state, &budget, &mut ledger, &ledger_path).unwrap();
        assert!(report.qc.problems.is_empty(), "{:?}", report.qc.problems);
        assert_eq!(report.qc.frames, 20);
        assert_eq!(state.stage, Stage::Done);
        assert!(production.path("final/film.mp4").is_file());
        assert!(production.path(QC_FILE).is_file());

        let entry = report.ledger_entry.unwrap();
        assert_eq!(entry.title, "Test Film");
        assert_eq!(entry.axes.formula_family, FormulaFamily::JuliaBox);
        assert_eq!(entry.gpu_minutes, Some(10.0));
        assert_eq!((entry.keyframes.len(), entry.descriptors.len()), (2, 2));
        assert!(entry.keyframes.iter().all(|path| Path::new(path).is_file()));
        assert!(!entry.compromised);
        // The film is now part of what later films are compared against.
        let reloaded = Ledger::load(&ledger_path).unwrap();
        assert_eq!(reloaded.entries(), [entry]);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn black_and_static_cuts_fail_qc_and_stay_out_of_the_ledger() {
        // Cut 1 is black throughout; cut 2 never changes.
        let (production, mut state, root) =
            rendered("fail", |cut, _| if cut == 1 { 0 } else { 120 });
        let ledger_path = root.join("portfolio/ledger.jsonl");
        let mut ledger = Ledger::default();

        let report = run(
            &production,
            &mut state,
            &Budget::default(),
            &mut ledger,
            &ledger_path,
        )
        .unwrap();
        let problems = report.qc.problems.join("\n");
        assert!(problems.contains("cut 1 has 10 black frames"), "{problems}");
        assert!(problems.contains("cut 2 never changes"), "{problems}");
        assert!(report.ledger_entry.is_none());
        assert_eq!(state.stage, Stage::Encode);
        assert!(!state.gates["qc"].passed);
        assert!(ledger.entries().is_empty());
        assert!(!ledger_path.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_frame_stops_the_encode() {
        let (production, mut state, root) = rendered("missing", |_, frame| (80 + frame) as u8);
        fs::remove_file(frame_path(&production, 2, 7)).unwrap();
        let error = run(
            &production,
            &mut state,
            &Budget::default(),
            &mut Ledger::default(),
            &root.join("ledger.jsonl"),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("frame_000007"), "{error:#}");
        // No half-written film may be left behind.
        assert!(!production.path("final/film.mp4").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn days_convert_to_calendar_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(59), "1970-03-01");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(civil_date(11_017), "2000-03-01");
        assert_eq!(civil_date(20_773), "2026-11-16");
        assert_eq!(civil_date(-1), "1969-12-31");
    }
}
