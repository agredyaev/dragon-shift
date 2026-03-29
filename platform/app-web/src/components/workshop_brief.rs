use dioxus::prelude::*;

#[component]
pub fn WorkshopBrief() -> Element {
    rsx! {
        article { class: "panel panel--runtime",
            h2 { class: "panel__title", "Workshop brief" }
            p { class: "panel__body", "One short discovery round, one careful handover, one shared care loop, then a final vote and archive." }
            div { class: "flow-cards",
                article { class: "flow-card",
                    p { class: "flow-card__title", "1. Create pet" }
                    p { class: "flow-card__body", "Start a room, describe your dragon, and get everyone ready to begin." }
                }
                article { class: "flow-card",
                    p { class: "flow-card__title", "2. Discover rules" }
                    p { class: "flow-card__body", "Observe what changes across the shift and capture the signals that matter." }
                }
                article { class: "flow-card",
                    p { class: "flow-card__title", "3. Handover" }
                    p { class: "flow-card__body", "Write practical notes so the next teammate can care for the dragon with confidence." }
                }
                article { class: "flow-card",
                    p { class: "flow-card__title", "4. Care and vote" }
                    p { class: "flow-card__body", "Use the handover, finish the round, then celebrate the most creative dragon together." }
                }
            }
        }
    }
}
