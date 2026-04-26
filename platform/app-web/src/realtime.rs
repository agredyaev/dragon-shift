use dioxus::prelude::*;
use protocol::{ClientGameState, JudgeBundle};

use crate::state::{IdentityState, OperationState};

#[cfg(target_arch = "wasm32")]
use protocol::{ClientWsMessage, ServerWsMessage};

#[cfg(target_arch = "wasm32")]
use crate::api::{build_session_envelope, build_ws_url};

#[cfg(target_arch = "wasm32")]
use crate::state::{
    ConnectionStatus, NoticeScope, apply_realtime_connecting, apply_server_ws_message,
    error_notice, scoped_notice,
};

#[cfg(target_arch = "wasm32")]
use std::cell::{Cell, RefCell};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, closure::Closure};

#[cfg(target_arch = "wasm32")]
pub struct RealtimeClientHandle {
    socket: web_sys::WebSocket,
    _onopen: Closure<dyn FnMut(web_sys::Event)>,
    _onmessage: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _onerror: Closure<dyn FnMut(web_sys::ErrorEvent)>,
    _onclose: Closure<dyn FnMut(web_sys::Event)>,
}

#[cfg(target_arch = "wasm32")]
std::thread_local! {
    static REALTIME_CLIENT: RefCell<Option<RealtimeClientHandle>> = const { RefCell::new(None) };
    static REALTIME_GENERATION: Cell<u64> = const { Cell::new(0) };
}

#[cfg(target_arch = "wasm32")]
fn next_realtime_generation() -> u64 {
    REALTIME_GENERATION.with(|generation| {
        let next = generation.get().checked_add(1).unwrap_or(1).max(1);
        generation.set(next);
        next
    })
}

#[cfg(target_arch = "wasm32")]
fn is_current_generation(generation: u64) -> bool {
    REALTIME_GENERATION.with(|current| current.get() == generation)
}

#[cfg(target_arch = "wasm32")]
fn close_socket_silently(handle: RealtimeClientHandle) {
    handle.socket.set_onopen(None);
    handle.socket.set_onmessage(None);
    handle.socket.set_onerror(None);
    handle.socket.set_onclose(None);
    let _ = handle.socket.close();
}

#[cfg(target_arch = "wasm32")]
pub fn disconnect_realtime() {
    next_realtime_generation();
    clear_realtime_client();
}

#[cfg(target_arch = "wasm32")]
fn clear_realtime_client() {
    REALTIME_CLIENT.with(|client| {
        if let Some(existing) = client.borrow_mut().take() {
            close_socket_silently(existing);
        }
    });
}

#[cfg(target_arch = "wasm32")]
pub fn bootstrap_realtime(
    mut identity: Signal<IdentityState>,
    game_state: Signal<Option<ClientGameState>>,
    mut ops: Signal<OperationState>,
    judge_bundle: Signal<Option<JudgeBundle>>,
) -> Result<(), String> {
    let generation = next_realtime_generation();
    clear_realtime_client();

    let (base_url, snapshot) = {
        let id = identity.read();
        (id.api_base_url.clone(), id.session_snapshot.clone())
    };
    let snapshot =
        snapshot.ok_or_else(|| "Join a workshop before syncing the session.".to_string())?;
    identity.with_mut(|id| {
        ops.with_mut(|o| {
            apply_realtime_connecting(id, o);
        });
    });
    let envelope_json = serde_json::to_string(&ClientWsMessage::AttachSession(
        build_session_envelope(&snapshot),
    ))
    .map_err(|error| format!("failed to encode attach payload: {error}"))?;
    let socket = web_sys::WebSocket::new(&build_ws_url(&base_url))
        .map_err(|_| "failed to open session connection".to_string())?;

    let open_socket = socket.clone();
    let mut open_identity = identity;
    let mut open_ops = ops;
    let onopen = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if !is_current_generation(generation) {
            return;
        }
        if open_socket.send_with_str(&envelope_json).is_err() {
            open_identity.with_mut(|id| {
                id.connection_status = ConnectionStatus::Offline;
            });
            open_ops.with_mut(|o| {
                o.notice = Some(scoped_notice(
                    NoticeScope::Session,
                    error_notice("Could not sync the session."),
                ));
            });
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onopen(Some(onopen.as_ref().unchecked_ref()));

    let mut msg_identity = identity;
    let mut msg_game_state = game_state;
    let mut msg_ops = ops;
    let mut msg_judge_bundle = judge_bundle;
    let onmessage = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
        if !is_current_generation(generation) {
            return;
        }
        if let Some(text) = event.data().as_string() {
            match serde_json::from_str::<ServerWsMessage>(&text) {
                Ok(message) => {
                    msg_identity.with_mut(|id| {
                        msg_game_state.with_mut(|gs| {
                            msg_ops.with_mut(|o| {
                                msg_judge_bundle.with_mut(|jb| {
                                    apply_server_ws_message(id, gs, o, jb, message);
                                });
                            });
                        });
                    });
                }
                Err(_) => {
                    msg_identity.with_mut(|id| {
                        id.connection_status = ConnectionStatus::Offline;
                    });
                    msg_ops.with_mut(|o| {
                        o.notice = Some(scoped_notice(
                            NoticeScope::Session,
                            error_notice("Received an invalid session update."),
                        ));
                    });
                }
            }
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    let mut err_identity = identity;
    let mut err_ops = ops;
    let onerror = Closure::wrap(Box::new(move |_event: web_sys::ErrorEvent| {
        if !is_current_generation(generation) {
            return;
        }
        err_identity.with_mut(|id| {
            id.connection_status = ConnectionStatus::Offline;
        });
        err_ops.with_mut(|o| {
            o.notice = Some(scoped_notice(
                NoticeScope::Session,
                error_notice("Session connection failed."),
            ));
        });
    }) as Box<dyn FnMut(_)>);
    socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let mut close_identity = identity;
    let mut close_ops = ops;
    let onclose = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if !is_current_generation(generation) {
            return;
        }
        let should_announce_close =
            close_identity.read().connection_status != ConnectionStatus::Offline;
        close_identity.with_mut(|id| {
            id.connection_status = ConnectionStatus::Offline;
        });
        if should_announce_close {
            close_ops.with_mut(|o| {
                o.notice = Some(scoped_notice(
                    NoticeScope::Session,
                    crate::state::info_notice("Session connection closed."),
                ));
            });
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));

    REALTIME_CLIENT.with(|client| {
        client.borrow_mut().replace(RealtimeClientHandle {
            socket,
            _onopen: onopen,
            _onmessage: onmessage,
            _onerror: onerror,
            _onclose: onclose,
        });
    });

    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub fn disconnect_realtime() {}

#[cfg(not(target_arch = "wasm32"))]
pub fn bootstrap_realtime(
    mut identity: Signal<IdentityState>,
    _game_state: Signal<Option<ClientGameState>>,
    _ops: Signal<OperationState>,
    _judge_bundle: Signal<Option<JudgeBundle>>,
) -> Result<(), String> {
    identity.with_mut(|id| {
        id.realtime_bootstrap_attempted = true;
    });
    Ok(())
}
