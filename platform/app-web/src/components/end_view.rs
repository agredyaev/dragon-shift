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
        // ---- Scoring methodology ----
        article { class: "scoring-method",
            p { class: "scoring-method__title", "How scores work" }
            div { class: "scoring-method__body",
                p { class: "scoring-method__section", "The judge evaluates each dragon on two axes (0-100 each):" }
                ul { class: "panel__list",
                    li { strong { "Observation score" } " — awarded to the Phase 1 creator. Measures quality of discovery notes and handover tags: did they identify the dragon's real food, play, and sleep preferences?" }
                    li { strong { "Care score" } " — awarded to the Phase 2 caretaker. Measures how well they followed handover instructions, chose correct actions, and kept stats healthy." }
                }
                p { class: "scoring-method__section", "Final score = observation score + care score (max 200)." }
                p { class: "scoring-method__section", "The judge also considers:" }
                ul { class: "panel__list",
                    li { "Correct action ratio — feeding the right food, playing the right game for the time of day." }
                    li { "Wrong action count — wrong food/play choices escalate happiness penalties (20/25/30/35)." }
                    li { "Cooldown discipline — spamming actions during cooldown counts against the care score." }
                    li { "Stat health at finish — hunger, energy, and happiness levels at the end of Phase 2." }
                    li { "Lowest happiness reached — recovering from a crash shows adaptability." }
                }
                p { class: "scoring-method__section", "Phase 2 decay is 3x stronger than Phase 1. Penalty stacks from wrong actions increase happiness drain." }
            }
        }
        // ---- Achievement reference ----
        article { class: "scoring-method",
            p { class: "scoring-method__title", "Achievements" }
            div { class: "scoring-method__body",
                ul { class: "panel__list",
                    li { strong { "master_chef" } " — first food attempt was correct." }
                    li { strong { "playful_spirit" } " — first play attempt was correct." }
                    li { strong { "speed_learner" } " — found both correct food and play within the first 3 actions." }
                    li { strong { "no_mistakes" } " — zero wrong food or play choices (5+ actions required)." }
                    li { strong { "zen_master" } " — ended Phase 2 with zero penalty stacks (8+ actions)." }
                    li { strong { "perfectionist" } " — 80%+ correct action ratio (10+ actions)." }
                    li { strong { "steady_hand" } " — held happiness above 60 for 20+ consecutive ticks in Phase 2." }
                    li { strong { "comeback_kid" } " — recovered from happiness at or below 15 to finish at or above 70." }
                    li { strong { "helicopter_parent" } " — performed 20+ total actions." }
                    li { strong { "button_masher" } " — triggered 5+ cooldown violations." }
                    li { strong { "chaos_gremlin" } " — reached 4+ penalty stacks at some point." }
                    li { strong { "rock_bottom" } " — happiness dropped to zero." }
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
