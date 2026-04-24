use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

use crate::flows::submit_logout_flow;
use crate::state::{IdentityState, OperationState, ShellScreen};

/// Disclosure-menu ids used by keyboard handlers to restore focus after
/// opening/closing the menu. Kept as constants so the trigger and the
/// items stay in sync.
const TRIGGER_ID: &str = "app-bar-menu-trigger";
const MENU_ID: &str = "app-bar-menu";
const MENU_ITEM_ID_PREFIX: &str = "app-bar-menu-item-";
/// Id on the disclosure wrapper element. The document-level
/// outside-click handler uses it to decide whether a pointerdown
/// originated inside the disclosure (trigger + menu) or outside.
const WRAP_ID: &str = "app-bar-menu-wrap";

/// Count of menu items in the account disclosure. Keep in sync with
/// the rendered list in `AppBar`. Shared with `handle_menu_item_key`
/// so the arrow-key skip-disabled logic sizes its mask correctly.
const MENU_ITEM_COUNT: usize = 2;

/// Focus a DOM element by id. No-op on non-wasm targets (unit tests).
#[cfg(target_arch = "wasm32")]
fn focus_element_by_id(id: &str) {
    use wasm_bindgen::JsCast;
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(id))
        .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = el.focus();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn focus_element_by_id(_id: &str) {}

/// True when `node` is inside the `WRAP_ID` subtree. Used by
/// `onfocusout` to avoid closing the menu on focus moves between the
/// trigger and its menu items (same subtree) — only an outward focus
/// move should close. No-op / always-false on non-wasm targets.
#[cfg(target_arch = "wasm32")]
fn wrap_contains(node: &web_sys::Node) -> bool {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return false;
    };
    let Some(wrap) = document.get_element_by_id(WRAP_ID) else {
        return false;
    };
    wrap.contains(Some(node))
}

fn menu_item_id(index: usize) -> String {
    format!("{MENU_ITEM_ID_PREFIX}{index}")
}

/// Install a document-level `pointerdown` listener that closes the
/// disclosure menu when the pointerdown target is outside the
/// `WRAP_ID` container. Returns a handle that detaches the listener
/// when dropped. No-op on non-wasm targets.
#[cfg(target_arch = "wasm32")]
struct OutsideClickGuard {
    target: web_sys::EventTarget,
    closure: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
}

#[cfg(target_arch = "wasm32")]
impl Drop for OutsideClickGuard {
    fn drop(&mut self) {
        use wasm_bindgen::JsCast;
        let _ = self.target.remove_event_listener_with_callback(
            "pointerdown",
            self.closure.as_ref().unchecked_ref(),
        );
    }
}

#[cfg(target_arch = "wasm32")]
fn install_outside_click_guard(mut open: Signal<bool>) -> Option<OutsideClickGuard> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let document = web_sys::window()?.document()?;
    let target: web_sys::EventTarget = document.unchecked_into();

    let closure = Closure::wrap(Box::new(move |event: web_sys::Event| {
        // Resolve the pointerdown target to an Element we can test with
        // `contains()` against the disclosure wrapper.
        let Some(node) = event
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
        else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };
        let Some(wrap) = document.get_element_by_id(WRAP_ID) else {
            return;
        };
        if !wrap.contains(Some(&node)) && *open.read() {
            open.set(false);
        }
    }) as Box<dyn FnMut(web_sys::Event)>);

    target
        .add_event_listener_with_callback("pointerdown", closure.as_ref().unchecked_ref())
        .ok()?;

    Some(OutsideClickGuard { target, closure })
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
fn install_outside_click_guard(_open: Signal<bool>) -> Option<()> {
    None
}

/// Returns `true` when a focusout event indicates focus is leaving
/// the disclosure wrap entirely (i.e. `related_target` resolves to a
/// node outside `WRAP_ID`). Returns `false` when focus is moving
/// within the subtree (trigger → item 0 on keyboard open) or when
/// `related_target` is `null` (programmatic focus transitions). On
/// non-wasm targets returns `false` so the menu never closes spuriously
/// in unit tests.
#[cfg(target_arch = "wasm32")]
fn focus_leaving_wrap(evt: &Event<FocusData>) -> bool {
    use wasm_bindgen::JsCast;
    let data = evt.data();
    let Some(web_evt) = data.downcast::<web_sys::FocusEvent>() else {
        // Headless / server-rendered platform data — don't close.
        return false;
    };
    let Some(rt) = web_evt.related_target() else {
        // `null` related_target: programmatic focus move (e.g. the
        // trigger's `focus_element_by_id(ITEM_0_ID)` path). Stay open.
        return false;
    };
    let Some(node) = rt.dyn_ref::<web_sys::Node>() else {
        return false;
    };
    !wrap_contains(node)
}

