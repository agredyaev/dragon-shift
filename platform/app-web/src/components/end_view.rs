use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, Phase, SessionCommand};

use crate::flows::submit_workshop_command;
use crate::helpers::*;
use crate::state::{clear_session_identity, IdentityState, OperationState};

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
    let game_over_rows = game_over_player_rows(state);
    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);
    let is_judge_screen = state.phase == Phase::Judge;
    let is_end_screen = state.phase == Phase::End;
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

    let mut show_game_over = use_signal(|| true);

    // Game Over overlay — shown on End phase until dismissed
    if is_end_screen && *show_game_over.read() {
        return rsx! {
            div { class: "game-over", "data-testid": "game-over-overlay",
                h1 { class: "game-over__title", "Game Over!" }
                p { class: "game-over__subtitle", "High Scores" }
                div { class: "game-over__list",
                    for row in game_over_rows {
                        div {
                            class: format!(
                                "game-over__player{}",
                                if row.is_winner { " game-over__player--winner" } else { "" },
                            ),
                            div { class: "game-over__player-header",
                                span {
                                    class: format!(
                                        "game-over__name{}",
                                        if row.is_winner { " game-over__name--winner" } else { "" },
                                    ),
                                    if row.is_winner {
                                        span { class: "game-over__crown", {poke_icon_url("crown")} }
                                    }
                                    "{row.placement_label}. {row.player_name}"
                                }
                                span {
                                    class: format!(
                                        "game-over__score{}",
                                        if row.is_winner { " game-over__score--winner" } else { "" },
                                    ),
                                    {row.score_label.clone()}
                                }
                            }
                            if !row.achievement_badges.is_empty() {
                                div { class: "game-over__achievements",
                                    for (name, icon) in row.achievement_badges {
                                        span { class: "game-over__badge",
                                            img {
                                                class: "game-over__badge-icon",
                                                src: poke_icon_url(icon),
                                                alt: "{name}",
                                            }
                                            "{name}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "button-row",
                    button {
                        class: "button button--primary",
                        "data-testid": "game-over-continue-button",
                        onclick: move |_| {
                            show_game_over.set(false);
                        },
                        "Continue"
                    }
                }
            }
        };
    }

    rsx! {
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", {header_title} }
                p { class: "roster__meta", {header_meta} }
            }
            span { class: "roster__status roster__status--phase status-connected", {header_status} }
        }
        // ---- Mechanics leaderboard ----
        if !score_rows.is_empty() {
            p { class: "meta", "Mechanics leaderboard" }
            div { class: "leaderboard",
                div { class: "leaderboard__header",
                    span { class: "leaderboard__col leaderboard__col--rank", "#" }
                    span { class: "leaderboard__col leaderboard__col--name", "Player" }
                    span { class: "leaderboard__col leaderboard__col--score", "Phase 1" }
                    span { class: "leaderboard__col leaderboard__col--score", "Phase 2" }
                    span { class: "leaderboard__col leaderboard__col--total", "Total" }
                    span { class: "leaderboard__col leaderboard__col--status", "Status" }
                }
                for row in score_rows {
                    div {
                        class: format!("leaderboard__row{}", if row.is_winner { " leaderboard__row--winner" } else { "" }),
                        span { class: "leaderboard__col leaderboard__col--rank", {row.placement_label.clone()} }
                        span { class: "leaderboard__col leaderboard__col--name", {row.player_name.clone()} }
                        span { class: "leaderboard__col leaderboard__col--score", {row.phase1_score_label.clone()} }
                        span { class: "leaderboard__col leaderboard__col--score", {row.phase2_score_label.clone()} }
                        span { class: "leaderboard__col leaderboard__col--total", {row.total_score_label.clone()} }
                        if row.judge_status == "Good" {
                            span { class: "leaderboard__col leaderboard__col--status leaderboard__status--good", "Good" }
                        } else {
                            span {
                                class: "leaderboard__col leaderboard__col--status leaderboard__status--bad leaderboard__tooltip-anchor",
                                "Bad"
                                span { class: "leaderboard__tooltip", {row.judge_status_tooltip.clone()} }
                            }
                        }
                    }
                }
            }
        }
        // ---- Creativity leaderboard ----
        if !is_judge_screen && !vote_rows.is_empty() {
            p { class: "meta", "Creativity leaderboard" }
            div { class: "leaderboard leaderboard--creativity",
                div { class: "leaderboard__header",
                    span { class: "leaderboard__col leaderboard__col--rank", "#" }
                    span { class: "leaderboard__col leaderboard__col--name", "Dragon" }
                    span { class: "leaderboard__col leaderboard__col--name", "Creator" }
                    span { class: "leaderboard__col leaderboard__col--total", "Votes" }
                }
                for row in vote_rows {
                    div { class: "leaderboard__row",
                        span { class: "leaderboard__col leaderboard__col--rank", {row.placement_label.clone()} }
                        span { class: "leaderboard__col leaderboard__col--name", {row.dragon_name.clone()} }
                        span { class: "leaderboard__col leaderboard__col--name", {row.creator_name.clone()} }
                        span { class: "leaderboard__col leaderboard__col--total", {row.votes_label.clone()} }
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
                if is_end_screen {
                    button {
                        class: "button button--secondary",
                        "data-testid": "leave-workshop-button",
                        onclick: move |_| {
                            clear_session_identity(&mut identity.write());
                        },
                        "Leave workshop"
                    }
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
            if is_end_screen {
                div { class: "button-row",
                    button {
                        class: "button button--secondary",
                        "data-testid": "leave-workshop-button",
                        onclick: move |_| {
                            clear_session_identity(&mut identity.write());
                        },
                        "Leave workshop"
                    }
                }
            }
        }
        // ---- Workshop archive (End phase only) ----
        if !is_judge_screen {
            ArchivePanel { game_state, judge_bundle }
        }
    }
}
