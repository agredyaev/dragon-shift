use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle, Phase};

use crate::helpers::{judge_bundle_dragon_rows, judge_bundle_player_rows, judge_bundle_summary};

#[component]
pub fn ArchivePanel(
    game_state: Signal<Option<ClientGameState>>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Element {
    let gs = game_state.read();
    let jb = judge_bundle.read();

    let summary_label = jb.as_ref().map(judge_bundle_summary).unwrap_or_else(|| {
        if gs.as_ref().map(|s| s.phase == Phase::End).unwrap_or(false) {
            "Build the workshop archive to capture the final workshop snapshot.".to_string()
        } else {
            String::new()
        }
    });

    let player_rows = jb
        .as_ref()
        .map(judge_bundle_player_rows)
        .unwrap_or_default();
    let dragon_rows = jb
        .as_ref()
        .map(judge_bundle_dragon_rows)
        .unwrap_or_default();

    rsx! {
        article { class: "panel panel--judge",
            h2 { class: "panel__title", "Workshop archive" }
            p { class: "panel__body", {summary_label.clone()} }
            if !player_rows.is_empty() {
                p { class: "meta", "Captured final standings" }
                div { class: "roster",
                    for row in player_rows {
                        article { class: "roster__item",
                            div {
                                p { class: "roster__name", {row.player_name.clone()} }
                                p { class: "roster__meta", {row.achievements_label.clone()} }
                            }
                            span {
                                class: format!("roster__status {}", if row.is_top_score { "status-connected" } else { "status-connecting" }),
                                {row.score_label.clone()}
                            }
                        }
                    }
                }
            }
            if !dragon_rows.is_empty() {
                p { class: "meta", "Captured dragons" }
                div { class: "roster",
                    for row in dragon_rows {
                        article { class: "roster__item",
                            div {
                                p { class: "roster__name", {row.dragon_name.clone()} }
                                p { class: "roster__meta", "Creator: " {row.creator_name.clone()} " - Caretaker: " {row.caretaker_name.clone()} }
                                p { class: "roster__meta", {row.actions_label.clone()} " - " {row.handover_label.clone()} }
                            }
                            span { class: "roster__status status-connected", {row.votes_label.clone()} }
                        }
                    }
                }
            }
        }
    }
}
