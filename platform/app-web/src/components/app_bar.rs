use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

use crate::flows::{
    begin_load_my_characters, load_my_characters_flow, start_delete_character_flow,
    start_logout_flow, start_rename_character_flow,
};
use crate::state::{IdentityState, OperationState, PendingFlow, ShellScreen, navigate_to_screen};
use protocol::SpriteSet;

/// Disclosure-menu ids used by keyboard handlers to restore focus after
/// opening/closing the menu. Kept as constants so the trigger and the
/// items stay in sync.
const TRIGGER_ID: &str = "app-bar-menu-trigger";
const MENU_ID: &str = "app-bar-menu";
const MENU_ITEM_ID_PREFIX: &str = "app-bar-menu-item-";
const MANAGE_DRAGONS_TITLE_ID: &str = "manage-dragons-modal-title";
const DELETE_CHARACTER_TITLE_ID: &str = "delete-character-modal-title";
const RENAME_CHARACTER_TITLE_ID: &str = "rename-character-modal-title";
const RENAME_CHARACTER_INPUT_ID: &str = "rename-character-input";
/// Id on the disclosure wrapper element. The document-level
/// outside-click handler uses it to decide whether a pointerdown
/// originated inside the disclosure (trigger + menu) or outside.
const WRAP_ID: &str = "app-bar-menu-wrap";

const CHARACTER_SPRITE_LABELS: [&str; 4] = ["Neutral", "Happy", "Angry", "Sleepy"];

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

fn sprite_for_index(sprites: &SpriteSet, index: usize) -> &str {
    match index {
        0 => &sprites.neutral,
        1 => &sprites.happy,
        2 => &sprites.angry,
        _ => &sprites.sleepy,
    }
}

fn character_display_name(character_name: Option<&str>, fallback_number: usize) -> String {
    character_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Dragon {fallback_number}"))
}

