//! Stage machine of one production. The order of stages and the decision to
//! leave a stage belong to this module, never to an agent.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::tuning::Limits;

pub const STATE_VERSION: u32 = 1;

/// Gate decided by the critic's verdict rather than by a harness measurement.
pub const CRITIC_GATE: &str = "critic";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Scout,
    Brief,
    Shotlist,
    Look,
    Proof,
    Production,
    Encode,
    Done,
}

impl Stage {
    pub const ALL: &'static [Self] = &[
        Self::Scout,
        Self::Brief,
        Self::Shotlist,
        Self::Look,
        Self::Proof,
        Self::Production,
        Self::Encode,
        Self::Done,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scout => "scout",
            Self::Brief => "brief",
            Self::Shotlist => "shotlist",
            Self::Look => "look",
            Self::Proof => "proof",
            Self::Production => "production",
            Self::Encode => "encode",
            Self::Done => "done",
        }
    }

    /// Stage label used in the specification, e.g. `S3`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Scout => "S1",
            Self::Brief => "S2",
            Self::Shotlist => "S3",
            Self::Look => "S4",
            Self::Proof => "S5",
            Self::Production => "S6",
            Self::Encode => "S7",
            Self::Done => "done",
        }
    }

    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Scout => Some(Self::Brief),
            Self::Brief => Some(Self::Shotlist),
            Self::Shotlist => Some(Self::Look),
            Self::Look => Some(Self::Proof),
            Self::Proof => Some(Self::Production),
            Self::Production => Some(Self::Encode),
            Self::Encode => Some(Self::Done),
            Self::Done => None,
        }
    }

    /// Gates that must all pass before the stage can be left.
    #[must_use]
    pub const fn gates(self) -> &'static [&'static str] {
        match self {
            Self::Scout => &["locations-selected", "novelty", "cost-ceiling"],
            Self::Brief => &["schema", "mandate", "axis-diff"],
            Self::Shotlist => &[
                "schema",
                "route-safety",
                "in-film-variety",
                "legibility",
                CRITIC_GATE,
            ],
            Self::Look => &["schema", "hue-count", "clipping", CRITIC_GATE],
            Self::Proof => &["budget", "compression", "flicker", CRITIC_GATE],
            Self::Production => &["frames-complete"],
            Self::Encode => &["qc"],
            Self::Done => &[],
        }
    }

    #[must_use]
    pub fn has_critic(self) -> bool {
        self.gates().contains(&CRITIC_GATE)
    }

    /// Design stages can still be abandoned for a new scouting round.
    #[must_use]
    pub const fn can_kill(self) -> bool {
        matches!(
            self,
            Self::Brief | Self::Shotlist | Self::Look | Self::Proof
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateRecord {
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    Revise,
    Kill,
}

/// What a critic verdict did to the production.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum ReviewOutcome {
    Passed,
    /// The stage must be reworked. `remaining` further revisions are allowed.
    Revise {
        remaining: u32,
    },
    /// The production returned to scouting.
    Killed {
        remaining: u32,
    },
    /// A limit was reached, so the harness accepted the stage as it is and
    /// marked the film as compromised.
    LimitReached,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    Advanced {
        from: Stage,
        to: Stage,
        /// Gate results the stage was left with, kept for audit.
        #[serde(default)]
        gates: BTreeMap<String, GateRecord>,
    },
    Reviewed {
        stage: Stage,
        verdict: Verdict,
    },
    Killed {
        from: Stage,
        reason: String,
    },
    LimitReached {
        stage: Stage,
    },
    /// The stage was reopened because its inputs became invalid.
    Reopened {
        from: Stage,
        to: Stage,
        reason: String,
    },
    /// A mandate assignment could not be realised.
    Infeasible {
        reason: String,
        outcome: InfeasibleOutcome,
    },
}

/// What the harness did about a mandate assignment that cannot be realised.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InfeasibleOutcome {
    /// The assignment was replaced; the brief must be written again.
    Redrawn,
    /// Redraws were exhausted, so the concept was dropped.
    Killed,
    /// Redraws and kills were exhausted, so the assignment was waived and
    /// the film marked as compromised.
    Waived,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub version: u32,
    pub id: String,
    pub stage: Stage,
    /// Gate results for the current stage only.
    pub gates: BTreeMap<String, GateRecord>,
    /// Critic revisions consumed, per stage, since the last scouting round.
    pub revisions: BTreeMap<Stage, u32>,
    pub kills: u32,
    /// Locations abandoned by a kill. They cannot be selected again.
    pub banned_locations: Vec<String>,
    /// Mandate assignments replaced because they could not be realised.
    #[serde(default)]
    pub mandate_redraws: u32,
    /// How often this production may go back, fixed when it was created.
    #[serde(default)]
    pub limits: Limits,
    /// Set when a revision or kill limit forced the harness to move on.
    pub compromised: bool,
    pub history: Vec<Event>,
}

