use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, Phase, SessionCommand};

use crate::flows::submit_workshop_command;
use crate::helpers::*;
use crate::state::{IdentityState, OperationState};

use super::archive_panel::ArchivePanel;

#[component]
pub fn EndView(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let results_status = end_results_status_copy(state);
    let vote_rows = end_vote_result_rows(state);
    let score_rows = end_player_score_rows(state);
    let judge_rows = judge_feedback_rows(state);
    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);
    let is_judge_screen = state.phase == Phase::Judge;
    let header_title = if is_judge_screen {
        "Judge review"
    } else {
        "Workshop results"
    };
    let header_meta = if is_judge_screen {
        "Mechanics scoring is ready before the anonymous design vote.".to_string()
    } else {
        results_status
    };
    let header_status = if is_judge_screen { "Judge" } else { "Final" };

    let commands_disabled = {
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some()
    };

    drop(gs);

    rsx! {
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", {header_title} }
                p { class: "roster__meta", {header_meta} }
            }
            span { class: "roster__status roster__status--phase status-connected", {header_status} }
        }
        if !score_rows.is_empty() {
            p { class: "meta", if is_judge_screen { "Mechanics leaderboard" } else { "Mechanics leaderboard" } }
            div { class: "roster",
                for row in score_rows {
                    article { class: "roster__item roster__item--feedback",
                        div {
                            p { class: "roster__name", {row.player_name.clone()} }
                            p { class: "roster__meta", {row.placement_label.clone()} " - " {row.achievements_label.clone()} }
                            p { class: "roster__meta roster__meta--feedback", {row.judge_feedback_label.clone()} }
                        }
                        span {
                            class: format!("roster__status {}", if row.is_winner { "status-connected" } else { "status-connecting" }),
                            {row.score_label.clone()}
                        }
                    }
                }
            }
        }
        if !judge_rows.is_empty() {
            p { class: "meta", "Judge feedback by dragon" }
            div { class: "roster" }
            div { class: "judge-feedback-grid",
                for row in judge_rows {
                    article { class: "judge-feedback-card",
                        p { class: "roster__name", {row.dragon_name.clone()} }
                        p { class: "roster__meta", {row.observation_score_label.clone()} " - " {row.care_score_label.clone()} }
                        p { class: "judge-feedback-card__body", {row.feedback.clone()} }
                    }
                }
            }
        }
        if !is_judge_screen && !vote_rows.is_empty() {
            p { class: "meta", "Creativity Leaderboard" }
            div { class: "roster",
                for row in vote_rows {
                    article { class: "roster__item",
                        div {
                            p { class: "roster__name", {row.dragon_name.clone()} }
                            p { class: "roster__meta", {row.placement_label.clone()} " - Created by " {row.creator_name.clone()} }
                        }
                        span { class: "roster__status status-connected", {row.votes_label.clone()} }
                    }
                }
            }
        }
        // ---- Host controls ----
        if is_host {
            div { class: "button-row",
                if is_judge_screen {
                    button {
                        class: "button button--primary",
                        "data-testid": "start-voting-button",
                        disabled: commands_disabled,
                        onclick: move |_| {
                            spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::StartVoting, None));
                        },
                        "Open design voting"
                    }
                }
                button {
                    class: "button button--secondary",
                    "data-testid": "reset-game-button",
                    disabled: commands_disabled,
                    onclick: move |_| {
                        spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::ResetGame, None));
                    },
                    "Reset workshop"
                }
            }
        } else {
            p {
                class: "meta",
                if is_judge_screen {
                    "Waiting for the host to open anonymous design voting."
                } else {
                    "Waiting for the host to reset or archive this workshop."
                }
            }
        }
        article { class: "scoring-method",
            p { class: "scoring-method__title", "How scores work" }
            div { class: "scoring-method__body",
                p { class: "scoring-method__section", "The judge evaluates each dragon on two axes (0-100 each):" }
                ul { class: "panel__list",
                    li { strong { "Observation score" } " — awarded to the Phase 1 creator. Measures quality of discovery notes and handover tags: did they identify the dragon's real food, play, and sleep preferences?" }
                    li { strong { "Care score" } " — awarded to the Phase 2 caretaker. Measures how well they followed handover instructions, chose correct actions, and kept stats healthy." }
                }
                p { class: "scoring-method__section", "Final score = observation score + care score (max 200)." }
                p { class: "scoring-method__section", "The judge also considers:" }
                ul { class: "panel__list",
                    li { "Correct action ratio — feeding the right food, playing the right game for the time of day." }
                    li { "Wrong action count — wrong food/play choices escalate happiness penalties (20/25/30/35)." }
                    li { "Cooldown discipline — spamming actions during cooldown counts against the care score." }
                    li { "Stat health at finish — hunger, energy, and happiness levels at the end of Phase 2." }
                    li { "Lowest happiness reached — recovering from a crash shows adaptability." }
                }
                p { class: "scoring-method__section", "Phase 2 decay is 3x stronger than Phase 1. Penalty stacks from wrong actions increase happiness drain." }
            }
        }
        // ---- Achievement reference ----
        article { class: "scoring-method",
            p { class: "scoring-method__title", "Achievements" }
            div { class: "scoring-method__body",
                ul { class: "panel__list",
                    li { strong { "master_chef" } " — first food attempt was correct." }
                    li { strong { "playful_spirit" } " — first play attempt was correct." }
                    li { strong { "speed_learner" } " — found both correct food and play within the first 3 actions." }
                    li { strong { "no_mistakes" } " — zero wrong food or play choices (5+ actions required)." }
                    li { strong { "zen_master" } " — ended Phase 2 with zero penalty stacks (8+ actions)." }
                    li { strong { "perfectionist" } " — 80%+ correct action ratio (10+ actions)." }
                    li { strong { "steady_hand" } " — held happiness above 60 for 20+ consecutive ticks in Phase 2." }
                    li { strong { "comeback_kid" } " — recovered from happiness at or below 15 to finish at or above 70." }
                    li { strong { "helicopter_parent" } " — performed 20+ total actions." }
                    li { strong { "button_masher" } " — triggered 5+ cooldown violations." }
                    li { strong { "chaos_gremlin" } " — reached 4+ penalty stacks at some point." }
                    li { strong { "rock_bottom" } " — happiness dropped to zero." }
                }
            }
        }
        // ---- Workshop archive (End phase only) ----
        if !is_judge_screen {
            ArchivePanel { game_state, judge_bundle }
        }
    }
}
