//! The brief: a film's concept, stated before any shot is planned. It commits
//! the film to axis values, which the diversity rules judge here and which
//! later stages must realise.

use std::fs;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    axes::{Axes, AxisId, Light, Motion, Pacing, Palette, ScaleRegime},
    cut::{self, Format, MotionSpec},
    ledger::{AxisDiffViolation, Ledger},
    mandate::{InfeasibleReport, Mandate, MandateViolation, resolve_infeasible},
    production::Production,
    scout::{self, Index},
    stage::{Stage, State},
    tuning::Tuning,
};
use fractal_renderer_core::load_scene;

pub const BRIEF_VERSION: u32 = 1;
pub const BRIEF_FILE: &str = "brief.yaml";
/// Harness-owned record of the axes the brief committed to.
pub const AXES_FILE: &str = "axes.json";

const MAX_TITLE_CHARS: usize = 80;
/// A subject must be sayable in one sentence.
const MAX_SUBJECT_CHARS: usize = 120;
const MAX_ARC_CHARS: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Brief {
    pub version: u32,
    pub title: String,
    /// What the film shows, in one sentence. The critic checks images
    /// against it.
    pub subject: String,
    pub axes: DeclaredAxes,
    /// Locations the film uses. The first is the primary location and
    /// decides the film's formula family.
    pub locations: Vec<String>,
    /// How the film develops from first cut to last.
    pub arc: String,
}

/// Axes a director chooses. The formula family is not among them: it follows
/// from the primary location.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredAxes {
    pub scale_regime: ScaleRegime,
    pub motion: Motion,
    pub palette: Palette,
    pub light: Light,
    pub pacing: Pacing,
}

#[derive(Clone, Debug, Serialize)]
pub struct BriefReport {
    pub schema_errors: Vec<String>,
    /// Axes the film is committed to; absent while the schema is invalid.
    pub axes: Option<Axes>,
    pub mandate_violations: Vec<MandateViolation>,
    pub axis_diff_violations: Vec<AxisDiffViolation>,
    /// Set when the mandated motion cannot be realised on any selected
    /// location; the brief was not judged further.
    pub infeasible: Option<InfeasibleReport>,
    pub advanced_to: Option<Stage>,
}

/// Reads `brief.yaml` and judges every gate of the brief stage.
pub fn check(
    production: &Production,
    state: &mut State,
    mandate: &mut Mandate,
    ledger: &Ledger,
) -> Result<BriefReport> {
    if state.stage != Stage::Brief {
        bail!(
            "the brief is checked in stage brief; the production is in stage {}",
            state.stage.as_str()
        );
    }
    let index = Index::load(production)?;
    let tuning = production.load_tuning()?;
    // All three gates are judged from the file, so earlier results are void.
    state.reset_gates();

    let mut report = BriefReport {
        schema_errors: Vec::new(),
        axes: None,
        mandate_violations: Vec::new(),
        axis_diff_violations: Vec::new(),
        infeasible: None,
        advanced_to: None,
    };
    let brief = match load(production) {
        Ok(brief) => brief,
        Err(error) => {
            report.schema_errors.push(format!("{error:#}"));
            state.record_gate("schema", false, report.schema_errors.join("; "))?;
            return Ok(report);
        }
    };
    report.schema_errors = schema_errors(&brief, &index);
    if !report.schema_errors.is_empty() {
        state.record_gate("schema", false, report.schema_errors.join("; "))?;
        return Ok(report);
    }

    // A mandated motion that no selected location supports cannot be met
    // by any brief, so it is resolved before the brief is judged.
    if let Some(motion) = mandate
        .assigned_value(AxisId::Motion)
        .and_then(Motion::parse)
        && !motion_feasible(production, &index, motion, &tuning)?
    {
        report.infeasible = resolve_infeasible(
            state,
            mandate,
            ledger,
            AxisId::Motion,
            format!("no selected location supports a {} path", motion.as_str()),
            index.selected.clone(),
        );
        production.save_mandate(mandate)?;
        return Ok(report);
    }
    state.record_gate("schema", true, "brief.yaml is well formed")?;

    let primary = &index.locations[&brief.locations[0]];
    let axes = Axes {
        formula_family: primary.family,
        scale_regime: brief.axes.scale_regime,
        motion: brief.axes.motion,
        palette: brief.axes.palette,
        light: brief.axes.light,
        pacing: brief.axes.pacing,
    };

    report.mandate_violations = mandate.violations(&axes);
    state.record_gate(
        "mandate",
        report.mandate_violations.is_empty(),
        if report.mandate_violations.is_empty() {
            "the brief satisfies the mandate".to_owned()
        } else {
            serde_json::to_string(&report.mandate_violations)?
        },
    )?;

    report.axis_diff_violations = ledger.axis_diff_violations(&axes, &tuning.diversity);
    state.record_gate(
        "axis-diff",
        report.axis_diff_violations.is_empty(),
        if report.axis_diff_violations.is_empty() {
            "the brief differs enough from every recent film".to_owned()
        } else {
            serde_json::to_string(&report.axis_diff_violations)?
        },
    )?;

    save_axes(production, &axes)?;
    report.axes = Some(axes);
    report.advanced_to = state.advance_if_ready()?;
    Ok(report)
}

