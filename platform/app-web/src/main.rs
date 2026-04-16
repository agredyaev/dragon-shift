mod api;
mod components;
mod flows;
mod helpers;
mod realtime;
mod state;

use dioxus::prelude::*;

use components::create_panel::CreatePanel;
use components::end_view::EndView;
use components::handover_view::HandoverView;
use components::hero::Hero;
use components::join_panel::JoinPanel;
use components::lobby_view::LobbyView;
use components::notice::NoticeBar;
use components::phase0_view::Phase0View;
use components::phase1_view::Phase1View;
use components::phase2_view::Phase2View;
use components::voting_view::VotingView;
use components::workshop_brief::WorkshopBrief;

use helpers::poke_icon_url;
use protocol::Phase;
use realtime::bootstrap_realtime;
use state::{ShellScreen, apply_realtime_bootstrap_error, bootstrap_state};

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
    let create_name = use_signal(|| bootstrap.create_name);
    let phase0_minutes = use_signal(|| "8".to_string());
    let phase1_minutes = use_signal(|| "8".to_string());
    let phase2_minutes = use_signal(|| "8".to_string());
    let join_session_code = use_signal(|| bootstrap.join_session_code);
    let join_name = use_signal(|| bootstrap.join_name);
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

    // ---- Phase detection & day/night cycle (single read) ----
    let (current_phase, is_daytime, game_time, is_clock_phase) = {
        let gs = game_state.read();
        match gs.as_ref() {
            Some(s) => {
                let t = s.time.rem_euclid(24);
                (
                    Some(s.phase),
                    t >= 6 && t < 18,
                    s.time,
                    matches!(s.phase, Phase::Phase1 | Phase::Phase2),
                )
            }
            None => (None, false, 0, false),
        }
    };

    let is_lobby = render_session_panels_first && current_phase == Some(Phase::Lobby);
    let is_phase0 = render_session_panels_first && current_phase == Some(Phase::Phase0);
    let is_phase1 = render_session_panels_first && current_phase == Some(Phase::Phase1);
    let is_phase2 = render_session_panels_first && current_phase == Some(Phase::Phase2);
    let is_handover = render_session_panels_first && current_phase == Some(Phase::Handover);
    let is_voting = render_session_panels_first && current_phase == Some(Phase::Voting);
    let is_judge = render_session_panels_first && current_phase == Some(Phase::Judge);
    let is_end = render_session_panels_first && current_phase == Some(Phase::End);
    let show_clock = render_session_panels_first && is_clock_phase;

    let time_string = format!("{:02}:00", game_time.rem_euclid(24));
    let clock_icon = if is_daytime { "sun" } else { "moon" };
    let clock_icon_url = poke_icon_url(clock_icon);

    // ---- Container class ----
    let container_class = if is_phase0 {
        "shell__container shell__container--phase0"
    } else if is_phase1 || is_phase2 {
        "shell__container shell__container--phase1"
    } else if is_handover {
        "shell__container shell__container--handover"
    } else if is_voting {
        "shell__container shell__container--voting"
    } else if is_judge || is_end {
        "shell__container shell__container--end"
    } else {
        "shell__container"
    };

    // Day/night background only applies during clock phases (Phase1/Phase2).
    // Home screen (no game_state) and other phases use the neutral shell background.
    let shell_class = if is_clock_phase {
        if is_daytime {
            "shell shell--day"
        } else {
            "shell shell--night"
        }
    } else {
        "shell"
    };

    let mut effect_identity = identity;
    let mut effect_ops = ops;

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
                if is_phase0 {
                    Phase0View {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_phase1 {
                    NoticeBar { ops }
                    Phase1View {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_handover {
                    NoticeBar { ops }
                    HandoverView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_phase2 {
                    NoticeBar { ops }
                    Phase2View {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_voting {
                    NoticeBar { ops }
                    VotingView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_judge {
                    NoticeBar { ops }
                    EndView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else if is_end {
                    NoticeBar { ops }
                    EndView {
                        identity,
                        game_state,
                        ops,
                        handover_tags_input,
                        judge_bundle,
                    }
                } else {
                    // ---- Home screen ----
                    Hero { identity, game_state }
                    NoticeBar { ops }
                    section { class: "grid",
                        WorkshopBrief {}
                        if is_lobby {
                            LobbyView {
                                identity,
                                game_state,
                                ops,
                                handover_tags_input,
                                judge_bundle,
                            }
                        } else {
                            CreatePanel {
                                identity,
                                game_state,
                                ops,
                                create_name,
                                phase0_minutes,
                                phase1_minutes,
                                phase2_minutes,
                                join_session_code,
                                reconnect_session_code,
                                reconnect_token,
                                judge_bundle,
                            }
                            JoinPanel {
                                identity,
                                game_state,
                                ops,
                                join_session_code,
                                join_name,
                                reconnect_session_code,
                                reconnect_token,
                                judge_bundle,
                            }
                        }
                    }
                }
            }
        }
    }
}
