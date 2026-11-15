//! Constraints drawn by the harness when a production starts. They push each
//! film away from the portfolio before any design decision is made.

use serde::{Deserialize, Serialize};

use crate::{
    axes::{Axes, AxisId, FormulaFamily},
    ledger::Ledger,
    rng::Rng,
    stage::{InfeasibleOutcome, State},
    tuning::Diversity,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub axis: AxisId,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mandate {
    pub seed: u64,
    /// Families used by the most recent films. Scouting never samples them.
    pub banned_families: Vec<FormulaFamily>,
    /// Axis values the film must take.
    pub assigned: Vec<Assignment>,
    /// Assignments that proved impossible to realise and were replaced.
    #[serde(default)]
    pub retired: Vec<Assignment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MandateViolation {
    BannedFamily {
        family: FormulaFamily,
    },
    AssignedValue {
        axis: AxisId,
        required: String,
        actual: String,
    },
}

/// What happened to an assignment that could not be realised.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InfeasibleReport {
    pub axis: AxisId,
    pub value: String,
    pub reason: String,
    pub outcome: InfeasibleOutcome,
    /// The assignment that takes its place after a redraw, if any.
    pub replacement: Option<Assignment>,
}

/// Handles an assignment the harness found impossible to realise: redraw it
/// while redraws remain, otherwise drop the concept, otherwise waive it.
/// Returns `None` when `axis` carries no assignment.
pub fn resolve_infeasible(
    state: &mut State,
    mandate: &mut Mandate,
    ledger: &Ledger,
    axis: AxisId,
    reason: String,
    locations: Vec<String>,
) -> Option<InfeasibleReport> {
    let value = mandate.assigned_value(axis)?.to_owned();
    let outcome = state.mandate_infeasible(reason.clone(), locations);
    let replacement = match outcome {
        InfeasibleOutcome::Redrawn => mandate.redraw(ledger, axis),
        // The next concept may still realise the assignment.
        InfeasibleOutcome::Killed => None,
        InfeasibleOutcome::Waived => {
            mandate.waive(axis);
            None
        }
    };
    Some(InfeasibleReport {
        axis,
        value,
        reason,
        outcome,
        replacement,
    })
}

impl Mandate {
    /// Draws the constraints for a new film from the portfolio and a seed.
    #[must_use]
    pub fn draw(ledger: &Ledger, seed: u64, rules: &Diversity) -> Self {
        let mut rng = Rng::new(seed);
        let banned_families = ledger.banned_families(rules.family_ban_window);

        let mut axes = AxisId::ALL.to_vec();
        let mut assigned = Vec::with_capacity(rules.assigned_axes);
        while assigned.len() < rules.assigned_axes {
            let Some(axis) = rng.take(&mut axes) else {
                break;
            };
            let mut candidates = least_used_values_among(ledger, axis, &banned_families, |value| {
                compatible(&assigned, axis, value)
            });
            if let Some(value) = rng.take(&mut candidates) {
                assigned.push(Assignment {
                    axis,
                    value: value.to_owned(),
                });
            }
        }
        // Stable order keeps the file readable regardless of the draw order.
        assigned.sort_by_key(|assignment| assignment.axis);

        Self {
            seed,
            banned_families,
            assigned,
            retired: Vec::new(),
        }
    }

    /// Replaces the assignment on `axis` after it proved impossible to
    /// realise. The new value is the least used one that has not been tried;
    /// when none is left the axis is no longer prescribed. Returns the new
    /// assignment, if any.
    pub fn redraw(&mut self, ledger: &Ledger, axis: AxisId) -> Option<Assignment> {
        let position = self
            .assigned
            .iter()
            .position(|assignment| assignment.axis == axis)?;
        let failed = self.assigned.remove(position);
        self.retired.push(failed);

        let mut candidates: Vec<&'static str> =
            least_used_values_among(ledger, axis, &self.banned_families, |value| {
                compatible(&self.assigned, axis, value)
                    && !self
                        .retired
                        .iter()
                        .any(|retired| retired.axis == axis && retired.value == value)
            });
        // Each redraw draws from its own stream so repeats stay reproducible.
        let mut rng =
            Rng::new(self.seed ^ (self.retired.len() as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
        let replacement = rng.take(&mut candidates).map(|value| Assignment {
            axis,
            value: value.to_owned(),
        })?;
        self.assigned.push(replacement.clone());
        self.assigned.sort_by_key(|assignment| assignment.axis);
        Some(replacement)
    }

    /// Stops prescribing `axis` altogether.
    pub fn waive(&mut self, axis: AxisId) {
        if let Some(position) = self
            .assigned
            .iter()
            .position(|assignment| assignment.axis == axis)
        {
            let waived = self.assigned.remove(position);
            self.retired.push(waived);
        }
    }

    /// The value prescribed for `axis`, if any.
    #[must_use]
    pub fn assigned_value(&self, axis: AxisId) -> Option<&str> {
        self.assigned
            .iter()
            .find(|assignment| assignment.axis == axis)
            .map(|assignment| assignment.value.as_str())
    }

    /// Families scouting may sample, honouring both the ban and an assigned
    /// family.
    #[must_use]
    pub fn allowed_families(&self) -> Vec<FormulaFamily> {
        if let Some(assignment) = self
            .assigned
            .iter()
            .find(|assignment| assignment.axis == AxisId::FormulaFamily)
        {
            return FormulaFamily::parse(&assignment.value)
                .into_iter()
                .collect();
        }
        FormulaFamily::ALL
            .iter()
            .copied()
            .filter(|family| !self.banned_families.contains(family))
            .collect()
    }

    /// Everything about `axes` that breaks this mandate.
    #[must_use]
    pub fn violations(&self, axes: &Axes) -> Vec<MandateViolation> {
        let mut violations = Vec::new();
        if self.banned_families.contains(&axes.formula_family) {
            violations.push(MandateViolation::BannedFamily {
                family: axes.formula_family,
            });
        }
        for assignment in &self.assigned {
            let actual = axes.value(assignment.axis);
            if actual != assignment.value {
                violations.push(MandateViolation::AssignedValue {
                    axis: assignment.axis,
                    required: assignment.value.clone(),
                    actual: actual.to_owned(),
                });
            }
        }
        violations
    }
}

type AxisValue = (AxisId, &'static str);

/// Value pairs no film can realise together. A flyover keeps the camera a
/// fraction of the shape's radius above its surface, which is never far
/// enough out to see the shape whole.
const INCOMPATIBLE: [(AxisValue, AxisValue); 1] = [(
    (AxisId::Motion, "flyover"),
    (AxisId::ScaleRegime, "overview"),
)];

/// Whether `value` on `axis` can be realised alongside `assigned`.
fn compatible(assigned: &[Assignment], axis: AxisId, value: &str) -> bool {
    !INCOMPATIBLE.iter().any(|(first, second)| {
        [(first, second), (second, first)]
            .into_iter()
            .any(|(candidate, other)| {
                *candidate == (axis, value)
                    && assigned
                        .iter()
                        .any(|assignment| (assignment.axis, assignment.value.as_str()) == *other)
            })
    })
}

fn least_used_values_among(
    ledger: &Ledger,
    axis: AxisId,
    banned_families: &[FormulaFamily],
    eligible: impl Fn(&str) -> bool,
) -> Vec<&'static str> {
    let permitted: Vec<&'static str> = axis
        .domain()
        .into_iter()
        .filter(|value| {
            axis != AxisId::FormulaFamily
                || !banned_families
                    .iter()
                    .any(|family| family.as_str() == *value)
        })
        .filter(|value| eligible(value))
        .collect();
    let minimum = permitted
        .iter()
        .map(|value| ledger.usage(axis, value))
        .min()
        .unwrap_or(0);
    permitted
        .into_iter()
        .filter(|value| ledger.usage(axis, value) == minimum)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{axes::Motion, ledger::tests::entry};

    fn portfolio() -> Ledger {
        Ledger::from_entries(vec![
            entry("a", FormulaFamily::Mandelbox, Motion::Dive),
            entry("b", FormulaFamily::Mandelbox, Motion::Dive),
            entry("c", FormulaFamily::AmazingSurf, Motion::Orbit),
        ])
        .unwrap()
    }

    #[test]
    fn draw_is_deterministic_for_a_seed() {
        let ledger = portfolio();
        assert_eq!(
            Mandate::draw(&ledger, 11, &Diversity::default()),
            Mandate::draw(&ledger, 11, &Diversity::default())
        );
    }

    #[test]
    fn draws_two_distinct_axes_with_valid_values() {
        let ledger = portfolio();
        for seed in 0..200 {
            let mandate = Mandate::draw(&ledger, seed, &Diversity::default());
            assert_eq!(mandate.assigned.len(), Diversity::default().assigned_axes);
            assert_ne!(mandate.assigned[0].axis, mandate.assigned[1].axis);
            for assignment in &mandate.assigned {
                assert!(
                    assignment
                        .axis
                        .domain()
                        .contains(&assignment.value.as_str())
                );
            }
        }
    }

    #[test]
    fn assignments_avoid_used_values_and_banned_families() {
        let ledger = portfolio();
        for seed in 0..200 {
            let mandate = Mandate::draw(&ledger, seed, &Diversity::default());
            assert_eq!(
                mandate.banned_families,
                vec![FormulaFamily::AmazingSurf, FormulaFamily::Mandelbox]
            );
            for assignment in &mandate.assigned {
                assert_eq!(
                    ledger.usage(assignment.axis, &assignment.value),
                    0,
                    "seed {seed} assigned a used value {assignment:?}"
                );
            }
        }
    }

    #[test]
    fn unrealisable_pairs_are_never_drawn_or_redrawn() {
        let ledger = Ledger::default();
        let pair = |mandate: &Mandate| {
            mandate.assigned_value(AxisId::Motion) == Some("flyover")
                && mandate.assigned_value(AxisId::ScaleRegime) == Some("overview")
        };
        let mut both_axes_drawn = 0;
        for seed in 0..2000 {
            let mandate = Mandate::draw(&ledger, seed, &Diversity::default());
            assert!(!pair(&mandate), "seed {seed}: {mandate:?}");
            both_axes_drawn += usize::from(
                mandate.assigned_value(AxisId::Motion).is_some()
                    && mandate.assigned_value(AxisId::ScaleRegime).is_some(),
            );
        }
        // The rule must have had the chance to matter.
        assert!(both_axes_drawn > 50, "{both_axes_drawn}");

        // A redraw may not complete the pair either.
        for seed in 0..200 {
            let mut mandate = Mandate {
                seed,
                banned_families: Vec::new(),
                assigned: vec![
                    Assignment {
                        axis: AxisId::ScaleRegime,
                        value: "overview".to_owned(),
                    },
                    Assignment {
                        axis: AxisId::Motion,
                        value: "dive".to_owned(),
                    },
                ],
                retired: Vec::new(),
            };
            mandate.redraw(&ledger, AxisId::Motion);
            assert!(!pair(&mandate), "seed {seed}: {mandate:?}");
        }
    }

    #[test]
    fn different_seeds_spread_over_axes() {
        let ledger = portfolio();
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..200 {
            for assignment in Mandate::draw(&ledger, seed, &Diversity::default()).assigned {
                seen.insert(assignment.axis);
            }
        }
        assert_eq!(seen.len(), AxisId::ALL.len());
    }

    #[test]
    fn allowed_families_follow_ban_and_assignment() {
        let mut mandate = Mandate {
            seed: 0,
            banned_families: vec![FormulaFamily::Mandelbox],
            assigned: Vec::new(),
            retired: Vec::new(),
        };
        let allowed = mandate.allowed_families();
        assert!(!allowed.contains(&FormulaFamily::Mandelbox));
        assert_eq!(allowed.len(), FormulaFamily::ALL.len() - 1);

        mandate.assigned.push(Assignment {
            axis: AxisId::FormulaFamily,
            value: "julia-box".to_owned(),
        });
        assert_eq!(mandate.allowed_families(), vec![FormulaFamily::JuliaBox]);
    }

    #[test]
    fn the_number_of_assigned_axes_is_tunable() {
        let ledger = portfolio();
        for (count, window) in [(0, 0), (1, 1), (4, 3)] {
            let rules = Diversity {
                assigned_axes: count,
                family_ban_window: window,
                ..Diversity::default()
            };
            let mandate = Mandate::draw(&ledger, 7, &rules);
            assert_eq!(mandate.assigned.len(), count);
            assert_eq!(mandate.banned_families, ledger.banned_families(window));
        }
    }

    #[test]
    fn redraw_replaces_only_the_failed_axis_with_an_untried_value() {
        let ledger = portfolio();
        let mut mandate = Mandate {
            seed: 9,
            banned_families: Vec::new(),
            assigned: vec![
                Assignment {
                    axis: AxisId::Motion,
                    value: "flyover".to_owned(),
                },
                Assignment {
                    axis: AxisId::Light,
                    value: "backlit".to_owned(),
                },
            ],
            retired: Vec::new(),
        };

        // In the portfolio dive and orbit are used; multi-dive is the only
        // unused motion left once flyover is retired.
        let first = mandate.redraw(&ledger, AxisId::Motion).unwrap();
        assert_eq!(first.value, "multi-dive");
        assert_eq!(mandate.assigned_value(AxisId::Light), Some("backlit"));
        assert_eq!(mandate.retired.len(), 1);

        // Then the least used of the remaining values: orbit, used once.
        let second = mandate.redraw(&ledger, AxisId::Motion).unwrap();
        assert_eq!(second.value, "orbit");
        let third = mandate.redraw(&ledger, AxisId::Motion).unwrap();
        assert_eq!(third.value, "dive");

        // With every value tried, the axis is no longer prescribed.
        assert_eq!(mandate.redraw(&ledger, AxisId::Motion), None);
        assert_eq!(mandate.assigned_value(AxisId::Motion), None);
        assert_eq!(mandate.assigned.len(), 1);
        // An axis without an assignment has nothing to redraw.
        assert_eq!(mandate.redraw(&ledger, AxisId::Pacing), None);
    }

    #[test]
    fn redrawn_families_respect_the_ban() {
        let ledger = portfolio();
        for seed in 0..50 {
            let mut mandate = Mandate {
                seed,
                banned_families: ledger.banned_families(3),
                assigned: vec![Assignment {
                    axis: AxisId::FormulaFamily,
                    value: "mandelbulb".to_owned(),
                }],
                retired: Vec::new(),
            };
            let replacement = mandate.redraw(&ledger, AxisId::FormulaFamily).unwrap();
            assert_ne!(replacement.value, "mandelbulb");
            assert!(!["mandelbox", "amazing-surf"].contains(&replacement.value.as_str()));
        }
    }

    #[test]
    fn infeasible_assignments_are_resolved_in_order() {
        use crate::{stage::Stage, tuning::Limits};
        let ledger = portfolio();
        let mut mandate = Mandate {
            seed: 9,
            banned_families: Vec::new(),
            assigned: vec![Assignment {
                axis: AxisId::Motion,
                value: "flyover".to_owned(),
            }],
            retired: Vec::new(),
        };
        let mut state = State::new("film");
        state.stage = Stage::Brief;
        let resolve = |state: &mut State, mandate: &mut Mandate| {
            resolve_infeasible(
                state,
                mandate,
                &ledger,
                AxisId::Motion,
                "no surface".to_owned(),
                Vec::new(),
            )
            .unwrap()
        };

        for _ in 0..Limits::default().mandate_redraws {
            let report = resolve(&mut state, &mut mandate);
            assert_eq!(report.outcome, InfeasibleOutcome::Redrawn);
            assert_eq!(
                report
                    .replacement
                    .as_ref()
                    .map(|assignment| assignment.value.as_str()),
                mandate.assigned_value(AxisId::Motion)
            );
        }
        let kept = mandate.assigned_value(AxisId::Motion).unwrap().to_owned();
        for _ in 0..Limits::default().kills {
            let report = resolve(&mut state, &mut mandate);
            assert_eq!(report.outcome, InfeasibleOutcome::Killed);
            // A kill keeps the assignment for the next concept.
            assert_eq!(mandate.assigned_value(AxisId::Motion), Some(kept.as_str()));
            state.stage = Stage::Brief;
        }
        let report = resolve(&mut state, &mut mandate);
        assert_eq!(report.outcome, InfeasibleOutcome::Waived);
        assert_eq!(mandate.assigned_value(AxisId::Motion), None);
        assert!(state.compromised);

        // Nothing to resolve once the axis is free.
        assert!(
            resolve_infeasible(
                &mut state,
                &mut mandate,
                &ledger,
                AxisId::Motion,
                String::new(),
                Vec::new()
            )
            .is_none()
        );
    }

    #[test]
    fn waiving_drops_the_assignment() {
        let mut mandate = Mandate::draw(&portfolio(), 4, &Diversity::default());
        let axis = mandate.assigned[0].axis;
        mandate.waive(axis);
        assert_eq!(mandate.assigned_value(axis), None);
        assert_eq!(
            mandate.assigned.len(),
            Diversity::default().assigned_axes - 1
        );
        assert_eq!(mandate.retired.len(), 1);
    }

    #[test]
    fn violations_name_each_broken_constraint() {
        let mandate = Mandate {
            seed: 0,
            banned_families: vec![FormulaFamily::Mandelbox],
            assigned: vec![Assignment {
                axis: AxisId::Motion,
                value: "flyover".to_owned(),
            }],
            retired: Vec::new(),
        };
        let axes = entry("x", FormulaFamily::Mandelbox, Motion::Dive).axes;
        assert_eq!(
            mandate.violations(&axes),
            vec![
                MandateViolation::BannedFamily {
                    family: FormulaFamily::Mandelbox
                },
                MandateViolation::AssignedValue {
                    axis: AxisId::Motion,
                    required: "flyover".to_owned(),
                    actual: "dive".to_owned(),
                },
            ]
        );

        let compliant = entry("y", FormulaFamily::Mandelbulb, Motion::Flyover).axes;
        assert!(mandate.violations(&compliant).is_empty());
    }
}
