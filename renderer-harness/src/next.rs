//! The single entry point a director needs. `next` reports where the
//! production stands and what may be done about it, so the stage order never
//! has to live in a prompt.

use serde::Serialize;

use crate::{
    axes::{Axes, AxisId},
    budget::Budget,
    ledger::Ledger,
    mandate::Mandate,
    stage::{Stage, State},
    tuning::Tuning,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateStatus {
    Pending,
    Passed,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct GateReport {
    pub gate: &'static str,
    pub status: GateStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommandReport {
    pub command: &'static str,
    pub purpose: &'static str,
    /// False while the command belongs to a milestone that is not built yet.
    pub implemented: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Limits {
    pub revisions_used: u32,
    pub revisions_allowed: u32,
    /// Revisions of the whole concept, over all design stages.
    pub concept_revisions_used: u32,
    pub concept_revisions_allowed: u32,
    /// Revisions this stage may still spend, by either limit.
    pub revisions_remaining: u32,
    pub kills_used: u32,
    pub kills_allowed: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct BudgetReport {
    pub production_limit_gpu_seconds: f64,
    pub design_limit_gpu_seconds: f64,
    pub design_remaining_gpu_seconds: f64,
    pub max_frame_seconds: f64,
    pub max_gpu_duty_cycle: f64,
}

/// Axis values of a recent film. This is all a director learns about past
/// work.
#[derive(Clone, Debug, Serialize)]
pub struct PastFilm {
    pub id: String,
    pub axes: Axes,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiversityReport {
    pub mandate: Mandate,
    pub recent_films: Vec<PastFilm>,
    pub rule: String,
    pub axes: Vec<AxisId>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NextReport {
    pub production: String,
    pub stage: Stage,
    pub stage_code: &'static str,
    pub complete: bool,
    pub instruction: &'static str,
    pub gates: Vec<GateReport>,
    pub commands: Vec<CommandReport>,
    /// Files the director writes during this stage.
    pub write: Vec<&'static str>,
    /// Harness outputs worth reading during this stage.
    pub read: Vec<&'static str>,
    pub limits: Limits,
    pub compromised: bool,
    pub banned_locations: Vec<String>,
    pub budget: BudgetReport,
    pub diversity: DiversityReport,
}

struct StageGuide {
    instruction: &'static str,
    commands: &'static [(&'static str, &'static str, bool)],
    write: &'static [&'static str],
    read: &'static [&'static str],
}

const KILL: (&str, &str, bool) = (
    "kill",
    "企画を破棄してロケハンへ戻る。使ったロケ地は再選択できなくなる",
    true,
);
const REOPEN: (&str, &str, bool) = (
    "reopen",
    "shotlist か look の工程へ戻って作り直す。いまの工程の revise を1回消費する",
    true,
);
const REVIEW: (&str, &str, bool) = (
    "review submit",
    "批評役の判定 (pass / revise / kill) を登録する",
    true,
);

const fn guide(stage: Stage) -> StageGuide {
    match stage {
        Stage::Scout => StageGuide {
            instruction: "ロケハンを実行し、コンタクトシートの画像を見てロケ地を2〜5個選ぶ。形状の数値は書かない。",
            commands: &[
                (
                    "scout run",
                    "形状と視点をサンプリングし、選別を通った候補のコンタクトシートを作る。--around <loc> で既存候補の近傍を探す",
                    true,
                ),
                (
                    "scout select",
                    "ロケ地を2〜5個選び、選択数とコスト上限のゲートを判定する",
                    true,
                ),
            ],
            write: &[],
            read: &["scout/"],
        },
        Stage::Brief => StageGuide {
            instruction: "brief.yaml を書く。主題を一文で述べ、アークを場面（beats）の列に分け、各場面にロケ地・視点・運動を割り当てる。brief check → storyboard → 批評の順に通す。語彙で表せない場面は request で記録し、表せる形に書き直す。",
            commands: &[
                ("schema", "brief.yaml の注釈つき見本を返す", true),
                (
                    "brief check",
                    "スキーマ、指定制約、軸差分、各場面が撮れるかを検証する",
                    true,
                ),
                ("storyboard", "各場面の始点と終点を描いたシートを作る", true),
                REVIEW,
                (
                    "request",
                    "語彙に足りない表現（運動、形状、マテリアル、光）を記録する。制作はいまの語彙で続ける",
                    true,
                ),
                KILL,
            ],
            write: &["brief.yaml"],
            read: &["scout/", "storyboard/", "reviews/", "mandate.json"],
        },
        Stage::Shotlist => StageGuide {
            instruction: "shotlist.yaml を書く。ロケ地と視点は ID で指定する。アニマティックを作り、批評役に渡す。",
            commands: &[
                (
                    "shotlist check",
                    "スキーマ、経路の安全性、作品内の重複禁止を検証する",
                    true,
                ),
                ("animatic", "全尺の低解像度動画と読みやすさ指標を作る", true),
                REVIEW,
                KILL,
            ],
            write: &["shotlist.yaml"],
            read: &["animatic/", "reviews/"],
        },
        Stage::Look => StageGuide {
            instruction: "look.yaml を書く。品質設定は書かない。効果は effects_requested で要求する。",
            commands: &[
                ("look preview", "ルック確認画像と指標を作る", true),
                REVIEW,
                REOPEN,
                KILL,
            ],
            write: &["look.yaml"],
            read: &["look/", "reviews/"],
        },
        Stage::Proof => StageGuide {
            instruction: "プルーフを実行する。品質設定はハーネスが決める。ゲートを通らない場合は reopen で shotlist か look へ戻り、カットやルックを直す。",
            commands: &[
                (
                    "proof",
                    "効果の有無比較、品質決定、時間見積もり、プルーフ動画、圧縮耐性",
                    true,
                ),
                REVIEW,
                REOPEN,
                KILL,
            ],
            write: &[],
            read: &["proof/", "reviews/"],
        },
        Stage::Production => StageGuide {
            instruction: "設計は完了した。本番レンダリングとエンコードは起動スクリプトが自動で行うので、ここで作業を終えて報告する。",
            commands: &[(
                "status",
                "本番レンダリングの進捗と残り時間の見積もりを返す",
                true,
            )],
            write: &[],
            read: &[],
        },
        Stage::Encode => StageGuide {
            instruction: "本番レンダリングは完了した。エンコードは起動スクリプトが自動で行うので、ここで作業を終えて報告する。",
            commands: &[("status", "本番レンダリングの結果を返す", true)],
            write: &[],
            read: &[],
        },
        Stage::Done => StageGuide {
            instruction: "制作は完了している。成果物は final/ にある。",
            commands: &[],
            write: &[],
            read: &["final/"],
        },
    }
}

/// How much going back the production has left.
#[must_use]
pub fn limits(state: &State) -> Limits {
    Limits {
        revisions_used: state.revisions.get(&state.stage).copied().unwrap_or(0),
        revisions_allowed: state.limits.revisions_per_stage,
        concept_revisions_used: state.concept_revisions(),
        concept_revisions_allowed: state.limits.revisions_per_concept,
        revisions_remaining: state.revisions_remaining(),
        kills_used: state.kills,
        kills_allowed: state.limits.kills,
    }
}

/// Builds the report for the production's current stage.
#[must_use]
pub fn report(
    state: &State,
    mandate: &Mandate,
    budget: &Budget,
    ledger: &Ledger,
    tuning: &Tuning,
) -> NextReport {
    let guide = guide(state.stage);

    let gates = state
        .stage
        .gates()
        .iter()
        .map(|gate| match state.gates.get(*gate) {
            None => GateReport {
                gate,
                status: GateStatus::Pending,
                detail: String::new(),
            },
            Some(record) => GateReport {
                gate,
                status: if record.passed {
                    GateStatus::Passed
                } else {
                    GateStatus::Failed
                },
                detail: record.detail.clone(),
            },
        })
        .collect();

    let commands = guide
        .commands
        .iter()
        // A kill that would be refused is not offered.
        .filter(|(command, ..)| *command != "kill" || state.kills < state.limits.kills)
        .map(|(command, purpose, implemented)| CommandReport {
            command,
            purpose,
            implemented: *implemented,
        })
        .collect();

    NextReport {
        production: state.id.clone(),
        stage: state.stage,
        stage_code: state.stage.code(),
        complete: state.stage == Stage::Done,
        instruction: guide.instruction,
        gates,
        commands,
        write: guide.write.to_vec(),
        read: guide.read.to_vec(),
        limits: limits(state),
        compromised: state.compromised,
        banned_locations: state.banned_locations.clone(),
        budget: BudgetReport {
            production_limit_gpu_seconds: budget.production_limit_seconds(),
            design_limit_gpu_seconds: budget.design_limit_seconds(),
            design_remaining_gpu_seconds: budget.design_remaining_seconds(),
            max_frame_seconds: budget.max_frame_seconds,
            max_gpu_duty_cycle: budget.max_gpu_duty_cycle,
        },
        diversity: DiversityReport {
            mandate: mandate.clone(),
            recent_films: ledger
                .recent(tuning.diversity.axis_diff_window)
                .map(|entry| PastFilm {
                    id: entry.id.clone(),
                    axes: entry.axes,
                })
                .collect(),
            rule: format!(
                "直近{}作品のそれぞれと、6軸中{}軸以上が異なること",
                tuning.diversity.axis_diff_window, tuning.diversity.min_differing_axes
            ),
            axes: AxisId::ALL.to_vec(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        axes::{FormulaFamily, Motion},
        ledger::tests::entry,
        production::AUTHORED_FILES,
        stage::{CRITIC_GATE, Verdict},
    };

    fn fixtures() -> (Mandate, Budget, Ledger) {
        let ledger = Ledger::from_entries(vec![entry(
            "earlier",
            FormulaFamily::Mandelbox,
            Motion::Dive,
        )])
        .unwrap();
        (
            Mandate::draw(&ledger, 3, &Tuning::default().diversity),
            Budget::default(),
            ledger,
        )
    }

    fn at(stage: Stage) -> State {
        let mut state = State::new("film");
        while state.stage != stage {
            pass_measured(&mut state);
            if state.stage.has_critic() {
                state.review(Verdict::Pass, "").unwrap();
            }
            state.advance().unwrap();
        }
        state
    }

    fn pass_measured(state: &mut State) {
        for gate in state.stage.gates() {
            if *gate != CRITIC_GATE {
                state.record_gate(gate, true, "ok").unwrap();
            }
        }
    }

    #[test]
    fn every_stage_lists_its_gates_and_valid_files() {
        let (mandate, budget, ledger) = fixtures();
        for stage in Stage::ALL {
            let report = report(&at(*stage), &mandate, &budget, &ledger, &Tuning::default());
            let gates: Vec<_> = report.gates.iter().map(|gate| gate.gate).collect();
            assert_eq!(gates, stage.gates());
            assert!(
                report
                    .write
                    .iter()
                    .all(|file| AUTHORED_FILES.contains(file)),
                "{stage:?} asks the director to write a harness-owned file"
            );
            assert_eq!(report.complete, *stage == Stage::Done);
            assert!(!report.instruction.is_empty());
            assert_eq!(report.commands.is_empty(), *stage == Stage::Done);
        }
    }

    #[test]
    fn review_and_kill_follow_the_stage_machine() {
        let (mandate, budget, ledger) = fixtures();
        for stage in Stage::ALL {
            let report = report(&at(*stage), &mandate, &budget, &ledger, &Tuning::default());
            let offers = |name: &str| report.commands.iter().any(|entry| entry.command == name);
            assert_eq!(offers("review submit"), stage.has_critic(), "{stage:?}");
            assert_eq!(offers("kill"), stage.can_kill(), "{stage:?}");
        }
    }

    #[test]
    fn gate_statuses_reflect_recorded_results() {
        let (mandate, budget, ledger) = fixtures();
        let mut state = State::new("film");
        state
            .record_gate("novelty", false, "too close to earlier")
            .unwrap();
        state.record_gate("cost-ceiling", true, "ok").unwrap();
        let report = report(&state, &mandate, &budget, &ledger, &Tuning::default());
        let status = |name: &str| {
            report
                .gates
                .iter()
                .find(|gate| gate.gate == name)
                .map(|gate| (gate.status, gate.detail.as_str()))
                .unwrap()
        };
        assert_eq!(status("locations-selected"), (GateStatus::Pending, ""));
        assert_eq!(
            status("novelty"),
            (GateStatus::Failed, "too close to earlier")
        );
        assert_eq!(status("cost-ceiling"), (GateStatus::Passed, "ok"));
    }

    #[test]
    fn exhausted_kills_are_not_offered() {
        let (mandate, budget, ledger) = fixtures();
        let mut state = at(Stage::Brief);
        state.kills = state.limits.kills;
        let report = report(&state, &mandate, &budget, &ledger, &Tuning::default());
        assert!(report.commands.iter().all(|entry| entry.command != "kill"));
        assert_eq!(report.limits.kills_used, state.limits.kills);
    }

    #[test]
    fn past_films_expose_axes_only() {
        let (mandate, budget, ledger) = fixtures();
        let report = report(
            &State::new("film"),
            &mandate,
            &budget,
            &ledger,
            &Tuning::default(),
        );
        let json = serde_json::to_value(&report).unwrap();
        let film = &json["diversity"]["recent_films"][0];
        let keys: Vec<_> = film.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["axes", "id"]);
        assert_eq!(
            json["diversity"]["mandate"]["banned_families"][0],
            "mandelbox"
        );
    }
}
