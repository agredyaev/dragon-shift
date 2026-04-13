mod api;
mod components;
mod flows;
mod helpers;
mod realtime;
mod state;

use dioxus::prelude::*;

use components::advanced_panel::AdvancedPanel;
use components::archive_panel::ArchivePanel;
use components::controls_panel::ControlsPanel;
use components::create_panel::CreatePanel;
use components::hero::Hero;
use components::join_panel::JoinPanel;
use components::notice::NoticeBar;
use components::session_panel::SessionPanel;
use components::workshop_brief::WorkshopBrief;

use realtime::bootstrap_realtime;
use state::{ShellScreen, apply_realtime_bootstrap_error, bootstrap_state};

#[cfg(target_arch = "wasm32")]
fn main() {
    #[cfg(debug_assertions)]
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

    let has_session_snapshot = {
        let id = identity.read();
        id.session_snapshot.is_some()
    };
    let render_session_panels_first = {
        let id = identity.read();
        id.screen == ShellScreen::Session
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

    let render_session_panels = has_session_snapshot;

    rsx! {
        main { class: "shell",
            section { class: "shell__container",
                Hero { identity, game_state }
                NoticeBar { ops }
                section { class: "grid",
                    if render_session_panels && render_session_panels_first {
                        SessionPanel {
                            identity,
                            game_state,
                            ops,
                            handover_tags_input,
                            judge_bundle,
                        }
                        ControlsPanel {
                            identity,
                            game_state,
                            ops,
                            handover_tags_input,
                            judge_bundle,
                        }
                        ArchivePanel { game_state, judge_bundle }
                    }
                    WorkshopBrief {}
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
                    if render_session_panels && !render_session_panels_first {
                        SessionPanel {
                            identity,
                            game_state,
                            ops,
                            handover_tags_input,
                            judge_bundle,
                        }
                        ControlsPanel {
                            identity,
                            game_state,
                            ops,
                            handover_tags_input,
                            judge_bundle,
                        }
                        ArchivePanel { game_state, judge_bundle }
                    }
                    AdvancedPanel { identity }
                }
            }
        }
    }
}
