//! The shot list: which cuts make up the film. Checking it compiles every
//! cut, walks its camera route, and verifies that the film realises the axes
//! its brief committed to.

use std::{collections::BTreeMap, fs};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::load_scene;
use serde::{Deserialize, Serialize};

use crate::{
    axes::{Axes, AxisId, Motion, ScaleRegime},
    brief,
    budget::Budget,
    classify,
    cut::{self, Format, MotionSpec, Route},
    production::Production,
    scout::{self, Index},
    stage::{Stage, State},
    tuning::{Classification, ShotlistRules},
};

pub const SHOTLIST_VERSION: u32 = 1;
pub const SHOTLIST_FILE: &str = "shotlist.yaml";
/// Harness-owned record of what the last successful check compiled.
pub const MANIFEST_FILE: &str = "compiled/manifest.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Establishing,
    Dive,
    Detail,
    Reveal,
    Transit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transition {
    Cut,
    FadeBlack,
    Crossfade,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shotlist {
    pub version: u32,
    pub format: Format,
    pub cuts: Vec<CutSpec>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CutSpec {
    /// Position in the film, starting at 1.
    pub id: u32,
    pub role: Role,
    pub location: String,
    pub viewpoint: String,
    pub motion: MotionSpec,
    pub duration_seconds: f64,
    /// How this cut hands over to the next.
    pub transition_out: Transition,
}

/// What the check established about one cut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompiledCut {
    pub id: u32,
    pub role: Role,
    pub location: String,
    pub motion: Motion,
    pub scale_regime: ScaleRegime,
    pub duration_seconds: f64,
    pub frame_count: u32,
    pub transition_out: Transition,
    /// Scene file relative to the production directory.
    pub scene: String,
}

/// Everything later stages need to know about the checked shot list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Hash of the shot list text the cuts were compiled from.
    pub shotlist_hash: String,
    pub format: Format,
    /// Frames a fade or crossfade lasts.
    pub transition_frames: u32,
    pub cuts: Vec<CompiledCut>,
}

impl Manifest {
    /// Loads the manifest and refuses it if the shot list changed since.
    pub fn load_current(production: &Production) -> Result<Self> {
        let path = production.path(MANIFEST_FILE);
        let text = fs::read_to_string(&path)
            .context("the shot list has not been checked; run `shotlist check` first")?;
        let manifest: Self =
            serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        let shotlist = fs::read_to_string(production.path(SHOTLIST_FILE))
            .with_context(|| format!("could not read {SHOTLIST_FILE}"))?;
        if content_hash(&shotlist) != manifest.shotlist_hash {
            bail!("{SHOTLIST_FILE} changed after it was checked; run `shotlist check` again");
        }
        Ok(manifest)
    }

    /// Frames of the finished film, after crossfades overlap their cuts.
    #[must_use]
    pub fn film_frames(&self) -> u32 {
        let overlap = self.transition_frames;
        let crossfades = self
            .cuts
            .iter()
            .filter(|cut| cut.transition_out == Transition::Crossfade)
            .count() as u32;
        self.cuts.iter().map(|cut| cut.frame_count).sum::<u32>() - crossfades * overlap
    }
}

/// Frames a fade or crossfade lasts at the given frame rate.
#[must_use]
pub fn transition_frames(fps: u32, transition_seconds: f64) -> u32 {
    (transition_seconds * f64::from(fps)).round() as u32
}

