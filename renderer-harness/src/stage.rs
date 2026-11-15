//! Stage machine of one production. The order of stages and the decision to
//! leave a stage belong to this module, never to an agent.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub const STATE_VERSION: u32 = 1;
/// Critic `revise` verdicts accepted per stage before the harness moves on.
pub const MAX_REVISIONS_PER_STAGE: u32 = 3;
/// Times a production may be sent back to scouting.
pub const MAX_KILLS: u32 = 2;

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
    Advanced { from: Stage, to: Stage },
    Reviewed { stage: Stage, verdict: Verdict },
    Killed { from: Stage, reason: String },
    LimitReached { stage: Stage },
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
    /// Set when a revision or kill limit forced the harness to move on.
    pub compromised: bool,
    pub history: Vec<Event>,
}

impl State {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            version: STATE_VERSION,
            id: id.into(),
            stage: Stage::Scout,
            gates: BTreeMap::new(),
            revisions: BTreeMap::new(),
            kills: 0,
            banned_locations: Vec::new(),
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
        });
        self.stage = next;
        self.gates.clear();
        Ok(next)
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
                if *used >= MAX_REVISIONS_PER_STAGE {
                    return self.accept_at_limit();
                }
                *used += 1;
                let remaining = MAX_REVISIONS_PER_STAGE - *used;
                // Reworked artifacts must be measured again.
                self.gates.clear();
                self.set_gate(CRITIC_GATE, false, notes)?;
                Ok(ReviewOutcome::Revise { remaining })
            }
            Verdict::Kill => {
                if self.kills >= MAX_KILLS {
                    return self.accept_at_limit();
                }
                self.return_to_scout(notes, Vec::new());
                Ok(ReviewOutcome::Killed {
                    remaining: MAX_KILLS - self.kills,
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
        if self.kills >= MAX_KILLS {
            bail!("the production already used all {MAX_KILLS} kills");
        }
        self.return_to_scout(reason.into(), locations);
        Ok(MAX_KILLS - self.kills)
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
        for remaining in (0..MAX_REVISIONS_PER_STAGE).rev() {
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
                remaining: MAX_REVISIONS_PER_STAGE - 1
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
        for _ in 0..MAX_KILLS {
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
