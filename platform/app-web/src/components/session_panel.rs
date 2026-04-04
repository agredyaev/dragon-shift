use dioxus::prelude::*;
use protocol::ClientGameState;
use protocol::JudgeBundle;
use protocol::Phase;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

use crate::helpers::*;
use crate::realtime::bootstrap_realtime;
use crate::state::{IdentityState, OperationState, apply_realtime_bootstrap_error};

use super::end_view::EndView;
use super::handover_view::HandoverView;
use super::lobby_view::LobbyView;
use super::phase1_view::Phase1View;
use super::phase2_view::Phase2View;
use super::voting_view::VotingView;

#[component]
pub fn SessionPanel(
    identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    ops: Signal<OperationState>,
    handover_tags_input: Signal<String>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let now_tick = use_signal(current_time_seconds);

    #[cfg(target_arch = "wasm32")]
    {
        use_effect(move || {
            let mut now_tick = now_tick;
            if let Some(window) = web_sys::window() {
                let callback = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
                    now_tick.set(current_time_seconds());
                })
                    as Box<dyn FnMut()>);
                let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
                    callback.as_ref().unchecked_ref(),
                    1000,
                );
                callback.forget();
            }
        });
    }

    // Summary chip data — cheap string computations
    let session_code_label = {
        let id = identity.read();
        id.session_snapshot
            .as_ref()
            .map(|s| s.session_code.clone())
            .unwrap_or_else(|| "\u{2014}".to_string())
    };
    let (
        active_player_label,
        session_phase_label,
        players_count_label,
        session_phase_title,
        session_phase_body,
        countdown_label,
        current_phase,
    ) = {
        let gs = game_state.read();
        let now = *now_tick.read();
        let active = gs
            .as_ref()
            .and_then(active_player_name)
            .unwrap_or_else(|| "Not attached yet".to_string());
        let phase_label = gs
            .as_ref()
            .map(|s| format!("{:?}", s.phase))
            .unwrap_or_else(|| "Not connected".to_string());
        let count = gs
            .as_ref()
            .map(|s| s.players.len().to_string())
            .unwrap_or_else(|| "0".to_string());
        let title = gs
            .as_ref()
            .map(|s| phase_screen_title(s.phase))
            .unwrap_or("Awaiting session");
        let body = gs
            .as_ref()
            .map(|s| phase_screen_body(s.phase))
            .unwrap_or("Connect to a workshop to see the active gameplay screen.");
        let countdown = gs
            .as_ref()
            .and_then(|s| phase_remaining_seconds(s, now))
            .map(format_remaining_duration);
        let phase = gs.as_ref().map(|s| s.phase);
        (active, phase_label, count, title, body, countdown, phase)
    };

    // Realtime button state
    let (has_snapshot, realtime_button_label, pending_flow) = {
        let id = identity.read();
        let o = ops.read();
        let has = id.session_snapshot.is_some();
        let label = if id.realtime_bootstrap_attempted {
            "Reconnect session"
        } else {
            "Sync session"
        };
        let pending = o.pending_flow.is_some();
        (has, label, pending)
    };

    let mut rt_identity = identity;
    let mut rt_ops = ops;

    rsx! {
        article { class: "panel panel--session", "data-testid": "session-panel",
            h2 { class: "panel__title", "Shift board" }
            div { class: "session-summary",
                p { class: "summary-chip", "Workshop code: " {session_code_label} }
                p { class: "summary-chip", "Current caretaker: " {active_player_label} }
                p { class: "summary-chip", "Current round: " {session_phase_label} }
                p { class: "summary-chip", "Players in view: " {players_count_label} }
                if let Some(countdown_label) = countdown_label {
                    p { class: "summary-chip", "Time left: " {countdown_label} }
                }
            }
            h2 { class: "panel__title", "Current round" }
            h3 { class: "panel__title", {session_phase_title} }
            p { class: "panel__body", {session_phase_body} }

            match current_phase {
                Some(Phase::Lobby) => rsx! {
                    LobbyView { game_state: game_state }
                },
                Some(Phase::Phase1) => rsx! {
                    Phase1View { game_state: game_state }
                },
                Some(Phase::Handover) => rsx! {
                    HandoverView { game_state: game_state, handover_tags_input: handover_tags_input }
                },
                Some(Phase::Phase2) => rsx! {
                    Phase2View { game_state: game_state }
                },
                Some(Phase::Voting) => rsx! {
                    VotingView {
                        identity: identity, game_state: game_state, ops: ops,
                        handover_tags_input: handover_tags_input, judge_bundle: judge_bundle,
                    }
                },
                Some(Phase::End) => rsx! {
                    EndView { game_state: game_state }
                },
                None => rsx! {},
            }

            // Realtime sync button
            div { class: "button-row",
                button {
                    class: "button button--secondary",
                    "data-testid": "sync-session-button",
                    disabled: !has_snapshot || pending_flow,
                    onclick: move |_| {
                        if let Err(error) = bootstrap_realtime(identity, game_state, ops, judge_bundle) {
                            rt_identity.with_mut(|id| {
                                rt_ops.with_mut(|o| {
                                    apply_realtime_bootstrap_error(id, o, error);
                                });
                            });
                        }
                    },
                    {realtime_button_label}
                }
            }
            p { class: "meta", "This browser remembers your last workshop so you can reconnect without retyping everything." }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn current_time_seconds() -> i64 {
    (js_sys::Date::now() / 1000.0).floor() as i64
}

#[cfg(not(target_arch = "wasm32"))]
fn current_time_seconds() -> i64 {
    0
}