#[derive(Clone, Debug, Serialize)]
pub struct CutReport {
    pub id: u32,
    pub motion: Motion,
    pub scale_regime: ScaleRegime,
    pub route: Route,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ShotlistReport {
    pub schema_errors: Vec<String>,
    pub cuts: Vec<CutReport>,
    pub unsafe_routes: Vec<String>,
    pub variety_errors: Vec<String>,
    pub total_seconds: Option<f64>,
}

/// Reads `shotlist.yaml` and judges the gates that need no rendering.
pub fn check(
    production: &Production,
    state: &mut State,
    budget: &mut Budget,
) -> Result<ShotlistReport> {
    if state.stage != Stage::Shotlist {
        bail!(
            "the shot list is checked in stage shotlist; the production is in stage {}",
            state.stage.as_str()
        );
    }
    let tuning = production.load_tuning()?;
    // A changed shot list voids the animatic and the critic's verdict too.
    state.reset_gates();
    let _ = fs::remove_file(production.path(MANIFEST_FILE));

    let mut report = ShotlistReport::default();
    let fail_schema = |state: &mut State, report: ShotlistReport| -> Result<ShotlistReport> {
        state.record_gate("schema", false, report.schema_errors.join("; "))?;
        Ok(report)
    };

    let text = match fs::read_to_string(production.path(SHOTLIST_FILE)) {
        Ok(text) => text,
        Err(error) => {
            report
                .schema_errors
                .push(format!("could not read {SHOTLIST_FILE}: {error}"));
            return fail_schema(state, report);
        }
    };
    let shotlist: Shotlist = match serde_yaml_ng::from_str(&text) {
        Ok(shotlist) => shotlist,
        Err(error) => {
            report.schema_errors.push(format!(
                "{SHOTLIST_FILE} does not match the shot list schema: {error}"
            ));
            return fail_schema(state, report);
        }
    };

    let index = Index::load(production)?;
    let brief = brief::load(production)?;
    let axes = brief::load_axes(production)?;
    report.schema_errors = structure_errors(
        &shotlist,
        &brief.locations,
        &index,
        budget,
        &tuning.shotlist,
    );
    if !report.schema_errors.is_empty() {
        return fail_schema(state, report);
    }

    let mut compiled = Vec::with_capacity(shotlist.cuts.len());
    for spec in &shotlist.cuts {
        let record = &index.locations[&spec.location];
        let survey = record.survey();
        let viewpoint = load_scene(scout::viewpoint_scene_path(
            production,
            &spec.location,
            &spec.viewpoint,
        ))?;
        let name = format!("cut-{:02}", spec.id);
        let scene = match cut::compile(
            &viewpoint,
            &survey,
            &spec.motion,
            spec.duration_seconds,
            &shotlist.format,
            &name,
            &tuning,
        ) {
            Ok(scene) => scene,
            Err(error) => {
                report
                    .schema_errors
                    .push(format!("cut {}: {error:#}", spec.id));
                continue;
            }
        };
        let route = cut::check_route(&scene, &survey, &tuning)?;
        if !route.safe {
            report.unsafe_routes.push(format!(
                "cut {}: clearance is {:.4} of the subject distance at frame {}; minimum {}",
                spec.id,
                route.min_clearance_ratio,
                route.worst_frame,
                tuning.shotlist.min_clearance_ratio
            ));
        }
        let relative = format!("compiled/{name}.yaml");
        fs::write(production.path(&relative), scene.to_yaml()?)
            .with_context(|| format!("could not write {relative}"))?;
        report.cuts.push(CutReport {
            id: spec.id,
            motion: spec.motion.motion(),
            scale_regime: route.scale_regime,
            route,
        });
        compiled.push(CompiledCut {
            id: spec.id,
            role: spec.role,
            location: spec.location.clone(),
            motion: spec.motion.motion(),
            scale_regime: route.scale_regime,
            duration_seconds: spec.duration_seconds,
            frame_count: scene
                .animation
                .as_ref()
                .map_or(1, |animation| animation.frame_count),
            transition_out: spec.transition_out,
            scene: relative,
        });
    }
    if !report.schema_errors.is_empty() {
        return fail_schema(state, report);
    }

    report.schema_errors.extend(axis_mismatches(
        &compiled,
        &axes,
        &brief.locations[0],
        &tuning.classification,
    ));
    if !report.schema_errors.is_empty() {
        return fail_schema(state, report);
    }
    state.record_gate(
        "schema",
        true,
        "shotlist.yaml is well formed and matches the brief",
    )?;

    state.record_gate(
        "route-safety",
        report.unsafe_routes.is_empty(),
        if report.unsafe_routes.is_empty() {
            "every camera route keeps clear of the surface".to_owned()
        } else {
            report.unsafe_routes.join("; ")
        },
    )?;

    report.variety_errors = variety_errors(&compiled, tuning.shotlist.min_cut_differences);
    state.record_gate(
        "in-film-variety",
        report.variety_errors.is_empty(),
        if report.variety_errors.is_empty() {
            "consecutive cuts differ enough".to_owned()
        } else {
            report.variety_errors.join("; ")
        },
    )?;

    let total: f64 = compiled.iter().map(|cut| cut.duration_seconds).sum();
    report.total_seconds = Some(total);
    // The budget now follows the real film length instead of the estimate.
    budget.planned_duration_seconds = total;
    production.save_budget(budget)?;

    let manifest = Manifest {
        shotlist_hash: content_hash(&text),
        format: shotlist.format,
        transition_frames: transition_frames(
            shotlist.format.fps,
            tuning.shotlist.transition_seconds,
        ),
        cuts: compiled,
    };
    let mut json =
        serde_json::to_string_pretty(&manifest).context("could not serialize manifest")?;
    json.push('\n');
    fs::write(production.path(MANIFEST_FILE), json).context("could not write the manifest")?;
    Ok(report)
}

fn structure_errors(
    shotlist: &Shotlist,
    brief_locations: &[String],
    index: &Index,
    budget: &Budget,
    rules: &ShotlistRules,
) -> Vec<String> {
    let mut errors = Vec::new();
    if shotlist.version != SHOTLIST_VERSION {
        errors.push(format!(
            "version must be {SHOTLIST_VERSION}, not {}",
            shotlist.version
        ));
    }
    if let Err(error) = shotlist.format.validate() {
        errors.push(error.to_string());
    }
    let limits = &budget.film;
    let count = shotlist.cuts.len();
    if !(limits.min_cuts..=limits.max_cuts).contains(&count) {
        errors.push(format!(
            "the film has {count} cuts; it must have {} to {}",
            limits.min_cuts, limits.max_cuts
        ));
    }
    let total: f64 = shotlist.cuts.iter().map(|cut| cut.duration_seconds).sum();
    if !total.is_finite()
        || !(limits.min_duration_seconds..=limits.max_duration_seconds).contains(&total)
    {
        errors.push(format!(
            "the film lasts {total} s; it must last {} to {} s",
            limits.min_duration_seconds, limits.max_duration_seconds
        ));
    }

    for (position, cut) in shotlist.cuts.iter().enumerate() {
        let expected = position as u32 + 1;
        if cut.id != expected {
            errors.push(format!(
                "cut ids must count up from 1; found {} at position {expected}",
                cut.id
            ));
        }
        if !brief_locations.contains(&cut.location) {
            errors.push(format!(
                "cut {}: location {} is not named in the brief",
                cut.id, cut.location
            ));
        } else if !index.locations[&cut.location]
            .usable_viewpoints()
            .any(|view| view.id == cut.viewpoint)
        {
            errors.push(format!(
                "cut {}: {} has no usable viewpoint {}",
                cut.id, cut.location, cut.viewpoint
            ));
        }

        let last = position + 1 == count;
        if last && cut.transition_out != Transition::Cut {
            errors.push(format!(
                "cut {}: the last cut must end with transition_out: cut",
                cut.id
            ));
        }
        let previous = position
            .checked_sub(1)
            .map(|index| shotlist.cuts[index].transition_out);
        let has_transition = cut.transition_out != Transition::Cut
            || previous.is_some_and(|transition| transition != Transition::Cut);
        if has_transition && cut.duration_seconds < rules.min_transition_cut_seconds {
            errors.push(format!(
                "cut {}: a cut with a fade or crossfade must last at least {} s",
                cut.id, rules.min_transition_cut_seconds
            ));
        }
    }
    errors
}

/// Checks that the film is what the brief said it would be.
fn axis_mismatches(
    cuts: &[CompiledCut],
    axes: &Axes,
    primary_location: &str,
    rules: &Classification,
) -> Vec<String> {
    fn longest<K: Ord + Copy>(cuts: &[CompiledCut], key: impl Fn(&CompiledCut) -> K) -> K {
        let mut seconds: BTreeMap<K, f64> = BTreeMap::new();
        for cut in cuts {
            *seconds.entry(key(cut)).or_default() += cut.duration_seconds;
        }
        seconds
            .into_iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(key, _)| key)
            .expect("a checked shot list has cuts")
    }