fn character_name_input_value(character_name: Option<&str>) -> String {
    character_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_default()
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
    let mut focus_menu_on_open = use_signal(|| false);
    let mut manage_dragons_open = use_signal(|| false);
    let mut focus_manage_dragons_on_open = use_signal(|| false);
    let mut focus_manage_dragons_after_nested_close = use_signal(|| false);
    let mut pending_delete_character = use_signal(|| None::<(String, usize)>);
    let mut focus_delete_character_on_open = use_signal(|| false);
    let mut pending_rename_character = use_signal(|| None::<(String, String)>);
    let mut focus_rename_character_on_open = use_signal(|| false);
    let mut rename_character_name = use_signal(String::new);

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
    let command_pending = ops.read().pending_command.is_some();
    let judge_bundle_pending = ops.read().pending_judge_bundle;
    let delete_character_pending = ops.read().pending_flow == Some(PendingFlow::DeleteCharacter);
    let rename_character_pending = ops.read().pending_flow == Some(PendingFlow::RenameCharacter);
    let my_characters_loading = ops.read().my_characters_loading;
    let my_characters_loaded = ops.read().my_characters_loaded;
    let my_characters_load_failed = ops.read().my_characters_load_failed;
    let my_characters_count = ops.read().my_characters.len();
    let my_characters_limit = ops.read().my_characters_limit;
    let rename_input_value = rename_character_name.read().clone();
    let rename_input_empty = rename_input_value.trim().is_empty();
    let secondary_dialog_open =
        pending_delete_character.read().is_some() || pending_rename_character.read().is_some();
    let visible_my_characters = if *manage_dragons_open.read() {
        ops.read().my_characters.clone()
    } else {
        Vec::new()
    };

    // Disabled-state snapshot for each menu item, in render order.
    // Used by `handle_menu_item_key` to skip over disabled items when
    // arrow-navigating (V1 M1 / V3 MEDIUM-1 / V5 MEDIUM-2).
    // Index 0: Create dragon (always enabled).
    // Index 1: Log out (disabled while a flow is pending).
    let menu_disabled: [bool; MENU_ITEM_COUNT] = [
        my_characters_loading || flow_pending || command_pending || judge_bundle_pending,
        flow_pending || command_pending || judge_bundle_pending,
    ];

    // Wordmark routes home when signed in and not already there. On
    // SignIn or AccountHome it's a no-op (disabled) so keyboard users
    // don't get a trap that looks interactive.
    // CreateCharacter holds draft form state; wordmark is disabled
    // there until Tier 3a-2 adds a T-5 confirmation modal.
    let wordmark_disabled = !matches!(current_screen, ShellScreen::PickCharacter { .. })
        || ops.read().pending_flow == Some(PendingFlow::Join);

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

    use_effect(move || {
        if *open.read() && *focus_menu_on_open.read() {
            focus_menu_on_open.set(false);
            focus_first_enabled_menu_item(menu_disabled);
        }
    });

    use_effect(move || {
        if *manage_dragons_open.read() && *focus_manage_dragons_on_open.read() {
            focus_manage_dragons_on_open.set(false);
            focus_element_by_id(MANAGE_DRAGONS_TITLE_ID);
        }
    });

    use_effect(move || {
        if *manage_dragons_open.read()
            && pending_delete_character.read().is_none()
            && pending_rename_character.read().is_none()
            && *focus_manage_dragons_after_nested_close.read()
        {
            focus_manage_dragons_after_nested_close.set(false);
            focus_element_by_id(MANAGE_DRAGONS_TITLE_ID);
        }
    });

    use_effect(move || {
        if pending_delete_character.read().is_some() && *focus_delete_character_on_open.read() {
            focus_delete_character_on_open.set(false);
            focus_element_by_id(DELETE_CHARACTER_TITLE_ID);
        }
    });

    use_effect(move || {
        if pending_rename_character.read().is_some() && *focus_rename_character_on_open.read() {
            focus_rename_character_on_open.set(false);
            focus_element_by_id(RENAME_CHARACTER_INPUT_ID);
        }
    });

    rsx! {
        if *manage_dragons_open.read() {
            div {
                class: "modal-backdrop",
                role: "presentation",
                hidden: secondary_dialog_open,
                onclick: move |_| {
                    manage_dragons_open.set(false);
                    focus_element_by_id(TRIGGER_ID);
                },
                div {
                    class: "modal-card modal-card--characters",
                    role: "dialog",
                    "aria-modal": "true",
                    "aria-labelledby": MANAGE_DRAGONS_TITLE_ID,
                    "aria-describedby": "manage-dragons-modal-body",
                    onclick: move |event| event.stop_propagation(),
                    div { class: "panel__header",
                        h2 {
                            id: MANAGE_DRAGONS_TITLE_ID,
                            class: "panel__title modal-card__title",
                            tabindex: "-1",
                            "Your Dragons"
                        }
                        span { class: "badge", "{my_characters_count} / {my_characters_limit}" }
                    }
                    p {
                        id: "manage-dragons-modal-body",
                        class: "panel__body modal-card__body",
                        "Create, rename, or delete dragons from your account."
                    }
                    div { class: "button-row button-row--home-action modal-card__primary-action",
                        button {
                            class: "button button--secondary",
                            "data-testid": "create-character-button",
                            disabled: flow_pending || command_pending || judge_bundle_pending,
                            onclick: move |_| {
                                manage_dragons_open.set(false);
                                identity.with_mut(|id| {
                                    ops.with_mut(|o| {
                                        navigate_to_screen(id, o, ShellScreen::CreateCharacter);
                                    });
                                });
                            },
                            "Create a dragon"
                        }
                    }
                    div { class: "panel__stack",
                        if my_characters_loading && visible_my_characters.is_empty() {
                            p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", "Loading dragons..." }
                        } else if my_characters_load_failed && visible_my_characters.is_empty() {
                            p { class: "meta", role: "alert", "Could not load dragons right now." }
                        } else if my_characters_loaded && visible_my_characters.is_empty() {
                            p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", "No dragons yet. Create one to use it in workshops." }
                        } else if visible_my_characters.is_empty() {
                            p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", "Loading dragons..." }
                        } else {
                            div { class: "roster roster--modal",
                                for (character_index, character) in visible_my_characters.iter().enumerate() {
                                    {
                                        let rename_character_id = character.id.clone();
                                        let delete_character_id = character.id.clone();
                                        let character_number = character_index + 1;
                                        let character_name = character_display_name(
                                            character.name.as_deref(),
                                            character_number,
                                        );
                                        let rename_current_name = character_name_input_value(
                                            character.name.as_deref(),
                                        );
                                        rsx! {
                                            article { class: "roster__item pick-character-row", key: "{character.id}",
                                                div { class: "pick-character-row__body",
                                                    div {
                                                        class: "pick-character-row__sprites",
                                                        "aria-label": "Dragon {character_number} sprites",
                                                        for (sprite_index, label) in CHARACTER_SPRITE_LABELS.iter().enumerate() {
                                                            div { class: "pick-character-row__sprite-frame",
                                                                img {
                                                                    class: "pick-character-row__sprite",
                                                                    src: "data:image/png;base64,{sprite_for_index(&character.sprites, sprite_index)}",
                                                                    alt: "{character_name}: {label} sprite",
                                                                }
                                                            }
                                                        }
                                                    }
                                                    div { class: "pick-character-row__copy",
                                                        p { class: "roster__name", "{character_name}" }
                                                        p { class: "roster__meta", "Ready for workshops" }
                                                    }
                                                }
                                                if !my_characters_loading {
                                                    div { class: "button-row roster__actions",
                                                        button {
                                                            class: "button button--secondary button--small",
                                                            "data-testid": "rename-character-button",
                                                            disabled: flow_pending || command_pending || judge_bundle_pending,
                                                            onclick: move |_| {
                                                                let current_name = rename_current_name.clone();
                                                                rename_character_name.set(current_name.clone());
                                                                pending_rename_character.set(Some((
                                                                    rename_character_id.clone(),
                                                                    character_name.clone(),
                                                                )));
                                                                focus_rename_character_on_open.set(true);
                                                            },
                                                            if rename_character_pending { "Renaming..." } else { "Rename" }
                                                        }
                                                        button {
                                                            class: "button button--danger button--small",
                                                            "data-testid": "delete-character-button",
                                                            disabled: flow_pending || command_pending || judge_bundle_pending,
                                                            onclick: move |_| {
                                                                pending_delete_character.set(Some((
                                                                    delete_character_id.clone(),
                                                                    character_number,
                                                                )));
                                                                focus_delete_character_on_open.set(true);
                                                            },
                                                            if delete_character_pending { "Deleting..." } else { "Delete" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if my_characters_loading {
                                p { class: "meta", role: "status", "aria-live": "polite", "aria-atomic": "true", "Refreshing dragons..." }
                            } else if my_characters_load_failed {
                                p { class: "meta", role: "alert", "Could not refresh dragons right now." }
                            }
                        }
                    }
                    div { class: "button-row modal-card__actions",
                        button {
                            class: "button button--primary",
                            onclick: move |_| {
                                manage_dragons_open.set(false);
                                focus_element_by_id(TRIGGER_ID);
                            },
                            "Close"
                        }
                    }
                }
            }
        }

        if let Some((delete_character_id, delete_character_number)) = pending_delete_character.read().clone() {
            div {
                class: "modal-backdrop",
                role: "presentation",
                onclick: move |_| {
                    pending_delete_character.set(None);
                    focus_manage_dragons_after_nested_close.set(true);
                },
                div {
                    class: "modal-card",
                    role: "dialog",
                    "aria-modal": "true",
                    "aria-labelledby": DELETE_CHARACTER_TITLE_ID,
                    "aria-describedby": "delete-character-modal-body",
                    onclick: move |event| event.stop_propagation(),
                    h2 {
                        id: DELETE_CHARACTER_TITLE_ID,
                        class: "panel__title modal-card__title",
                        tabindex: "-1",
                        "Delete Dragon"
                    }
                    p {
                        id: "delete-character-modal-body",
                        class: "panel__body modal-card__body",
                        "Delete Dragon {delete_character_number}? This pet will no longer be available for new workshops."
                    }
                    div { class: "button-row modal-card__actions",
                        button {
                            class: "button button--secondary",
                            disabled: delete_character_pending,
                            onclick: move |_| {
                                pending_delete_character.set(None);
                                focus_manage_dragons_after_nested_close.set(true);
                            },
                            "Cancel"
                        }
                        button {
                            class: "button button--danger",
                            "data-testid": "confirm-delete-character-button",
                            disabled: delete_character_pending,
                            onclick: move |_| {
                                if start_delete_character_flow(
                                    identity,
                                    ops,
                                    delete_character_id.clone(),
                                ) {
                                    pending_delete_character.set(None);
                                    focus_manage_dragons_after_nested_close.set(true);
                                }
                            },
                            if delete_character_pending { "Deleting..." } else { "Delete" }
                        }
                    }
                }
            }
        }

        if let Some((rename_character_id, current_character_name)) = pending_rename_character.read().clone() {
            div {
                class: "modal-backdrop",
                role: "presentation",
                onclick: move |_| {
                    pending_rename_character.set(None);
                    focus_manage_dragons_after_nested_close.set(true);
                },
                div {
                    class: "modal-card",
                    role: "dialog",
                    "aria-modal": "true",
                    "aria-labelledby": RENAME_CHARACTER_TITLE_ID,
                    "aria-describedby": "rename-character-modal-body",
                    onclick: move |event| event.stop_propagation(),
                    h2 {
                        id: RENAME_CHARACTER_TITLE_ID,
                        class: "panel__title modal-card__title",
                        "Rename Dragon"
                    }
                    p {
                        id: "rename-character-modal-body",
                        class: "panel__body modal-card__body",
                        "Choose a new name for {current_character_name}."
                    }
                    input {
                        class: "input",
                        "data-testid": "rename-character-input",
                        id: RENAME_CHARACTER_INPUT_ID,
                        r#type: "text",
                        maxlength: 64,
                        value: "{rename_input_value}",
                        disabled: rename_character_pending,
                        oninput: move |event| rename_character_name.set(event.value()),
                    }
                    div { class: "button-row modal-card__actions",
                        button {
                            class: "button button--secondary",
                            disabled: rename_character_pending,
                            onclick: move |_| {
                                pending_rename_character.set(None);
                                focus_manage_dragons_after_nested_close.set(true);
                            },
                            "Cancel"
                        }
                        button {
                            class: "button button--primary",
                            "data-testid": "confirm-rename-character-button",
                            disabled: rename_character_pending || rename_input_empty,
                            onclick: move |_| {
                                let next_name = rename_character_name.read().clone();
                                if start_rename_character_flow(
                                    identity,
                                    ops,
                                    rename_character_id.clone(),
                                    next_name,
                                ) {
                                    pending_rename_character.set(None);
                                    focus_manage_dragons_after_nested_close.set(true);
                                }
                            },
                            if rename_character_pending { "Renaming..." } else { "Save" }
                        }
                    }
                }
            }
        }

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
                        ops.with_mut(|o| {
                            if id.account.is_some() {
                                navigate_to_screen(id, o, ShellScreen::AccountHome);
                            }
                        });
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
                                focus_menu_on_open.set(true);
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
                                    focus_menu_on_open.set(true);
                                }
                            } else if matches!(key, Key::ArrowDown) {
                                event.prevent_default();
                                if !*open.read() {
                                    open.set(true);
                                    focus_menu_on_open.set(true);
                                } else {
                                    focus_first_enabled_menu_item(menu_disabled);
                                }
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
                        // Item 0: Manage dragons
                        li { role: "none",
                            button {
                                id: "{menu_item_id(0)}",
                                class: "app-bar__menu-item",
                                "data-testid": "app-bar-menu-manage-dragons",
                                r#type: "button",
                                role: "menuitem",
                                tabindex: "-1",
                                disabled: my_characters_loading || flow_pending || command_pending || judge_bundle_pending,
                                onclick: move |_| {
                                    if my_characters_loading || flow_pending || command_pending || judge_bundle_pending {
                                        return;
                                    }
                                    open.set(false);
                                    ops.with_mut(begin_load_my_characters);
                                    manage_dragons_open.set(true);
                                    focus_manage_dragons_on_open.set(true);
                                    spawn(load_my_characters_flow(identity, ops));
                                },
                                onkeydown: move |event| {
                                    handle_menu_item_key(event, 0, open, menu_disabled);
                                },
                                "Your dragons"
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
                                disabled: flow_pending || command_pending || judge_bundle_pending,
                                onclick: move |_| {
                                    open.set(false);
                                    let _ = start_logout_flow(identity, ops);
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

fn focus_first_enabled_menu_item(disabled: [bool; MENU_ITEM_COUNT]) {
    match disabled.iter().position(|&item_disabled| !item_disabled) {
        Some(index) => focus_element_by_id(&menu_item_id(index)),
        None => focus_element_by_id(TRIGGER_ID),
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
