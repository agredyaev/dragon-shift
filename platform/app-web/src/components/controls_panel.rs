use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, Phase, SessionCommand};

use crate::flows::{submit_judge_bundle_request, submit_workshop_command};
use crate::helpers::{current_player, voting_reveal_ready};
use crate::state::{IdentityState, OperationState};

fn command_test_id(command: SessionCommand) -> &'static str {
    match command {
        SessionCommand::StartHandover => "start-handover-button",
        SessionCommand::EndGame => "end-game-button",
        SessionCommand::StartVoting => "start-voting-button",
        SessionCommand::RevealVotingResults => "reveal-results-button",
        SessionCommand::ResetGame => "reset-workshop-button",
        _ => "session-command-button",
    }
}

#[component]
pub fn ControlsPanel(
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

    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);
    if !is_host {
        return rsx! {};
    }

    let current_phase = state.phase;
    let commands_disabled = {
        let id = identity.read();
        let o = ops.read();
        o.pending_flow.is_some() || o.pending_command.is_some() || id.session_snapshot.is_none()
    };
    let judge_bundle_disabled = commands_disabled || ops.read().pending_judge_bundle;
    let pending_judge_bundle = ops.read().pending_judge_bundle;
    let reveal_ready = voting_reveal_ready(state);

    let (title, body, primary_command, primary_label, secondary_command, secondary_label) =
        match current_phase {
            Phase::Phase1 => (
                "Host controls",
                "Move the group into handover when discovery time is done.",
                Some(SessionCommand::StartHandover),
                Some("Start handover"),
                Some(SessionCommand::ResetGame),
                Some("Reset workshop"),
            ),
            Phase::Phase2 => (
                "Host controls",
                "Run the judge once care is complete, then review the mechanics leaderboard.",
                Some(SessionCommand::EndGame),
                Some("Run judge review"),
                Some(SessionCommand::ResetGame),
                Some("Reset workshop"),
            ),
            Phase::Judge => (
                "Host controls",
                "Open anonymous design voting after the group reviews the judge feedback.",
                Some(SessionCommand::StartVoting),
                Some("Open design voting"),
                Some(SessionCommand::ResetGame),
                Some("Reset workshop"),
            ),
            Phase::Voting => (
                "Host controls",
                "Reveal the final standings after every eligible player has voted.",
                Some(SessionCommand::RevealVotingResults),
                Some("Reveal final standings"),
                Some(SessionCommand::ResetGame),
                Some("Reset workshop"),
            ),
            Phase::End => (
                "Host controls",
                "Archive the workshop or reset for another round.",
                Some(SessionCommand::ResetGame),
                Some("Reset workshop"),
                None,
                None,
            ),
            _ => return rsx! {},
        };

    drop(gs);

    rsx! {
        article { class: "panel panel--controls", "data-testid": "controls-panel",
            h2 { class: "panel__title", {title} }
            p { class: "panel__body", {body} }
            div { class: "panel__stack",
                div { class: "button-row",
                    if let (Some(command), Some(label)) = (primary_command, primary_label) {
                        button {
                            class: "button button--primary",
                            "data-testid": command_test_id(command),
                            disabled: commands_disabled
                                || (current_phase == Phase::Voting
                                    && command == SessionCommand::RevealVotingResults
                                    && !reveal_ready),
                            onclick: move |_| {
                                spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, command, None));
                            },
                            "{label}"
                        }
                    }
                    if let (Some(command), Some(label)) = (secondary_command, secondary_label) {
                        button {
                            class: "button button--secondary",
                            "data-testid": command_test_id(command),
                            disabled: commands_disabled,
                            onclick: move |_| {
                                spawn(submit_workshop_command(identity, ops, handover_tags_input, judge_bundle, command, None));
                            },
                            "{label}"
                        }
                    }
                }
                if current_phase == Phase::End {
                    div { class: "button-row",
                        button {
                            class: "button button--secondary",
                            "data-testid": "build-archive-button",
                            disabled: judge_bundle_disabled,
                            onclick: move |_| {
                                spawn(submit_judge_bundle_request(identity, game_state, ops, judge_bundle));
                            },
                            if pending_judge_bundle {
                                "Building archive..."
                            } else {
                                "Build archive"
                            }
                        }
                    }
                }
            }
        }
    }
}