    let mut errors = Vec::new();
    let mut expect = |axis: AxisId, actual: &str| {
        let committed = axes.value(axis);
        if actual != committed {
            errors.push(format!(
                "the brief commits to {} {committed}, but the cuts amount to {actual}",
                axis.as_str()
            ));
        }
    };
    expect(AxisId::Motion, longest(cuts, |cut| cut.motion).as_str());
    expect(
        AxisId::ScaleRegime,
        longest(cuts, |cut| cut.scale_regime).as_str(),
    );
    let total: f64 = cuts.iter().map(|cut| cut.duration_seconds).sum();
    expect(
        AxisId::Pacing,
        classify::pacing(total / cuts.len() as f64, rules).as_str(),
    );

    let mut screen_time: BTreeMap<&str, f64> = BTreeMap::new();
    for cut in cuts {
        *screen_time.entry(&cut.location).or_default() += cut.duration_seconds;
    }
    let primary_seconds = screen_time.get(primary_location).copied().unwrap_or(0.0);
    if screen_time
        .values()
        .any(|seconds| *seconds > primary_seconds)
    {
        errors.push(format!(
            "the primary location {primary_location} must have the most screen time"
        ));
    }
    errors
}

fn variety_errors(cuts: &[CompiledCut], min_differences: usize) -> Vec<String> {
    cuts.windows(2)
        .filter_map(|pair| {
            let differences = usize::from(pair[0].role != pair[1].role)
                + usize::from(pair[0].scale_regime != pair[1].scale_regime)
                + usize::from(pair[0].motion != pair[1].motion);
            (differences < min_differences).then(|| {
                format!(
                    "cuts {} and {} differ in only {differences} of role, scale regime, and motion; {min_differences} are required",
                    pair[0].id, pair[1].id
                )
            })
        })
        .collect()
}

