use dioxus::prelude::*;

use crate::helpers::notice_class;
use crate::state::OperationState;

#[component]
pub fn NoticeBar(ops: Signal<OperationState>) -> Element {
    let o = ops.read();
    let Some(notice) = o.notice.clone() else {
        return rsx! {};
    };

    rsx! {
        article { class: format!("notice {}", notice_class(notice.tone)),
            {notice.message}
        }
    }
}
