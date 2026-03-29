use dioxus::prelude::*;
use protocol::ClientGameState;

use crate::helpers::*;

#[component]
pub fn Phase1View(game_state: Signal<Option<ClientGameState>>) -> Element {
    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let title = phase1_focus_title(state);
    let body = phase1_focus_body(state);
    let observations = phase1_observation_summary(state);
    let emotion = current_dragon(state)
        .map(|d| dragon_emotion_label(d.last_emotion))
        .unwrap_or("");
    let last_action = current_dragon(state)
        .map(|d| dragon_action_label(d.last_action))
        .unwrap_or("");

    rsx! {
        p { class: "meta", "Current dragon mood: " {emotion} }
        p { class: "meta", "Last action: " {last_action} }
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", {title} }
                p { class: "roster__meta", {observations} }
            }
            span { class: "roster__status roster__status--phase status-connecting", "Discovery" }
        }
        p { class: "panel__body", {body} }
    }
}