impl State {
    /// A new production with the default limits.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self::with_limits(id, Limits::default())
    }

    #[must_use]
    pub fn with_limits(id: impl Into<String>, limits: Limits) -> Self {
        Self {
            limits,
            version: STATE_VERSION,
            id: id.into(),
            stage: Stage::Scout,
            gates: BTreeMap::new(),
            revisions: BTreeMap::new(),
            kills: 0,
            banned_locations: Vec::new(),
            mandate_redraws: 0,
            compromised: false,
            history: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != STATE_VERSION {
            bail!(
                "unsupported production state version {} (expected {STATE_VERSION})",
                self.version
            );
        }
        if let Some(unknown) = self
            .gates
            .keys()
            .find(|gate| !self.stage.gates().contains(&gate.as_str()))
        {
            bail!(
                "state records gate {unknown:?}, which stage {} does not have",
                self.stage.as_str()
            );
        }
        Ok(())
    }

    /// Records a measurement-based gate result for the current stage.
    pub fn record_gate(
        &mut self,
        gate: &str,
        passed: bool,
        detail: impl Into<String>,
    ) -> Result<()> {
        if gate == CRITIC_GATE {
            bail!("the critic gate is decided by a review verdict");
        }
        self.set_gate(gate, passed, detail.into())
    }

    fn set_gate(&mut self, gate: &str, passed: bool, detail: String) -> Result<()> {
        if !self.stage.gates().contains(&gate) {
            bail!(
                "stage {} has no gate {gate:?}; its gates are {:?}",
                self.stage.as_str(),
                self.stage.gates()
            );
        }
        self.gates
            .insert(gate.to_owned(), GateRecord { passed, detail });
        Ok(())
    }

    /// Gates of the current stage that have not passed, in declaration order.
    #[must_use]
    pub fn unmet_gates(&self) -> Vec<&'static str> {
        self.stage
            .gates()
            .iter()
            .copied()
            .filter(|gate| !self.gates.get(*gate).is_some_and(|record| record.passed))
            .collect()
    }

    /// Gates other than the critic's that have not passed.
    fn unmet_measured_gates(&self) -> Vec<&'static str> {
        self.unmet_gates()
            .into_iter()
            .filter(|gate| *gate != CRITIC_GATE)
            .collect()
    }

    /// Moves to the next stage once every gate has passed.
    pub fn advance(&mut self) -> Result<Stage> {
        let Some(next) = self.stage.next() else {
            bail!("the production is already complete");
        };
        let unmet = self.unmet_gates();
        if !unmet.is_empty() {
            bail!(
                "cannot leave stage {}: gates not passed: {}",
                self.stage.as_str(),
                unmet.join(", ")
            );
        }
        self.history.push(Event::Advanced {
            from: self.stage,
            to: next,
            gates: std::mem::take(&mut self.gates),
        });
        self.stage = next;
        Ok(next)
    }

    /// Advances when every gate of the current stage has passed. Commands
    /// that judge gates call this, so no separate "proceed" step exists for
    /// an agent to forget or to attempt early.
    pub fn advance_if_ready(&mut self) -> Result<Option<Stage>> {
        if self.stage == Stage::Done || !self.unmet_gates().is_empty() {
            return Ok(None);
        }
        self.advance().map(Some)
    }

    /// Forgets every gate result of the current stage, for commands that
    /// re-judge the whole stage from changed inputs.
    pub fn reset_gates(&mut self) {
        self.gates.clear();
    }

    /// Applies a critic verdict to the current stage.
    ///
    /// The critic judges artifacts that already passed every measured gate,
    /// so a verdict is rejected while any of those gates is unmet.
    pub fn review(&mut self, verdict: Verdict, notes: impl Into<String>) -> Result<ReviewOutcome> {
        if !self.stage.has_critic() {
            bail!("stage {} has no critic review", self.stage.as_str());
        }
        let unmet = self.unmet_measured_gates();
        if !unmet.is_empty() {
            bail!(
                "stage {} cannot be reviewed before its measured gates pass: {}",
                self.stage.as_str(),
                unmet.join(", ")
            );
        }
        let notes = notes.into();
        self.history.push(Event::Reviewed {
            stage: self.stage,
            verdict,
        });

        match verdict {
            Verdict::Pass => {
                self.set_gate(CRITIC_GATE, true, notes)?;
                Ok(ReviewOutcome::Passed)
            }
            Verdict::Revise => {
                let used = self.revisions.entry(self.stage).or_insert(0);
                if *used >= self.limits.revisions_per_stage {
                    return self.accept_at_limit();
                }
                *used += 1;
                let remaining = self.limits.revisions_per_stage - *used;
                // Reworked artifacts must be measured again.
                self.gates.clear();
                self.set_gate(CRITIC_GATE, false, notes)?;
                Ok(ReviewOutcome::Revise { remaining })
            }
            Verdict::Kill => {
                if self.kills >= self.limits.kills {
                    return self.accept_at_limit();
                }
                self.return_to_scout(notes, Vec::new());
                Ok(ReviewOutcome::Killed {
                    remaining: self.limits.kills - self.kills,
                })
            }
        }
    }

    /// Abandons the current concept on the director's own initiative.
    pub fn kill(&mut self, reason: impl Into<String>, locations: Vec<String>) -> Result<u32> {
        if !self.stage.can_kill() {
            bail!(
                "stage {} cannot be abandoned for a new scouting round",
                self.stage.as_str()
            );
        }
        if self.kills >= self.limits.kills {
            bail!(
                "the production already used all {} kills",
                self.limits.kills
            );
        }
        self.return_to_scout(reason.into(), locations);
        Ok(self.limits.kills - self.kills)
    }

    /// Decides what happens when a mandate assignment cannot be realised,
    /// and applies the part of that decision that concerns the stage.
    ///
    /// A redraw sends the production back to the brief, because the brief
    /// committed to the value that is being replaced. The caller updates the
    /// mandate itself according to the returned outcome.
    pub fn mandate_infeasible(
        &mut self,
        reason: impl Into<String>,
        locations: Vec<String>,
    ) -> InfeasibleOutcome {
        let reason = reason.into();
        let outcome = if self.mandate_redraws < self.limits.mandate_redraws {
            self.mandate_redraws += 1;
            if self.stage > Stage::Brief {
                self.history.push(Event::Reopened {
                    from: self.stage,
                    to: Stage::Brief,
                    reason: reason.clone(),
                });
                self.stage = Stage::Brief;
            }
            self.gates.clear();
            InfeasibleOutcome::Redrawn
        } else if self.kills < self.limits.kills && self.stage.can_kill() {
            self.return_to_scout(reason.clone(), locations);
            InfeasibleOutcome::Killed
        } else {
            self.compromised = true;
            self.gates.clear();
            InfeasibleOutcome::Waived
        };
        self.history.push(Event::Infeasible { reason, outcome });
        outcome
    }

    /// Adds locations that the abandoned concept used to the ban list.
    pub fn ban_locations(&mut self, locations: impl IntoIterator<Item = String>) {
        for location in locations {
            if !self.banned_locations.contains(&location) {
                self.banned_locations.push(location);
            }
        }
    }

    fn return_to_scout(&mut self, reason: String, locations: Vec<String>) {
        self.history.push(Event::Killed {
            from: self.stage,
            reason,
        });
        self.kills += 1;
        self.ban_locations(locations);
        self.stage = Stage::Scout;
        self.gates.clear();
        self.revisions.clear();
    }

    fn accept_at_limit(&mut self) -> Result<ReviewOutcome> {
        self.compromised = true;
        self.history.push(Event::LimitReached { stage: self.stage });
        self.set_gate(
            CRITIC_GATE,
            true,
            "accepted because the revision or kill limit was reached".to_owned(),
        )?;
        Ok(ReviewOutcome::LimitReached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_measured(state: &mut State) {
        for gate in state.stage.gates() {
            if *gate != CRITIC_GATE {
                state.record_gate(gate, true, "ok").unwrap();
            }
        }
    }

    /// Passes every gate of the current stage and advances.
    fn clear_stage(state: &mut State) -> Stage {
        pass_measured(state);
        if state.stage.has_critic() {
            assert_eq!(
                state.review(Verdict::Pass, "").unwrap(),
                ReviewOutcome::Passed
            );
        }
        state.advance().unwrap()
    }

    fn at(stage: Stage) -> State {
        let mut state = State::new("film");
        while state.stage != stage {
            clear_stage(&mut state);
        }
        state
    }

    #[test]
    fn stages_form_one_chain_ending_at_done() {
        let mut stage = Stage::Scout;
        let mut visited = vec![stage];
        while let Some(next) = stage.next() {
            visited.push(next);
            stage = next;
        }
        assert_eq!(visited, Stage::ALL);
        assert!(Stage::Done.gates().is_empty());
    }

    #[test]
    fn advance_is_refused_until_every_gate_passes() {
        let mut state = State::new("film");
        let error = state.advance().unwrap_err().to_string();
        assert!(
            error.contains("locations-selected, novelty, cost-ceiling"),
            "{error}"
        );

        state.record_gate("locations-selected", true, "3").unwrap();
        state.record_gate("novelty", false, "too close").unwrap();
        state.record_gate("cost-ceiling", true, "ok").unwrap();
        assert_eq!(state.unmet_gates(), vec!["novelty"]);
        assert!(state.advance().is_err());

        state.record_gate("novelty", true, "ok").unwrap();
        assert_eq!(state.advance().unwrap(), Stage::Brief);
        assert!(state.gates.is_empty(), "gates must not carry over");
        // The results that justified leaving the stage stay on record.
        let Some(Event::Advanced { gates, .. }) = state.history.last() else {
            panic!("advance must be recorded");
        };
        assert_eq!(gates["novelty"].detail, "ok");
        assert_eq!(gates.len(), 3);
    }

    #[test]
    fn advance_if_ready_waits_for_the_last_gate() {
        let mut state = State::new("film");
        state.record_gate("locations-selected", true, "").unwrap();
        state.record_gate("novelty", true, "").unwrap();
        assert_eq!(state.advance_if_ready().unwrap(), None);
        assert_eq!(state.stage, Stage::Scout);

        state.record_gate("cost-ceiling", true, "").unwrap();
        assert_eq!(state.advance_if_ready().unwrap(), Some(Stage::Brief));
        // The new stage has unjudged gates, so it does not cascade.
        assert_eq!(state.advance_if_ready().unwrap(), None);

        state.record_gate("schema", true, "").unwrap();
        state.reset_gates();
        assert_eq!(state.unmet_gates(), Stage::Brief.gates());
    }

    #[test]
    fn gates_of_other_stages_are_rejected() {
        let mut state = State::new("film");
        assert!(state.record_gate("budget", true, "").is_err());
        assert!(state.record_gate("unknown", true, "").is_err());
    }

    #[test]
    fn critic_gate_cannot_be_recorded_directly() {
        let mut state = at(Stage::Shotlist);
        assert!(state.record_gate(CRITIC_GATE, true, "").is_err());
    }

    #[test]
    fn review_requires_a_critic_stage_and_measured_gates() {
        let mut state = State::new("film");
        assert!(state.review(Verdict::Pass, "").is_err());

        let mut state = at(Stage::Shotlist);
        let error = state.review(Verdict::Pass, "").unwrap_err().to_string();
        assert!(error.contains("route-safety"), "{error}");
    }

    #[test]
    fn full_run_reaches_done() {
        let mut state = State::new("film");
        while state.stage != Stage::Done {
            clear_stage(&mut state);
        }
        assert!(state.advance().is_err());
        assert!(!state.compromised);
        state.validate().unwrap();
    }

    #[test]
    fn revise_resets_measurements_and_counts_down() {
        let mut state = at(Stage::Shotlist);
        pass_measured(&mut state);
        assert_eq!(
            state
                .review(Verdict::Revise, "cut 2 is unreadable")
                .unwrap(),
            ReviewOutcome::Revise { remaining: 2 }
        );
        assert_eq!(state.unmet_gates(), Stage::Shotlist.gates());
        assert!(state.advance().is_err());
        assert!(state.review(Verdict::Pass, "").is_err());
    }

    #[test]
    fn revision_limit_accepts_the_stage_as_compromised() {
        let mut state = at(Stage::Look);
        for remaining in (0..Limits::default().revisions_per_stage).rev() {
            pass_measured(&mut state);
            assert_eq!(
                state.review(Verdict::Revise, "").unwrap(),
                ReviewOutcome::Revise { remaining }
            );
        }
        pass_measured(&mut state);
        assert_eq!(
            state.review(Verdict::Revise, "").unwrap(),
            ReviewOutcome::LimitReached
        );
        assert!(state.compromised);
        assert_eq!(state.advance().unwrap(), Stage::Proof);
    }

    #[test]
    fn revision_counts_are_per_stage() {
        let mut state = at(Stage::Shotlist);
        pass_measured(&mut state);
        state.review(Verdict::Revise, "").unwrap();
        clear_stage(&mut state);
        assert_eq!(state.stage, Stage::Look);
        pass_measured(&mut state);
        assert_eq!(
            state.review(Verdict::Revise, "").unwrap(),
            ReviewOutcome::Revise {
                remaining: Limits::default().revisions_per_stage - 1
            }
        );
    }

    #[test]
    fn critic_kill_returns_to_scout_and_resets_revisions() {
        let mut state = at(Stage::Proof);
        pass_measured(&mut state);
        state.review(Verdict::Revise, "").unwrap();
        pass_measured(&mut state);
        assert_eq!(
            state
                .review(Verdict::Kill, "variation of an earlier film")
                .unwrap(),
            ReviewOutcome::Killed { remaining: 1 }
        );
        assert_eq!(state.stage, Stage::Scout);
        assert!(state.gates.is_empty());
        assert!(state.revisions.is_empty());
        assert_eq!(state.kills, 1);
    }

    #[test]
    fn kill_limit_accepts_the_stage_as_compromised() {
        let mut state = State::new("film");
        for _ in 0..Limits::default().kills {
            while state.stage != Stage::Shotlist {
                clear_stage(&mut state);
            }
            pass_measured(&mut state);
            assert!(matches!(
                state.review(Verdict::Kill, "").unwrap(),
                ReviewOutcome::Killed { .. }
            ));
        }
        while state.stage != Stage::Shotlist {
            clear_stage(&mut state);
        }
        pass_measured(&mut state);
        assert_eq!(
            state.review(Verdict::Kill, "").unwrap(),
            ReviewOutcome::LimitReached
        );
        assert!(state.compromised);
        assert_eq!(state.stage, Stage::Shotlist);
        assert_eq!(state.advance().unwrap(), Stage::Look);
    }

    #[test]
    fn director_kill_bans_locations_and_is_limited() {
        let mut state = State::new("film");
        assert!(state.kill("too early", Vec::new()).is_err());

        let mut state = at(Stage::Brief);
        assert_eq!(
            state
                .kill("weak subject", vec!["loc-1".into(), "loc-2".into()])
                .unwrap(),
            1
        );
        assert_eq!(state.stage, Stage::Scout);
        assert_eq!(state.banned_locations, vec!["loc-1", "loc-2"]);

        clear_stage(&mut state);
        state
            .kill("again", vec!["loc-2".into(), "loc-3".into()])
            .unwrap();
        assert_eq!(state.banned_locations, vec!["loc-1", "loc-2", "loc-3"]);

        clear_stage(&mut state);
        assert!(state.kill("third", Vec::new()).is_err());
        assert_eq!(state.stage, Stage::Brief);
    }

    #[test]
    fn infeasible_mandates_are_redrawn_then_killed_then_waived() {
        let mut state = at(Stage::Shotlist);
        pass_measured(&mut state);

        // Redraws reopen the brief, which committed to the failed value.
        for used in 1..=Limits::default().mandate_redraws {
            assert_eq!(
                state.mandate_infeasible("no flyover surface", vec!["loc-1".into()]),
                InfeasibleOutcome::Redrawn
            );
            assert_eq!(state.stage, Stage::Brief);
            assert_eq!(state.mandate_redraws, used);
            assert!(state.gates.is_empty());
            assert!(state.banned_locations.is_empty());
        }

        // With redraws exhausted the concept is dropped like a kill.
        for used in 1..=Limits::default().kills {
            assert_eq!(
                state.mandate_infeasible("still none", vec![format!("loc-{used}")]),
                InfeasibleOutcome::Killed
            );
            assert_eq!(state.stage, Stage::Scout);
            assert_eq!(state.kills, used);
            clear_stage(&mut state);
        }
        assert_eq!(state.banned_locations, vec!["loc-1", "loc-2"]);

        // With kills exhausted too, the assignment is waived.
        assert_eq!(state.stage, Stage::Brief);
        assert_eq!(
            state.mandate_infeasible("again", Vec::new()),
            InfeasibleOutcome::Waived
        );
        assert_eq!(state.stage, Stage::Brief);
        assert!(state.compromised);
        assert!(matches!(
            state.history.last(),
            Some(Event::Infeasible {
                outcome: InfeasibleOutcome::Waived,
                ..
            })
        ));
    }

    #[test]
    fn an_infeasible_family_during_scouting_is_never_a_kill() {
        let mut state = State::new("film");
        state.mandate_redraws = Limits::default().mandate_redraws;
        // Scouting has no concept to drop, so the assignment is waived.
        assert_eq!(
            state.mandate_infeasible("no usable shape", Vec::new()),
            InfeasibleOutcome::Waived
        );
        assert_eq!(state.stage, Stage::Scout);
        assert_eq!(state.kills, 0);
    }

    #[test]
    fn limits_come_from_the_production_not_from_constants() {
        let strict = Limits {
            revisions_per_stage: 1,
            kills: 0,
            mandate_redraws: 0,
        };
        let mut state = State::with_limits("film", strict);
        while state.stage != Stage::Shotlist {
            clear_stage(&mut state);
        }

        pass_measured(&mut state);
        assert_eq!(
            state.review(Verdict::Revise, "").unwrap(),
            ReviewOutcome::Revise { remaining: 0 }
        );
        pass_measured(&mut state);
        assert_eq!(
            state.review(Verdict::Revise, "").unwrap(),
            ReviewOutcome::LimitReached
        );

        // No kills and no redraws: an unrealisable mandate is waived at once.
        let mut state = State::with_limits("film", strict);
        clear_stage(&mut state);
        assert!(state.kill("no", Vec::new()).is_err());
        assert_eq!(
            state.mandate_infeasible("none", Vec::new()),
            InfeasibleOutcome::Waived
        );

        // The limits survive a save and load.
        let restored: State =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(restored.limits, strict);
    }

    #[test]
    fn production_cannot_be_killed() {
        let mut state = at(Stage::Production);
        assert!(state.kill("late", Vec::new()).is_err());
        assert!(state.review(Verdict::Kill, "").is_err());
    }

    #[test]
    fn state_round_trips_and_validates() {
        let mut state = at(Stage::Shotlist);
        pass_measured(&mut state);
        state.review(Verdict::Revise, "note").unwrap();
        let json = serde_json::to_string(&state).unwrap();
        let restored: State = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, state);
        restored.validate().unwrap();

        let mut corrupt = restored;
        corrupt.gates.insert(
            "qc".to_owned(),
            GateRecord {
                passed: true,
                detail: String::new(),
            },
        );
        assert!(corrupt.validate().is_err());
    }
}