/// Raster used to probe whether a motion can be planned at all.
const PROBE_FORMAT: Format = Format {
    width: 1920,
    height: 1080,
    fps: 30,
};
const PROBE_SECONDS: f64 = 12.0;

/// Whether any selected location can carry a typical path of this motion.
fn motion_feasible(
    production: &Production,
    index: &Index,
    motion: Motion,
    tuning: &Tuning,
) -> Result<bool> {
    if matches!(motion, Motion::Orbit | Motion::Dive) {
        // These start from a scouted viewpoint and always plan.
        return Ok(true);
    }
    for location in &index.selected {
        let record = &index.locations[location];
        // The automatic paths search from outside a bounding sphere.
        if !record.bounded {
            continue;
        }
        let Some(view) = record.usable_viewpoints().next() else {
            continue;
        };
        let viewpoint = load_scene(scout::viewpoint_scene_path(production, location, &view.id))?;
        if cut::compile(
            &viewpoint,
            &record.survey(),
            &MotionSpec::typical(motion),
            PROBE_SECONDS,
            &PROBE_FORMAT,
            "probe",
            tuning,
        )
        .is_ok()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn load(production: &Production) -> Result<Brief> {
    let path = production.path(BRIEF_FILE);
    let text =
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    serde_yaml_ng::from_str(&text)
        .with_context(|| format!("{BRIEF_FILE} does not match the brief schema"))
}

/// Axes the brief committed the film to.
pub fn load_axes(production: &Production) -> Result<Axes> {
    let path = production.path(AXES_FILE);
    let text =
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("invalid {}", path.display()))
}

pub(crate) fn save_axes(production: &Production, axes: &Axes) -> Result<()> {
    let path = production.path(AXES_FILE);
    let mut text = serde_json::to_string_pretty(axes).context("could not serialize axes")?;
    text.push('\n');
    fs::write(&path, text).with_context(|| format!("could not write {}", path.display()))
}

fn schema_errors(brief: &Brief, index: &Index) -> Vec<String> {
    let mut errors = Vec::new();
    if brief.version != BRIEF_VERSION {
        errors.push(format!(
            "version must be {BRIEF_VERSION}, not {}",
            brief.version
        ));
    }
    let mut text = |name: &str, value: &str, max_chars: usize, single_line: bool| {
        let length = value.trim().chars().count();
        if length == 0 {
            errors.push(format!("{name} must not be empty"));
        } else if length > max_chars {
            errors.push(format!(
                "{name} has {length} characters; the limit is {max_chars}"
            ));
        } else if single_line && value.trim().contains('\n') {
            errors.push(format!("{name} must be a single line"));
        }
    };
    text("title", &brief.title, MAX_TITLE_CHARS, true);
    text("subject", &brief.subject, MAX_SUBJECT_CHARS, true);
    text("arc", &brief.arc, MAX_ARC_CHARS, false);

    if brief.locations.is_empty() {
        errors.push("locations must name at least one selected location".to_owned());
    }
    for (position, location) in brief.locations.iter().enumerate() {
        if brief.locations[..position].contains(location) {
            errors.push(format!("location {location} is listed more than once"));
        } else if !index.selected.contains(location) {
            errors.push(format!(
                "location {location} is not among the selected locations {:?}",
                index.selected
            ));
        }
    }
    errors
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        axes::{AxisId, FormulaFamily},
        budget::Budget,
        ledger::tests::entry,
        mandate::Assignment,
        scout::tests::{ids, seed_index},
    };
    use std::path::PathBuf;

    pub(crate) const VALID_BRIEF: &str = "\
version: 1
title: Glass Reef
subject: A pale reef of stacked plates seen from inside its own canyon.
axes:
  scale_regime: mid
  motion: flyover
  palette: { primary_hue: amber, key: high }
  light: backlit
  pacing: medium
locations: [loc-0001, loc-0003]
arc: Open on the whole reef, descend between the plates, end looking back out.
";

    /// A production in the brief stage with loc-0001 and loc-0003 selected.
    pub(crate) fn brief_stage(name: &str) -> (Production, State, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "fractal-harness-brief-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let production =
            Production::create(&root, "film", &Ledger::default(), 1, Budget::default()).unwrap();
        seed_index(&production);
        let mut state = production.load_state().unwrap();
        crate::scout::select(
            &production,
            &mut state,
            &Budget::default(),
            &Ledger::default(),
            &ids(&["loc-0001", "loc-0003"]),
        )
        .unwrap();
        assert_eq!(state.stage, Stage::Brief);
        (production, state, root)
    }

    fn write_brief(production: &Production, text: &str) {
        fs::write(production.path(BRIEF_FILE), text).unwrap();
    }

    fn no_mandate() -> Mandate {
        Mandate {
            seed: 0,
            banned_families: Vec::new(),
            assigned: Vec::new(),
            retired: Vec::new(),
        }
    }

    #[test]
    fn a_valid_brief_passes_and_advances() {
        let (production, mut state, root) = brief_stage("valid");
        write_brief(&production, VALID_BRIEF);
        let report = check(
            &production,
            &mut state,
            &mut no_mandate(),
            &Ledger::default(),
        )
        .unwrap();

        assert!(report.schema_errors.is_empty(), "{report:?}");
        assert_eq!(report.advanced_to, Some(Stage::Shotlist));
        assert_eq!(state.stage, Stage::Shotlist);
        // The seeded locations are julia-box; the family comes from them.
        let axes = report.axes.unwrap();
        assert_eq!(axes.formula_family, FormulaFamily::JuliaBox);
        assert_eq!(axes.motion, Motion::Flyover);
        assert_eq!(load_axes(&production).unwrap(), axes);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_or_malformed_brief_fails_the_schema_gate() {
        let (production, mut state, root) = brief_stage("malformed");
        let report = check(
            &production,
            &mut state,
            &mut no_mandate(),
            &Ledger::default(),
        )
        .unwrap();
        assert!(report.schema_errors[0].contains("brief.yaml"));
        assert_eq!(state.unmet_gates(), Stage::Brief.gates());
        assert!(!state.gates["schema"].passed);

        // Quality settings and unknown fields are not part of a brief.
        write_brief(
            &production,
            &format!("{VALID_BRIEF}samples_per_pixel: 64\n"),
        );
        let report = check(
            &production,
            &mut state,
            &mut no_mandate(),
            &Ledger::default(),
        )
        .unwrap();
        assert!(report.schema_errors[0].contains("schema"), "{report:?}");

        // Axis values outside the vocabulary are rejected by the schema.
        write_brief(&production, &VALID_BRIEF.replace("flyover", "teleport"));
        let report = check(
            &production,
            &mut state,
            &mut no_mandate(),
            &Ledger::default(),
        )
        .unwrap();
        assert!(!report.schema_errors.is_empty());
        assert_eq!(state.stage, Stage::Brief);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn field_rules_are_reported_together() {
        let (production, mut state, root) = brief_stage("fields");
        let long_subject = "x".repeat(MAX_SUBJECT_CHARS + 1);
        write_brief(
            &production,
            &VALID_BRIEF
                .replace("version: 1", "version: 2")
                .replace("title: Glass Reef", "title: \"  \"")
                .replace(
                    "A pale reef of stacked plates seen from inside its own canyon.",
                    &long_subject,
                )
                .replace("[loc-0001, loc-0003]", "[loc-0001, loc-0001, loc-0000]"),
        );
        let report = check(
            &production,
            &mut state,
            &mut no_mandate(),
            &Ledger::default(),
        )
        .unwrap();
        let errors = report.schema_errors.join("\n");
        for expected in [
            "version must be 1",
            "title must not be empty",
            "subject has 121 characters",
            "loc-0001 is listed more than once",
            "loc-0000 is not among the selected",
        ] {
            assert!(
                errors.contains(expected),
                "missing {expected:?} in:\n{errors}"
            );
        }
        assert!(report.axes.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mandate_and_axis_rules_are_judged_separately() {
        let (production, mut state, root) = brief_stage("rules");
        write_brief(&production, VALID_BRIEF);
        let mut mandate = Mandate {
            seed: 0,
            banned_families: vec![FormulaFamily::JuliaBox],
            assigned: vec![Assignment {
                axis: AxisId::Motion,
                value: "orbit".to_owned(),
            }],
            retired: Vec::new(),
        };
        // Differs from the brief only in family and scale regime.
        let mut near = entry("near", FormulaFamily::Mandelbox, Motion::Flyover);
        near.axes.scale_regime = ScaleRegime::Deep;
        near.axes.palette = Palette {
            primary_hue: crate::axes::PaletteHue::Amber,
            key: crate::axes::PaletteKey::Low,
        };
        near.axes.light = Light::Backlit;
        near.axes.pacing = Pacing::Medium;
        let ledger = Ledger::from_entries(vec![near]).unwrap();

        let report = check(&production, &mut state, &mut mandate, &ledger).unwrap();
        assert_eq!(report.mandate_violations.len(), 2);
        assert_eq!(report.axis_diff_violations.len(), 1);
        assert_eq!(report.axis_diff_violations[0].film, "near");
        assert!(state.gates["schema"].passed);
        assert!(!state.gates["mandate"].passed);
        assert!(!state.gates["axis-diff"].passed);
        assert_eq!(report.advanced_to, None);

        // Fixing the brief clears both gates on the next check.
        write_brief(
            &production,
            &VALID_BRIEF
                .replace("motion: flyover", "motion: orbit")
                .replace("light: backlit", "light: top"),
        );
        let mut mandate = Mandate {
            banned_families: Vec::new(),
            ..mandate
        };
        let report = check(&production, &mut state, &mut mandate, &ledger).unwrap();
        assert_eq!(report.advanced_to, Some(Stage::Shotlist));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unrealisable_motion_is_redrawn_instead_of_judged() {
        use crate::stage::InfeasibleOutcome;
        let (production, mut state, root) = brief_stage("infeasible");
        write_brief(&production, VALID_BRIEF);
        // Space-filling shapes cannot carry a flyover.
        let mut index = Index::load(&production).unwrap();
        for record in index.locations.values_mut() {
            record.bounded = false;
        }
        fs::write(
            production.path("scout/index.json"),
            serde_json::to_string(&index).unwrap(),
        )
        .unwrap();
        let mut mandate = Mandate {
            seed: 3,
            banned_families: Vec::new(),
            assigned: vec![Assignment {
                axis: AxisId::Motion,
                value: "flyover".to_owned(),
            }],
            retired: Vec::new(),
        };

        let report = check(&production, &mut state, &mut mandate, &Ledger::default()).unwrap();
        let infeasible = report.infeasible.unwrap();
        assert_eq!(infeasible.outcome, InfeasibleOutcome::Redrawn);
        assert_eq!(infeasible.value, "flyover");
        assert_ne!(mandate.assigned_value(AxisId::Motion), Some("flyover"));
        assert_eq!(state.stage, Stage::Brief);
        assert_eq!(state.mandate_redraws, 1);
        assert!(state.gates.is_empty());
        // The redrawn mandate is what later commands will read.
        assert_eq!(production.load_mandate().unwrap(), mandate);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_brief_is_only_checked_in_its_stage() {
        let (production, mut state, root) = brief_stage("stage");
        state.stage = Stage::Look;
        assert!(
            check(
                &production,
                &mut state,
                &mut no_mandate(),
                &Ledger::default()
            )
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
