//! Vocabulary available to a director. This is the only description of the
//! design space an agent receives; existing scene files stay out of reach.

use serde_json::{Value, json};

use crate::{
    axes::{AxisId, PaletteKey},
    budget::Budget,
    production::AUTHORED_FILES,
    stage::Stage,
    tuning::Tuning,
};

/// Roles a cut can play within a film.
pub const CUT_ROLES: [&str; 5] = ["establishing", "dive", "detail", "reveal", "transit"];
/// Ways one cut can hand over to the next.
pub const TRANSITIONS: [&str; 3] = ["cut", "fade-black", "crossfade"];
/// Camera path kinds implemented by the renderer.
pub const PATH_KINDS: [&str; 4] = [
    "exponential-dive",
    "target-orbit",
    "multi-target-dive",
    "surface-flyover",
];

#[must_use]
pub fn describe(tuning: &Tuning) -> Value {
    let axes: Vec<Value> = AxisId::ALL
        .iter()
        .map(|axis| json!({ "axis": axis, "values": axis.domain() }))
        .collect();
    let stages: Vec<Value> = Stage::ALL
        .iter()
        .map(|stage| {
            json!({
                "stage": stage,
                "code": stage.code(),
                "gates": stage.gates(),
                "critic_review": stage.has_critic(),
                "can_kill": stage.can_kill(),
            })
        })
        .collect();
    let palette_keys: Vec<&str> = PaletteKey::ALL.iter().map(|key| key.as_str()).collect();

    json!({
        "authored_files": AUTHORED_FILES,
        "axes": axes,
        "palette_keys": palette_keys,
        "tuning": tuning,
        "cut_roles": CUT_ROLES,
        "transitions": TRANSITIONS,
        "path_kinds": PATH_KINDS,
        "stages": stages,
        "default_budget": Budget::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_axis_and_stage() {
        let description = describe(&Tuning::default());
        assert_eq!(description["tuning"]["limits"]["kills"], 2);
        assert_eq!(
            description["axes"].as_array().unwrap().len(),
            AxisId::ALL.len()
        );
        assert_eq!(
            description["stages"].as_array().unwrap().len(),
            Stage::ALL.len()
        );
        assert_eq!(description["axes"][0]["axis"], "formula_family");
        assert_eq!(description["stages"][2]["code"], "S3");
        assert_eq!(description["stages"][2]["critic_review"], true);
    }

    /// The renderer rejects unknown path kinds, so a stale list here would
    /// advertise a motion the harness cannot compile.
    #[test]
    fn path_kinds_are_accepted_by_the_scene_schema() {
        // Empty parameters never form a valid path, so every kind fails; the
        // schema names the kind only when it does not recognise it.
        let kind_error = |kind: &str| {
            let yaml = format!(
                "{}\nanimation:\n  fps: 30\n  frame_count: 2\n  path:\n    kind: {kind}\n    parameters: {{}}\n",
                include_str!("../../scenes/examples/mandelbulb.yaml")
            );
            let error = fractal_renderer_core::parse_scene(&yaml).unwrap_err();
            format!("{error:#}")
        };

        let control = kind_error("no-such-path");
        assert!(control.contains("unknown variant"), "{control}");
        for kind in PATH_KINDS {
            let error = kind_error(kind);
            assert!(
                !error.contains("unknown variant"),
                "path kind {kind} is not known to the renderer: {error}"
            );
        }
    }
}
