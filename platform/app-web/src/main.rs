mod api;
mod components;
mod flows;
mod helpers;
mod realtime;
mod state;

use dioxus::prelude::*;

use components::account_home::AccountHomeView;
use components::app_bar::AppBar;
use components::create_character::CreateCharacterView;
use components::end_view::EndView;
use components::handover_view::HandoverView;
use components::lobby_view::LobbyView;
use components::notice::NoticeBar;
use components::phase1_view::Phase1View;
use components::phase2_view::Phase2View;
use components::pick_character::PickCharacterView;
use components::sign_in::SignInView;

use helpers::poke_icon_url;
use protocol::Phase;
use realtime::bootstrap_realtime;
use state::{NoticeScope, ShellScreen, apply_realtime_bootstrap_error, bootstrap_state};

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    dioxus_web::launch::launch_cfg(App, dioxus_web::Config::default());
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    launch(App);
}

#[component]
fn App() -> Element {
    let bootstrap = use_hook(bootstrap_state);

    let identity = use_signal(|| bootstrap.identity);
    let game_state = use_signal(|| bootstrap.game_state);
    let reconnect_session_code = use_signal(|| bootstrap.reconnect_session_code);
    let reconnect_token = use_signal(|| bootstrap.reconnect_token);
    let handover_tags_input = use_signal(|| bootstrap.handover_tags_input);
    let ops = use_signal(|| bootstrap.ops);
    let judge_bundle = use_signal(|| bootstrap.judge_bundle);
    let should_bootstrap_realtime = {
        let id = identity.read();
        id.session_snapshot.is_some() && !id.realtime_bootstrap_attempted
    };

    let render_session_panels_first = {
        let id = identity.read();
        id.screen == ShellScreen::Session
    };

    // Pre-session screen variant (only read when not in Session).
    let pre_session_screen = if !render_session_panels_first {
        Some(identity.read().screen.clone())
    } else {
        None
    };

    // ---- Phase detection & day/night cycle (single read) ----
    let (current_phase, is_daytime, game_time, is_clock_phase) = {
        let gs = game_state.read();
        match gs.as_ref() {
            Some(s) => {
                let t = s.time.rem_euclid(24);
                (
                    Some(s.phase),
                    (6..18).contains(&t),
                    s.time,
                    matches!(s.phase, Phase::Phase1 | Phase::Phase2),
                )
            }
            None => (None, false, 0, false),
        }
    };

    let is_lobby = render_session_panels_first && current_phase == Some(Phase::Lobby);
    let is_phase1 = render_session_panels_first && current_phase == Some(Phase::Phase1);
    let is_phase2 = render_session_panels_first && current_phase == Some(Phase::Phase2);
    let is_handover = render_session_panels_first && current_phase == Some(Phase::Handover);
    let is_voting = render_session_panels_first && current_phase == Some(Phase::Voting);
    let is_judge = render_session_panels_first && current_phase == Some(Phase::Judge);
    let is_end = render_session_panels_first && current_phase == Some(Phase::End);
    let is_session_bootstrapping = render_session_panels_first && current_phase.is_none();
    let show_clock = render_session_panels_first && is_clock_phase;

    let time_string = format!("{:02}:00", game_time.rem_euclid(24));
    let clock_icon = if is_daytime { "sun" } else { "moon" };
    let clock_icon_url = poke_icon_url(clock_icon);

    let is_create_character_screen = matches!(
        pre_session_screen.as_ref(),
        Some(ShellScreen::CreateCharacter)
    );
    let is_centered_card_screen = matches!(pre_session_screen.as_ref(), Some(ShellScreen::SignIn));
    let is_pick_character_screen = matches!(
        pre_session_screen.as_ref(),
        Some(ShellScreen::PickCharacter { .. })
    );

    // ---- Container class ----
    let container_class = if is_phase1 || is_phase2 {
        "shell__container shell__container--phase1"
    } else if is_handover {
        "shell__container shell__container--handover"
    } else if is_voting {
        "shell__container shell__container--voting"
    } else if is_judge || is_end {
        "shell__container shell__container--end"
    } else if is_create_character_screen {
        "shell__container shell__container--phase0"
    } else if is_pick_character_screen {
        "shell__container shell__container--pick-character"
    } else if is_centered_card_screen {
        "shell__container shell__container--card"
    } else {
        "shell__container"
    };

    // Day/night background only applies during clock phases (Phase1/Phase2).
    // Home screen (no game_state) and other phases use the neutral shell background.
    let shell_class = if render_session_panels_first && is_clock_phase {
        if is_daytime {
            "shell shell--day"
        } else {
            "shell shell--night"
        }
    } else if is_create_character_screen {
        "shell shell--pre-session shell--create-character"
    } else if pre_session_screen.is_some() {
        "shell shell--pre-session"
    } else {
        "shell"
    };

    let mut effect_identity = identity;
    let mut effect_ops = ops;
    let mut bootstrap_back_identity = identity;
    let mut bootstrap_back_ops = ops;

    use_effect(move || {
        if should_bootstrap_realtime
            && let Err(error) = bootstrap_realtime(identity, game_state, ops, judge_bundle)
        {
            effect_identity.with_mut(|id| {
                effect_ops.with_mut(|o| {
                    apply_realtime_bootstrap_error(id, o, error);
                });
            });
        }
    });

    rsx! {
        AppBar { identity, ops }
        main { class: shell_class,
            // Clock HUD (Phase1/Phase2 only)
            if show_clock {
                div { class: "clock-hud",
                    img {
                        class: "pixel-icon",
                        src: "{clock_icon_url}",
                        alt: "{clock_icon}",
                        width: 32, height: 32,
                    }
                    span { class: "clock-hud__time", {time_string} }
                }
            }
            section { class: container_class,
                if is_lobby
                    || is_phase1
                    || is_handover
                    || is_phase2
                    || is_voting
                    || is_judge
                    || is_end
                    || is_session_bootstrapping
                {
                    NoticeBar {
                        ops,
                        scope: NoticeScope::Session,
                    }
                } else {
                    match pre_session_screen.as_ref() {
                        Some(ShellScreen::AccountHome) => rsx! { NoticeBar { ops, scope: NoticeScope::AccountHome } },
                        Some(ShellScreen::CreateCharacter) => rsx! { NoticeBar { ops, scope: NoticeScope::CreateCharacter } },
                        Some(ShellScreen::PickCharacter { .. }) => rsx! { NoticeBar { ops, scope: NoticeScope::PickCharacter } },
                        _ => rsx! { NoticeBar { ops, scope: NoticeScope::SignIn } },
                    }
                }
                if is_lobby {
                    LobbyView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_phase1 {
                    Phase1View {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_handover {
                    HandoverView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_phase2 {
                    Phase2View {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_voting {
                    EndView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_judge {
                    EndView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_end {
                    EndView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_session_bootstrapping {
                    article { class: "panel", "data-testid": "session-bootstrap-panel",
                        h1 { class: "panel__title", "Reconnecting to workshop" }
                        p { class: "panel__body", "Syncing your workshop session..." }
                        div { class: "button-row",
                            button {
                                class: "button button--secondary",
                                "data-testid": "bootstrap-back-button",
                                onclick: move |_| {
                                    bootstrap_back_identity.with_mut(|id| {
                                        bootstrap_back_ops.with_mut(|o| {
                                            state::clear_session_identity(id);
                                            o.pending_flow = None;
                                            o.pending_command = None;
                                            o.pending_judge_bundle = false;
                                        });
                                    });
                                },
                                "Back to home"
                            }
                        }
                    }
                } else {
                    // ---- Pre-session screens (SignIn / AccountHome / etc.) ----
                    match pre_session_screen.as_ref() {
                        Some(ShellScreen::AccountHome) => rsx! {
                            AccountHomeView {
                                identity,
                                ops,
                            }
                        },
                        Some(ShellScreen::CreateCharacter) => rsx! {
                            CreateCharacterView { identity, ops }
                        },
                        Some(ShellScreen::PickCharacter { workshop_code }) => rsx! {
                            PickCharacterView {
                                key: "{workshop_code}",
                                identity,
                                game_state,
                                ops,
                                reconnect_session_code,
                                reconnect_token,
                                judge_bundle,
                                workshop_code: workshop_code.clone(),
                            }
                        },
                        _ => rsx! {
                            SignInView { identity, ops }
                        },
                    }
                }
            }
        }
    }
}
