use dioxus::prelude::*;

use crate::helpers::notice_class;
use crate::state::{NoticeScope, OperationState};

#[component]
pub fn NoticeBar(ops: Signal<OperationState>, scope: NoticeScope) -> Element {
    let o = ops.read();
    let Some(notice) = o.notice.clone() else {
        return rsx! {};
    };
    if notice.scope != scope {
        return rsx! {};
    }

    rsx! {
        article {
            class: format!("notice {}", notice_class(notice.tone)),
            "data-testid": "notice-bar",
            role: "status",
            "aria-live": "polite",
            "aria-atomic": "true",
            {notice.message}
        }
    }
}
