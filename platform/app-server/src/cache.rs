use std::sync::Arc;

use tokio::sync::Mutex;

use crate::app::AppState;

pub(crate) async fn ensure_session_cached(
    state: &AppState,
    session_code: &str,
) -> Result<bool, String> {
    {
        let sessions = state.sessions.lock().await;
        if sessions.contains_key(session_code) {
            return Ok(true);
        }
    }

    let load_lock = session_cache_load_lock(state, session_code).await;
    let _load_guard = load_lock.lock().await;

    {
        let sessions = state.sessions.lock().await;
        if sessions.contains_key(session_code) {
            return Ok(true);
        }
    }

    let Some(session) = state
        .store
        .load_session_by_code(session_code)
        .await
        .map_err(|error| format!("failed to load session: {error}"))?
    else {
        return Ok(false);
    };

    let mut sessions = state.sessions.lock().await;
    sessions.entry(session.code.0.clone()).or_insert(session);
    Ok(true)
}

pub(crate) async fn session_cache_load_lock(
    state: &AppState,
    session_code: &str,
) -> Arc<Mutex<()>> {
    let mut locks = state.session_cache_load_locks.lock().await;
    locks
        .entry(session_code.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub(crate) async fn session_write_lock(state: &AppState, session_code: &str) -> Arc<Mutex<()>> {
    let mut locks = state.session_write_locks.lock().await;
    locks
        .entry(session_code.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}
