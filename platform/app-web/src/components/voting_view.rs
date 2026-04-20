use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, SessionCommand};

use crate::flows::submit_workshop_command;
use crate::helpers::*;
use crate::state::{IdentityState, OperationState};

#[component]
pub fn VotingView(
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

    let commands_disabled = {
        let id = identity.read();
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some() || id.session_snapshot.is_none()
    };

    let progress = voting_progress_label(state);
    let status = voting_status_copy(state);
    let rows = voting_option_rows(state);
    let reveal_enabled = voting_reveal_ready(state);
    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);

    // Drop read guard before rsx closures that capture mutable signals
    drop(gs);

    rsx! {
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", "Vote for the most creative dragon design" }
                p { class: "roster__meta", {progress} }
            }
            span { class: "roster__status roster__status--phase status-connected", "Design vote" }
        }
        p { class: "panel__body", {status} }
        div { class: "voting-grid",
            for row in rows {
                article {
                    class: format!(
                        "voting-card{}{}",
                        if row.is_selected { " voting-card--selected" } else { "" },
                        if row.is_current_players_dragon { " voting-card--blocked" } else { "" },
                    ),
                    // ---- Dragon sprite ----
                    div { class: "voting-card__sprite",
                        if let Some(ref sprites) = row.custom_sprites {
                            img {
                                class: "voting-card__sprite-img",
                                src: "data:image/png;base64,{sprites.neutral}",
                                alt: "Dragon sprite",
                            }
                        } else {
                            // Fallback: CSS pixel dragon
                            // Body (primary color)
                            div {
                                class: "sprite-pixel sprite-body",
                                style: format!("background: {};", row.color_primary),
                            }
                            // Head (secondary color)
                            div {
                                class: "sprite-pixel sprite-head",
                                style: format!("background: {};", row.color_secondary),
                            }
                            // Eye (accent color)
                            div {
                                class: "sprite-pixel sprite-eye",
                                style: format!("background: {};", row.color_accent),
                            }
                            // Wing (secondary color, shifted)
                            div {
                                class: "sprite-pixel sprite-wing",
                                style: format!("background: {};", row.color_secondary),
                            }
                            // Tail (primary color, extended)
                            div {
                                class: "sprite-pixel sprite-tail",
                                style: format!("background: {};", row.color_primary),
                            }
                            // Horn / crest (accent)
                            div {
                                class: "sprite-pixel sprite-horn",
                                style: format!("background: {};", row.color_accent),
                            }
                            // Legs (secondary)
                            div {
                                class: "sprite-pixel sprite-legs",
                                style: format!("background: {};", row.color_secondary),
                            }
                        }
                    }
                    // ---- Label ----
                    p { class: "voting-card__name", {row.dragon_name.clone()} }
                    // ---- Action ----
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
                                    spawn(submit_workshop_command(
                                        identity,
                                        ops,
                                        handover_tags_input,
                                        judge_bundle,
                                        SessionCommand::SubmitVote,
                                        Some(serde_json::json!({ "dragonId": vote_target.clone() })),
                                    ));
                                }
                            },
                            "Vote"
                        }
                    }
                }
            }
        }
        if is_host {
            p {
                class: "meta",
                "The host can finish voting after at least one eligible vote is submitted, even if some players skip it."
            }
            div { class: "button-row",
                button {
                    class: "button button--primary",
                    "data-testid": "reveal-results-button",
                    disabled: commands_disabled || !reveal_enabled,
                    onclick: move |_| {
                        spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::RevealVotingResults, None));
                    },
                    "Finish voting"
                }
                button {
                    class: "button button--danger",
                    "data-testid": "end-session-button",
                    disabled: commands_disabled,
                    onclick: move |_| {
                        spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, SessionCommand::EndSession, None));
                    },
                    "End game"
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
            p { class: "meta", "Waiting for the host to finish voting and reveal the final standings." }
        }
    }
}
