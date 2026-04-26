use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, SessionCommand};

use crate::flows::{leave_workshop, start_judge_bundle_request, start_workshop_command};
use crate::helpers::*;
use crate::state::{ConnectionStatus, IdentityState, OperationState};

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
    let scoring_status = scoring_status_copy(state);
    let vote_rows = end_vote_result_rows(state);
    let score_rows = end_player_score_rows(state);
    let voting_rows = voting_option_rows(state);
    let game_over_rows = game_over_player_rows(state);
    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);
    let is_end_screen = matches!(state.phase, protocol::Phase::End);
    let is_voting_screen = matches!(
        state.phase,
        protocol::Phase::Voting | protocol::Phase::Judge
    );
    let reveal_enabled = voting_reveal_ready(state);
    let results_revealed = voting_results_revealed(state);
    let voting_progress = voting_progress_label(state);
    let session_code = state.session.code.clone();
    let header_title = if is_end_screen {
        "Game over"
    } else {
        "Scoring"
    };
    let header_meta = scoring_status.clone();
    let header_status = if is_end_screen { "Final" } else { "Scoring" };

    let commands_disabled = {
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some() || o.pending_judge_bundle
    };
    let scores_ready = !score_rows.is_empty();
    let archive_built = judge_bundle.read().is_some();
    let archive_disabled = commands_disabled
        || archive_built
        || !scores_ready
        || (is_voting_screen && !results_revealed);
    let archive_label = if archive_built {
        "Archive ready"
    } else {
        "Archive workshop"
    };
    let connection_status = identity.read().connection_status;
    let connection_label = match connection_status {
        ConnectionStatus::Offline => "Offline",
        ConnectionStatus::Connecting => "Connecting",
        ConnectionStatus::Connected => "Connected",
    };
    let connection_class = match connection_status {
        ConnectionStatus::Offline => "status-offline",
        ConnectionStatus::Connecting => "status-connecting",
        ConnectionStatus::Connected => "status-connected",
    };

    drop(gs);

    let mut show_game_over = use_signal(|| true);
    let mut active_tab = use_signal(|| {
        if is_end_screen || results_revealed {
            "design".to_string()
        } else {
            "vote".to_string()
        }
    });
    let active_tab_value = active_tab.read().clone();
    let active_tab_key = if is_end_screen || (results_revealed && active_tab_value == "vote") {
        "design"
    } else {
        active_tab_value.as_str()
    };

    // Game Over overlay — shown on End phase until dismissed
    if is_end_screen && !game_over_rows.is_empty() && *show_game_over.read() {
        return rsx! {
            div { class: "sr-only", "data-testid": "workshop-code-badge", {session_code.clone()} }
            div {
                class: format!("sr-only {}", connection_class),
                "data-testid": "connection-badge",
                {connection_label}
            }
            div { class: "sr-only", "data-testid": "controls-panel", if is_host { "visible" } else { "hidden" } }
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
                                        img {
                                            class: "game-over__crown game-over__crown-icon",
                                            src: poke_icon_url("crown"),
                                            alt: "Winner crown",
                                        }
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
        div { class: "sr-only", "data-testid": "workshop-code-badge", {session_code} }
        div {
            class: format!("sr-only {}", connection_class),
            "data-testid": "connection-badge",
            {connection_label}
        }
        div { class: "sr-only", "data-testid": "controls-panel", if is_host { "visible" } else { "hidden" } }
        div { "data-testid": "session-panel",
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", {header_title} }
                p { class: "roster__meta", {header_meta} }
            }
            span { class: "roster__status roster__status--phase status-connected", {header_status} }
        }
        if is_voting_screen || is_host {
            div { class: "button-row button-row--session-controls",
                if !results_revealed {
                    button {
                        class: if active_tab_key == "vote" { "button button--primary" } else { "button button--secondary" },
                        disabled: commands_disabled,
                        onclick: move |_| active_tab.set("vote".to_string()),
                        "Vote for design"
                    }
                }
                button {
                    class: if active_tab_key == "score" { "button button--primary" } else { "button button--secondary" },
                    disabled: commands_disabled,
                    onclick: move |_| active_tab.set("score".to_string()),
                    "View score"
                }
                if results_revealed {
                    button {
                        class: if active_tab_key == "design" { "button button--primary" } else { "button button--secondary" },
                        disabled: commands_disabled,
                        onclick: move |_| active_tab.set("design".to_string()),
                        "Design results"
                    }
                }
                if is_host {
                    if is_voting_screen && !results_revealed {
                        button {
                            class: "button button--primary",
                            "data-testid": "reveal-results-button",
                            disabled: commands_disabled || !reveal_enabled,
                            onclick: move |_| {
                                if start_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::RevealVotingResults, None) {
                                    active_tab.set("design".to_string());
                                }
                            },
                            "Finish voting"
                        }
                    }
                    if is_voting_screen {
                        button {
                            class: "button button--danger",
                            "data-testid": "end-session-button",
                            disabled: commands_disabled || !results_revealed,
                            onclick: move |_| {
                                let _ = start_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::EndSession, None);
                            },
                            "End game"
                        }
                    }
                    if is_voting_screen || is_end_screen {
                        button {
                            class: if archive_built { "button button--primary" } else { "button button--secondary" },
                            "data-testid": "archive-workshop-button",
                            disabled: archive_disabled,
                            onclick: move |_| {
                                let _ = start_judge_bundle_request(identity, game_state, ops, judge_bundle);
                            },
                            {archive_label}
                        }
                    }
                    if is_end_screen {
                        button {
                            class: "button button--secondary",
                            "data-testid": "leave-workshop-button",
                            onclick: move |_| {
                                leave_workshop(identity, ops);
                            },
                            "Leave workshop"
                        }
                    }
                }
            }
        }

        if !is_end_screen && !results_revealed && active_tab_key == "vote" {
            article { class: "roster__item roster__item--phase",
                div {
                    p { class: "roster__name", "Vote for the most creative dragon design" }
                    p { class: "roster__meta", {voting_progress} }
                }
                span { class: "roster__status roster__status--phase status-connected", "Design vote" }
            }
            div { class: "voting-grid",
                for row in voting_rows {
                    article {
                        class: format!(
                            "voting-card{}{}",
                            if row.is_selected { " voting-card--selected" } else { "" },
                            if row.is_current_players_dragon { " voting-card--blocked" } else { "" },
                        ),
                        div { class: "voting-card__sprite-stack",
                            if let Some(ref sprites) = row.custom_sprites {
                                div { class: "voting-card__emotion-row",
                                    img { class: "voting-card__sprite-img", src: "data:image/png;base64,{sprites.neutral}", alt: "Neutral sprite" }
                                    img { class: "voting-card__sprite-img", src: "data:image/png;base64,{sprites.happy}", alt: "Happy sprite" }
                                }
                                div { class: "voting-card__emotion-row",
                                    img { class: "voting-card__sprite-img", src: "data:image/png;base64,{sprites.angry}", alt: "Angry sprite" }
                                    img { class: "voting-card__sprite-img", src: "data:image/png;base64,{sprites.sleepy}", alt: "Sleepy sprite" }
                                }
                            } else {
                                div { class: "voting-card__sprite voting-card__sprite--fallback",
                                    div { class: "sprite-pixel sprite-body", style: format!("background: {};", row.color_primary) }
                                    div { class: "sprite-pixel sprite-head", style: format!("background: {};", row.color_secondary) }
                                    div { class: "sprite-pixel sprite-eye", style: format!("background: {};", row.color_accent) }
                                    div { class: "sprite-pixel sprite-wing", style: format!("background: {};", row.color_secondary) }
                                    div { class: "sprite-pixel sprite-tail", style: format!("background: {};", row.color_primary) }
                                    div { class: "sprite-pixel sprite-horn", style: format!("background: {};", row.color_accent) }
                                    div { class: "sprite-pixel sprite-legs", style: format!("background: {};", row.color_secondary) }
                                }
                            }
                        }
                        div { class: "voting-card__summary",
                            p { class: "voting-card__name", {row.dragon_name.clone()} }
                            if row.is_current_players_dragon {
                                span { class: "voting-card__badge status-offline", "Your dragon" }
                            } else if row.is_selected {
                                span { class: "voting-card__badge status-connected", "Voted" }
                            } else {
                                button {
                                    class: "button button--secondary voting-card__button",
                                    "data-testid": format!("vote-button-{}", row.dragon_id),
                                    disabled: commands_disabled,
                                    onclick: {
                                        let vote_target = row.dragon_id.clone();
                                        move |_| {
                                            let _ = start_workshop_command(
                                                identity,
                                                ops,
                                                handover_tags_input,
                                                judge_bundle,
                                                SessionCommand::SubmitVote,
                                                Some(serde_json::json!({ "dragonId": vote_target.clone() })),
                                            );
                                        }
                                    },
                                    "Vote"
                                }
                            }
                        }
                    }
                }
            }
        }

        if active_tab_key == "score" {
            if !score_rows.is_empty() {
                p { class: "meta", "Score leaderboard" }
                div { class: "leaderboard",
                    div { class: "leaderboard__header",
                        span { class: "leaderboard__col leaderboard__col--rank", "#" }
                        span { class: "leaderboard__col leaderboard__col--name", "Player" }
                        span { class: "leaderboard__col leaderboard__col--score", "Phase 1" }
                        span { class: "leaderboard__col leaderboard__col--score", "Phase 2" }
                        span { class: "leaderboard__col leaderboard__col--total", "Total" }
                        span { class: "leaderboard__col leaderboard__col--status", "Judge" }
                    }
                    for row in score_rows {
                        div {
                            class: format!("leaderboard__row{}", if row.is_winner { " leaderboard__row--winner" } else { "" }),
                            span { class: "leaderboard__col leaderboard__col--rank", {row.placement_label.clone()} }
                            span { class: "leaderboard__col leaderboard__col--name", {row.player_name.clone()} }
                            span { class: "leaderboard__col leaderboard__col--score", {row.phase1_score_label.clone()} }
                            span { class: "leaderboard__col leaderboard__col--score", {row.phase2_score_label.clone()} }
                            span { class: "leaderboard__col leaderboard__col--total", {row.total_score_label.clone()} }
                            span {
                                class: format!(
                                    "leaderboard__col leaderboard__col--status leaderboard__tooltip-anchor {}",
                                    if row.judge_status == "Good" {
                                        "leaderboard__status--good"
                                    } else {
                                        "leaderboard__status--bad"
                                    },
                                ),
                                {row.judge_status}
                                if !row.judge_status_tooltip.is_empty() {
                                    span { class: "leaderboard__tooltip", {row.judge_status_tooltip.clone()} }
                                }
                            }
                        }
                    }
                }
            } else {
                p { class: "meta", "Judge scores are still syncing." }
            }
        }

        if active_tab_key == "design" || is_end_screen {
            if !vote_rows.is_empty() {
                p { class: "meta", {results_status.clone()} }
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
        }
        // ---- Participant waiting / leave controls ----
        if !is_host {
            p {
                class: "meta",
                if is_end_screen {
                    "Waiting for the host to archive this workshop."
                } else if results_revealed {
                    "Waiting for the host to open the final game over screen."
                } else {
                    "Waiting for the host to finish voting."
                }
            }
            if is_end_screen {
                div { class: "button-row",
                    button {
                        class: "button button--secondary",
                        "data-testid": "leave-workshop-button",
                        onclick: move |_| {
                            leave_workshop(identity, ops);
                        },
                        "Leave workshop"
                    }
                }
            }
        }
        // ---- Workshop archive ----
        if is_end_screen || archive_built {
            ArchivePanel { game_state, judge_bundle }
        }
        }
    }
}
