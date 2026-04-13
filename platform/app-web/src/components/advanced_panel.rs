use dioxus::prelude::*;

use crate::state::{default_api_base_url, persist_browser_api_base_url, IdentityState};

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
                    placeholder: "https://dragon-shift.example.com",
                    oninput: move |event| {
                        let raw_value = event.value();
                        let next_value = if raw_value.trim().is_empty() {
                            default_api_base_url()
                        } else {
                            raw_value.clone()
                        };
                        let _ = persist_browser_api_base_url(&raw_value);
                        identity_w.with_mut(|id| id.api_base_url = next_value);
                    }
                }
                p { class: "meta", "This address is saved in this browser and is used for API and realtime session traffic." }
            }
        }
    }
}
