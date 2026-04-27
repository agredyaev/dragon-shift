use dioxus::prelude::*;

use crate::helpers::notice_class;
use crate::state::{NoticeScope, NoticeTone, OperationState};

#[component]
pub fn NoticeBar(ops: Signal<OperationState>, scope: NoticeScope) -> Element {
    let o = ops.read();
    let Some(notice) = o.notice.clone() else {
        return rsx! {};
    };
    if notice.scope != scope {
        return rsx! {};
    }
    let (role, live) = if notice.tone == NoticeTone::Error {
        ("alert", "assertive")
    } else {
        ("status", "polite")
    };

    rsx! {
        article {
            class: format!("notice {}", notice_class(notice.tone)),
            "data-testid": "notice-bar",
            role: role,
            "aria-live": live,
            "aria-atomic": "true",
            {notice.message}
        }
    }
}
