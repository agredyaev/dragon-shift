use dioxus::prelude::*;
use protocol::ClientGameState;

use crate::helpers::*;

#[component]
pub fn EndView(game_state: Signal<Option<ClientGameState>>) -> Element {
    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let results_status = end_results_status_copy(state);
    let vote_rows = end_vote_result_rows(state);
    let score_rows = end_player_score_rows(state);
    let is_host = current_player(state).map(|p| p.is_host).unwrap_or(false);

    rsx! {
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", "Workshop results" }
                p { class: "roster__meta", {results_status} }
            }
            span { class: "roster__status roster__status--phase status-connected", "Final" }
        }
        if !vote_rows.is_empty() {
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
        if !score_rows.is_empty() {
            p { class: "meta", "Mechanics Leaderboard" }
            div { class: "roster",
                for row in score_rows {
                    article { class: "roster__item",
                        div {
                            p { class: "roster__name", {row.player_name.clone()} }
                            p { class: "roster__meta", {row.placement_label.clone()} " - " {row.achievements_label.clone()} }
                        }
                        span {
                            class: format!("roster__status {}", if row.is_winner { "status-connected" } else { "status-connecting" }),
                            {row.score_label.clone()}
                        }
                    }
                }
            }
        }
        p {
            class: "meta",
            if is_host {
                "Host can reset the workshop when the group is ready for another round."
            } else {
                "Waiting for the host to reset or archive this workshop."
            }
        }
    }
}
