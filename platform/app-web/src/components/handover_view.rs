use dioxus::prelude::*;
use protocol::ClientGameState;

use crate::helpers::*;

#[component]
pub fn HandoverView(
    game_state: Signal<Option<ClientGameState>>,
    handover_tags_input: Signal<String>,
) -> Element {
    let gs = game_state.read();
    let Some(state) = gs.as_ref() else {
        return rsx! {};
    };

    let title = handover_focus_title(state);
    let summary = handover_saved_summary(state);
    let status = handover_status_copy(state);
    let saved_tags = handover_saved_tags(state);
    let draft_count = {
        let input = handover_tags_input.read();
        parse_tags_input(&input).len()
    };

    rsx! {
        article { class: "roster__item roster__item--phase",
            div {
                p { class: "roster__name", {title} }
                p { class: "roster__meta", {summary} }
            }
            span { class: "roster__status roster__status--phase status-connecting", "Handover" }
        }
        p { class: "panel__body", {status} }
        p { class: "meta", "Draft rules parsed from input: " {draft_count.to_string()} }
        if !saved_tags.is_empty() {
            div { class: "roster",
                for tag in saved_tags {
                    article { class: "roster__item",
                        div {
                            p { class: "roster__name", {tag} }
                            p { class: "roster__meta", "Saved handover rule" }
                        }
                        span { class: "roster__status status-connected", "Saved" }
                    }
                }
            }
        }
    }
}