/// FNV-1a, written out so the hash is stable across toolchains.
#[must_use]
pub fn content_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        axes::{FormulaFamily, Light, Pacing, Palette, PaletteHue, PaletteKey},
        budget::FilmLimits,
        ledger::Ledger,
        metrics::Legibility,
        rng::Rng,
        scout::{LocationRecord, ViewRecord},
        shape,
        tuning::Scouting,
    };
    use std::path::PathBuf;

    /// A production in the shotlist stage whose single location, `loc-0001`,
    /// is a bounded shape with `vp-1` overview, `vp-2` mid, and `vp-3` deep.
    pub(crate) fn shotlist_stage(name: &str) -> (Production, State, Budget, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "fractal-harness-shotlist-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let budget = Budget {
            film: FilmLimits {
                min_duration_seconds: 8.0,
                max_duration_seconds: 40.0,
                min_cuts: 2,
                max_cuts: 4,
            },
            ..Budget::default()
        };
        let production =
            Production::create(&root, "film", &Ledger::default(), 1, budget.clone()).unwrap();

        let mut rng = Rng::new(12);
        let (shape, viewpoints) = loop {
            let Some(shape) = shape::discover(&mut rng, FormulaFamily::Mandelbox) else {
                continue;
            };
            let viewpoints = shape::viewpoints(&mut rng, &shape, &Scouting::default());
            if shape.survey.bounded && viewpoints.len() == 3 {
                break (shape, viewpoints);
            }
        };
        let mut views = Vec::new();
        for (number, viewpoint) in viewpoints.iter().enumerate() {
            let id = format!("vp-{}", number + 1);
            scout::save_viewpoint_scene(
                &production,
                "loc-0001",
                &id,
                &shape::thumbnail_config(&shape, viewpoint, 320, 180, 1, &Scouting::default()),
            )
            .unwrap();
            views.push(ViewRecord {
                id,
                scale_regime: viewpoint.scale_regime,
                legibility: Legibility {
                    high_frequency_ratio: 0.1,
                    negative_space_ratio: 0.4,
                    hue_clusters: 0,
                    clipped_high_ratio: 0.0,
                    clipped_low_ratio: 0.0,
                    mean_luminance: 0.4,
                },
                estimated_frame_seconds: 1.0,
                rejection: None,
                sheet: None,
            });
        }
        let index = Index {
            rounds: 1,
            next_location: 2,
            locations: BTreeMap::from([(
                "loc-0001".to_owned(),
                LocationRecord {
                    family: FormulaFamily::Mandelbox,
                    bounded: true,
                    radius: shape.survey.radius,
                    round: 1,
                    viewpoints: views,
                },
            )]),
            selected: vec!["loc-0001".to_owned()],
        };
        fs::write(
            production.path("scout/index.json"),
            serde_json::to_string(&index).unwrap(),
        )
        .unwrap();
        fs::write(
            production.path(brief::BRIEF_FILE),
            "version: 1\ntitle: T\nsubject: S.\naxes:\n  scale_regime: overview\n  motion: orbit\n  palette: { primary_hue: teal, key: mid }\n  light: top\n  pacing: medium\nlocations: [loc-0001]\narc: A.\n",
        )
        .unwrap();
        brief::save_axes(
            &production,
            &Axes {
                formula_family: FormulaFamily::Mandelbox,
                scale_regime: ScaleRegime::Overview,
                motion: Motion::Orbit,
                palette: Palette {
                    primary_hue: PaletteHue::Teal,
                    key: PaletteKey::Mid,
                },
                light: Light::Top,
                pacing: Pacing::Medium,
            },
        )
        .unwrap();

        let mut state = production.load_state().unwrap();
        state.stage = Stage::Shotlist;
        (production, state, budget, root)
    }

    /// Orbiting overview, a dive into the mid view, and an orbit to close:
    /// 24 s over three cuts, so medium paced, mostly orbit, mostly overview.
    pub(crate) const VALID_SHOTLIST: &str = "\
version: 1
format: { width: 1920, height: 1080, fps: 30 }
cuts:
  - id: 1
    role: establishing
    location: loc-0001
    viewpoint: vp-1
    motion: { kind: orbit, revolutions: 0.2 }
    duration_seconds: 10
    transition_out: crossfade
  - id: 2
    role: dive
    location: loc-0001
    viewpoint: vp-2
    motion: { kind: dive, depth: 0.6 }
    duration_seconds: 6
    transition_out: fade-black
  - id: 3
    role: reveal
    location: loc-0001
    viewpoint: vp-1
    motion: { kind: orbit, revolutions: -0.15 }
    duration_seconds: 8
    transition_out: cut