#[cfg(not(target_arch = "wasm32"))]
fn focus_leaving_wrap(_evt: &Event<FocusData>) -> bool {
    false
}

/// Top-level app bar (landmark `<header>`): wordmark, reserved center
/// slot, and — when signed in — the account disclosure menu.
///
/// A11y contract (UX_RECOMPOSE_v2 §4.A "A11Y contract"):
/// - trigger: `aria-haspopup="menu"`, `aria-expanded` reflects open
/// - menu: `role="menu"`, items `role="menuitem"`
/// - keyboard: Enter/Space/ArrowDown open; ArrowUp/ArrowDown wrap;
///   Escape closes and returns focus to the trigger; Tab closes
///   naturally (focus leaves container → onfocusout).
/// - closed menu uses `hidden` (display:none) rather than
///   `aria-hidden=true`.
#[component]
pub fn AppBar(identity: Signal<IdentityState>, ops: Signal<OperationState>) -> Element {
    // Local disclosure state. Lives in the component so the menu closes
    // automatically when `AppBar` unmounts (e.g. on sign-out).
    let mut open = use_signal(|| false);

    // Snapshot read: account is the only bit we need from identity.
    let (account_name, is_signed_in, current_screen) = {
        let id = identity.read();
        (
            id.account.as_ref().map(|a| a.name.clone()),
            id.account.is_some(),
            id.screen.clone(),
        )
    };

    // Snapshot: logout / flow-sensitive actions must be disabled while
    // another pending flow is in-flight. Mirrors the prior
    // account_home.rs contract (`disabled: ops.read().pending_flow.is_some()`).
    let flow_pending = ops.read().pending_flow.is_some();

    // Disabled-state snapshot for each menu item, in render order.
    // Used by `handle_menu_item_key` to skip over disabled items when
    // arrow-navigating (V1 M1 / V3 MEDIUM-1 / V5 MEDIUM-2).
    // Index 0: Create dragon (always enabled).
    // Index 1: Log out (disabled while a flow is pending).
    let menu_disabled: [bool; MENU_ITEM_COUNT] = [false, flow_pending];

    // Wordmark routes home when signed in and not already there. On
    // SignIn or AccountHome it's a no-op (disabled) so keyboard users
    // don't get a trap that looks interactive.
    // CreateCharacter holds draft form state; wordmark is disabled
    // there until Tier 3a-2 adds a T-5 confirmation modal.
    let wordmark_disabled = !matches!(
        current_screen,
        ShellScreen::PickCharacter { .. }
    );

    // Outside-click close (UX_RECOMPOSE_v2 §4.A contract A-3). Attach a
    // document-level pointerdown listener while `open` is true and
    // detach it when it flips false. The guard must outlive the effect
    // closure — binding it to a `_guard` local inside `use_effect`
    // attaches and detaches the listener in the same microtask (V3
    // HIGH-1), so we own it in a `use_hook`-backed `Rc<RefCell<_>>`
    // that persists across renders.
    #[cfg(target_arch = "wasm32")]
    {
        let guard_cell: Rc<RefCell<Option<OutsideClickGuard>>> =
            use_hook(|| Rc::new(RefCell::new(None)));
        use_effect({
            let guard_cell = guard_cell.clone();
            move || {
                if *open.read() {
                    if guard_cell.borrow().is_none() {
                        *guard_cell.borrow_mut() = install_outside_click_guard(open);
                    }
                } else if guard_cell.borrow().is_some() {
                    // Dropping the guard runs `OutsideClickGuard::drop`
                    // and detaches the document listener.
                    *guard_cell.borrow_mut() = None;
                }
            }
        });
    }

    rsx! {
        header { class: "app-bar", role: "banner",
            // --- Left: wordmark ---
            button {
                class: "app-bar__wordmark",
                "data-testid": "app-bar-wordmark",
                r#type: "button",
                disabled: wordmark_disabled,
                onclick: move |_| {
                    if wordmark_disabled {
                        return;
                    }
                    identity.with_mut(|id| {
                        if id.account.is_some() {
                            id.screen = ShellScreen::AccountHome;
                        }
                    });
                },
                "DRAGON SHIFT"
            }

            // --- Center: reserved for Tier 3a-2 (stepper / phase+clock) ---
            div { class: "app-bar__center" }

            // --- Right: account menu (only when signed in) ---
            if is_signed_in {
                div {
                    id: WRAP_ID,
                    class: "app-bar__menu-wrap",
                    // Close when focus leaves the whole cluster. We
                    // gate on `related_target` so keyboard-open (trigger
                    // → item 0 focus transition) doesn't trip the
                    // handler: focus moves within the same subtree, so
                    // the menu must stay open (V5 HIGH-1).
                    onfocusout: move |evt| {
                        if !*open.read() {
                            return;
                        }
                        if focus_leaving_wrap(&evt) {
                            open.set(false);
                        }
                    },
                    button {
                        id: TRIGGER_ID,
                        class: "app-bar__menu-trigger",
                        "data-testid": "app-bar-menu-trigger",
                        r#type: "button",
                        "aria-haspopup": "menu",
                        "aria-expanded": if *open.read() { "true" } else { "false" },
                        "aria-controls": MENU_ID,
                        // Prevent mousedown from stealing focus so the
                        // wrapper's onfocusout doesn't race the onclick
                        // toggle when the menu is already open. The
                        // trigger is still keyboard-activatable via
                        // onkeydown below.
                        onmousedown: move |evt| evt.prevent_default(),
                        onclick: move |_| {
                            let next = !*open.read();
                            open.set(next);
                            if next {
                                focus_element_by_id(&menu_item_id(0));
                            }
                        },
                        onkeydown: move |event| {
                            let key = event.key();
                            let is_space = matches!(&key, Key::Character(c) if c == " ");
                            if matches!(key, Key::Enter) || is_space {
                                event.prevent_default();
                                let next = !*open.read();
                                open.set(next);
                                if next {
                                    focus_element_by_id(&menu_item_id(0));
                                }
                            } else if matches!(key, Key::ArrowDown) {
                                event.prevent_default();
                                if !*open.read() {
                                    open.set(true);
                                }
                                focus_element_by_id(&menu_item_id(0));
                            } else if matches!(key, Key::Escape) && *open.read() {
                                event.prevent_default();
                                open.set(false);
                                // Symmetry with Escape-on-item: keep
                                // focus on the trigger after close.
                                focus_element_by_id(TRIGGER_ID);
                            }
                        },
                        {account_name.clone().unwrap_or_default()}
                        " "
                        span { "aria-hidden": "true", "\u{25BE}" }
                    }

                    ul {
                        id: MENU_ID,
                        class: "app-bar__menu",
                        role: "menu",
                        hidden: !*open.read(),
                        // Item 0: Create dragon
                        li { role: "none",
                            button {
                                id: "{menu_item_id(0)}",
                                class: "app-bar__menu-item",
                                "data-testid": "app-bar-menu-manage-dragons",
                                r#type: "button",
                                role: "menuitem",
                                tabindex: "-1",
                                onclick: move |_| {
                                    open.set(false);
                                    identity.with_mut(|id| {
                                        id.screen = ShellScreen::CreateCharacter;
                                    });
                                },
                                onkeydown: move |event| {
                                    handle_menu_item_key(event, 0, open, menu_disabled);
                                },
                                "Create a dragon"
                            }
                        }
                        // Item 1: Logout
                        li { role: "none",
                            button {
                                id: "{menu_item_id(1)}",
                                class: "app-bar__menu-item",
                                "data-testid": "app-bar-menu-logout",
                                r#type: "button",
                                role: "menuitem",
                                tabindex: "-1",
                                disabled: flow_pending,
                                onclick: move |_| {
                                    open.set(false);
                                    spawn(submit_logout_flow(identity, ops));
                                },
                                onkeydown: move |event| {
                                    handle_menu_item_key(event, 1, open, menu_disabled);
                                },
                                "Log out"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Shared roving-tabindex-ish keyboard handler for menu items.
/// Arrow keys wrap and skip disabled items (V1 M1 / V3 MEDIUM-1 /
/// V5 MEDIUM-2); Escape closes and returns focus to the trigger;
/// Enter/Space fall through so the browser synthesises a click on the
/// button (native button activation semantics).
fn handle_menu_item_key(
    event: Event<KeyboardData>,
    index: usize,
    mut open: Signal<bool>,
    disabled: [bool; MENU_ITEM_COUNT],
) {
    let count = MENU_ITEM_COUNT;
    let all_disabled = disabled.iter().all(|&d| d);
    let step_to = |start: usize, delta: isize| -> Option<usize> {
        if all_disabled {
            return None;
        }
        let mut i = start;
        for _ in 0..count {
            i = ((i as isize + delta).rem_euclid(count as isize)) as usize;
            if !disabled[i] {
                return Some(i);
            }
        }
        None
    };
    match event.key() {
        Key::ArrowDown => {
            event.prevent_default();
            match step_to(index, 1) {
                Some(next) => focus_element_by_id(&menu_item_id(next)),
                None => focus_element_by_id(TRIGGER_ID),
            }
        }
        Key::ArrowUp => {
            event.prevent_default();
            match step_to(index, -1) {
                Some(prev) => focus_element_by_id(&menu_item_id(prev)),
                None => focus_element_by_id(TRIGGER_ID),
            }
        }
        Key::Escape => {
            event.prevent_default();
            open.set(false);
            focus_element_by_id(TRIGGER_ID);
        }
        // Enter / Space: let the browser activate the button.
        _ => {}
    }
}
