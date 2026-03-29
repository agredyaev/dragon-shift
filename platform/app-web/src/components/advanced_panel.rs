use dioxus::prelude::*;

use crate::state::IdentityState;

#[component]
pub fn AdvancedPanel(identity: Signal<IdentityState>) -> Element {
    let api_base_url = identity.read().api_base_url.clone();

    let mut identity_w = identity;

    rsx! {
        article { class: "panel panel--advanced",
            h2 { class: "panel__title", "Advanced" }
            p { class: "panel__body", "Use a different address only when you want this screen to point at another workshop server." }
            div { class: "panel__stack",
                input {
                    class: "input",
                    value: api_base_url,
                    placeholder: "http://127.0.0.1:4100",
                    oninput: move |event| identity_w.with_mut(|id| id.api_base_url = event.value())
                }
                p { class: "meta", "Everyone in the workshop sees the same session state through this connection." }
            }
        }
    }
}