";

    pub(crate) fn write_shotlist(production: &Production, text: &str) {
        fs::write(production.path(SHOTLIST_FILE), text).unwrap();
    }

    #[test]
    fn a_valid_shot_list_compiles_and_passes_its_gates() {
        let (production, mut state, mut budget, root) = shotlist_stage("valid");
        write_shotlist(&production, VALID_SHOTLIST);
        let report = check(&production, &mut state, &mut budget).unwrap();

        assert!(report.schema_errors.is_empty(), "{report:#?}");
        assert!(report.unsafe_routes.is_empty(), "{report:#?}");
        assert!(report.variety_errors.is_empty(), "{report:#?}");
        assert_eq!(report.total_seconds, Some(24.0));
        // Rendering-based gates remain, so the stage does not advance.
        assert_eq!(state.unmet_gates(), vec!["legibility", "critic"]);
        assert_eq!(budget.planned_duration_seconds, 24.0);
        assert_eq!(
            production.load_budget().unwrap().planned_duration_seconds,
            24.0
        );

        let manifest = Manifest::load_current(&production).unwrap();
        assert_eq!(manifest.cuts.len(), 3);
        assert_eq!(manifest.cuts[1].motion, Motion::Dive);
        assert_eq!(manifest.cuts[0].frame_count, 301);
        // One crossfade of 15 frames overlaps two cuts.
        assert_eq!(manifest.film_frames(), 301 + 181 + 241 - 15);
        for cut in &manifest.cuts {
            let scene = load_scene(production.path(&cut.scene)).unwrap();
            assert_eq!(scene.config.render.width, 1920);
            assert!(scene.animation.is_some());
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn editing_the_shot_list_invalidates_the_manifest() {
        let (production, mut state, mut budget, root) = shotlist_stage("stale");
        write_shotlist(&production, VALID_SHOTLIST);
        check(&production, &mut state, &mut budget).unwrap();
        write_shotlist(&production, &format!("{VALID_SHOTLIST}\n# tweak\n"));
        let error = Manifest::load_current(&production).unwrap_err();
        assert!(error.to_string().contains("changed after it was checked"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn structural_problems_fail_the_schema_gate() {
        let (production, mut state, mut budget, root) = shotlist_stage("structure");
        let mut errors_for = |text: &str| {
            write_shotlist(&production, text);
            let report = check(&production, &mut state, &mut budget).unwrap();
            assert!(!state.gates["schema"].passed);
            assert!(Manifest::load_current(&production).is_err());
            report.schema_errors.join("\n")
        };

        assert!(errors_for("version: 1\n").contains("schema"));
        // Quality settings are not part of a shot list.
        assert!(
            errors_for(&VALID_SHOTLIST.replace("version: 1", "version: 1\nsamples_per_pixel: 64"))
                .contains("schema")
        );
        assert!(errors_for(&VALID_SHOTLIST.replace("fps: 30", "fps: 31")).contains("fps 31"));
        assert!(errors_for(&VALID_SHOTLIST.replace("id: 3", "id: 5")).contains("count up from 1"));
        assert!(
            errors_for(&VALID_SHOTLIST.replace("viewpoint: vp-2", "viewpoint: vp-9"))
                .contains("no usable viewpoint vp-9")
        );
        assert!(
            errors_for(&VALID_SHOTLIST.replacen("loc-0001", "loc-0042", 1))
                .contains("not named in the brief")
        );
        assert!(
            errors_for(&VALID_SHOTLIST.replace("duration_seconds: 8", "duration_seconds: 30"))
                .contains("must last 8 to 40 s")
        );
        assert!(
            errors_for(&VALID_SHOTLIST.replace("transition_out: cut", "transition_out: crossfade"))
                .contains("last cut must end")
        );
        assert!(errors_for(&VALID_SHOTLIST.replace("depth: 0.6", "depth: 5.0")).contains("cut 2"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_film_that_departs_from_its_brief_fails_the_schema_gate() {
        let (production, mut state, mut budget, root) = shotlist_stage("axes");
        // Dives now outlast orbits, and the cuts are long enough to be slow.
        write_shotlist(
            &production,
            &VALID_SHOTLIST
                .replace("duration_seconds: 6", "duration_seconds: 20")
                .replace("duration_seconds: 8", "duration_seconds: 9"),
        );
        let report = check(&production, &mut state, &mut budget).unwrap();
        let errors = report.schema_errors.join("\n");
        assert!(errors.contains("commits to motion orbit"), "{errors}");
        assert!(errors.contains("amount to dive"), "{errors}");
        assert!(errors.contains("commits to pacing medium"), "{errors}");
        assert!(!state.gates["schema"].passed);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsafe_routes_and_repetitive_cuts_fail_their_own_gates() {
        let (production, mut state, mut budget, root) = shotlist_stage("gates");
        write_shotlist(
            &production,
            &VALID_SHOTLIST
                // A full turn at arm's length from a close-up subject sweeps
                // the camera through the surrounding surface.
                .replace("viewpoint: vp-2", "viewpoint: vp-3")
                .replace(
                    "motion: { kind: dive, depth: 0.6 }",
                    "motion: { kind: orbit, revolutions: 1.0 }",
                )
                // Cuts 2 and 3 now differ in scale regime alone.
                .replace("role: reveal", "role: dive"),
        );
        let report = check(&production, &mut state, &mut budget).unwrap();
        assert!(report.schema_errors.is_empty(), "{report:#?}");
        assert!(state.gates["schema"].passed);
        assert!(!state.gates["route-safety"].passed);
        assert!(report.unsafe_routes[0].starts_with("cut 2"));
        assert!(!state.gates["in-film-variety"].passed);
        assert!(report.variety_errors[0].contains("cuts 2 and 3"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hashes_are_stable_and_content_sensitive() {
        assert_eq!(content_hash(""), "cbf29ce484222325");
        assert_eq!(content_hash("a"), "af63dc4c8601ec8c");
        assert_ne!(content_hash("cut 1"), content_hash("cut 2"));
    }
}
