use dioxus::prelude::*;
use protocol::ClientGameState;

use crate::helpers::*;

#[component]
pub fn Phase2View(game_state: Signal<Option<ClientGameState>>) -> Element {
    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let title = phase2_focus_title(state);
    let creator = phase2_creator_label(state);
    let handover = phase2_handover_summary(state);
    let care = phase2_care_copy(state);

    rsx! {
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", {title} }
                p { class: "roster__meta", {creator} }
            }
            span { class: "roster__status roster__status--phase status-connected", "Care" }
        }
        p { class: "panel__body", {care} }
        div { class: "panel__body",
            p { class: "meta", "Handover notes from previous caretaker:" }
            p { {handover} }
        }
    }
}
