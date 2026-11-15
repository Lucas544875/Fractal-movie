//! Constraints drawn by the harness when a production starts. They push each
//! film away from the portfolio before any design decision is made.

use serde::{Deserialize, Serialize};

use crate::{
    axes::{Axes, AxisId, FormulaFamily},
    ledger::Ledger,
    rng::Rng,
};

/// Number of axes whose value the harness prescribes.
pub const ASSIGNED_AXES: usize = 2;

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

impl Mandate {
    /// Draws the constraints for a new film from the portfolio and a seed.
    #[must_use]
    pub fn draw(ledger: &Ledger, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let banned_families = ledger.banned_families();

        let mut axes = AxisId::ALL.to_vec();
        let mut assigned = Vec::with_capacity(ASSIGNED_AXES);
        while assigned.len() < ASSIGNED_AXES {
            let Some(axis) = rng.take(&mut axes) else {
                break;
            };
            let mut candidates = least_used_values(ledger, axis, &banned_families);
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
        }
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

fn least_used_values(
    ledger: &Ledger,
    axis: AxisId,
    banned_families: &[FormulaFamily],
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
        assert_eq!(Mandate::draw(&ledger, 11), Mandate::draw(&ledger, 11));
    }

    #[test]
    fn draws_two_distinct_axes_with_valid_values() {
        let ledger = portfolio();
        for seed in 0..200 {
            let mandate = Mandate::draw(&ledger, seed);
            assert_eq!(mandate.assigned.len(), ASSIGNED_AXES);
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
            let mandate = Mandate::draw(&ledger, seed);
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
    fn different_seeds_spread_over_axes() {
        let ledger = portfolio();
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..200 {
            for assignment in Mandate::draw(&ledger, seed).assigned {
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
    fn violations_name_each_broken_constraint() {
        let mandate = Mandate {
            seed: 0,
            banned_families: vec![FormulaFamily::Mandelbox],
            assigned: vec![Assignment {
                axis: AxisId::Motion,
                value: "flyover".to_owned(),
            }],
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
