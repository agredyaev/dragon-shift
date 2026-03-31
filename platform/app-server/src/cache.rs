use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

use chrono::Utc;
use domain::WorkshopSession;
use persistence::PersistenceError;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, sleep};
use uuid::Uuid;

use crate::app::AppState;

const SESSION_LEASE_TTL: Duration = Duration::from_secs(5);
const SESSION_LEASE_TIMEOUT: Duration = Duration::from_secs(2);
const SESSION_LEASE_RETRY_DELAY: Duration = Duration::from_millis(25);

fn restore_session(mut session: WorkshopSession) -> WorkshopSession {
    // Connection presence is runtime-only and must not survive process restarts.
    for player in session.players.values_mut() {
        player.is_connected = false;
    }
    session.ensure_host_assigned(false);
    session
}

async fn merge_runtime_presence(
    state: &AppState,
    session_code: &str,
    session: &mut WorkshopSession,
) {
    if let Ok(registrations) = state.store.list_realtime_connections(session_code).await {
        for registration in registrations {
            if let Some(player) = session.players.get_mut(&registration.player_id) {
                player.is_connected = true;
            }
        }
    }

    let has_connected_players = session.players.values().any(|player| player.is_connected);
    session.ensure_host_assigned(has_connected_players);
}

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
    let mut session = restore_session(session);
    merge_runtime_presence(state, session_code, &mut session).await;

    let mut sessions = state.sessions.lock().await;
    sessions.entry(session.code.0.clone()).or_insert(session);
    Ok(true)
}

pub(crate) async fn reload_cached_session(
    state: &AppState,
    session_code: &str,
) -> Result<bool, String> {
    let Some(session) = state
        .store
        .load_session_by_code(session_code)
        .await
        .map_err(|error| format!("failed to load session: {error}"))?
    else {
        state.sessions.lock().await.remove(session_code);
        return Ok(false);
    };

    let mut session = restore_session(session);
    merge_runtime_presence(state, session_code, &mut session).await;

    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session);
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

pub(crate) struct SessionWriteLease {
    store: Arc<dyn persistence::SessionStore>,
    session_code: String,
    lease_id: String,
    is_active: Arc<AtomicBool>,
    renewal_task: Option<JoinHandle<()>>,
}

impl SessionWriteLease {
    pub(crate) async fn acquire(
        state: &AppState,
        session_code: &str,
    ) -> Result<(Arc<Mutex<()>>, tokio::sync::OwnedMutexGuard<()> , Self), PersistenceError> {
        let local_lock = session_write_lock(state, session_code).await;
        let local_guard = local_lock.clone().lock_owned().await;
        let lease_id = format!("{}:{}", state.replica_id, Uuid::new_v4());
        let deadline = Instant::now() + SESSION_LEASE_TIMEOUT;

        loop {
            let expires_at = (chrono::Utc::now() + chrono::Duration::from_std(SESSION_LEASE_TTL).expect("lease ttl"))
                .to_rfc3339();
            if state
                .store
                .acquire_session_lease(session_code, &lease_id, &expires_at)
                .await?
            {
                let is_active = Arc::new(AtomicBool::new(true));
                let renewal_task = spawn_session_lease_renewal(
                    state.store.clone(),
                    session_code.to_string(),
                    lease_id.clone(),
                    is_active.clone(),
                );
                return Ok((
                    local_lock,
                    local_guard,
                    Self {
                        store: state.store.clone(),
                        session_code: session_code.to_string(),
                        lease_id,
                        is_active,
                        renewal_task: Some(renewal_task),
                    },
                ));
            }

            if Instant::now() >= deadline {
                return Err(PersistenceError::SessionLeaseTimeout {
                    session_code: session_code.to_string(),
                });
            }
            sleep(SESSION_LEASE_RETRY_DELAY).await;
        }
    }

    pub(crate) fn ensure_active(&self) -> Result<(), PersistenceError> {
        if self.is_active.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(PersistenceError::SessionLeaseTimeout {
                session_code: self.session_code.clone(),
            })
        }
    }
}

impl Drop for SessionWriteLease {
    fn drop(&mut self) {
        self.is_active.store(false, Ordering::SeqCst);
        if let Some(renewal_task) = self.renewal_task.take() {
            renewal_task.abort();
        }
        let store = self.store.clone();
        let session_code = self.session_code.clone();
        let lease_id = self.lease_id.clone();
        tokio::spawn(async move {
            let _ = store.release_session_lease(&session_code, &lease_id).await;
        });
    }
}

fn spawn_session_lease_renewal(
    store: Arc<dyn persistence::SessionStore>,
    session_code: String,
    lease_id: String,
    is_active: Arc<AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let renew_interval = SESSION_LEASE_TTL / 2;
        loop {
            sleep(renew_interval).await;
            let expires_at = (Utc::now()
                + chrono::Duration::from_std(SESSION_LEASE_TTL).expect("lease ttl"))
            .to_rfc3339();
            match store
                .renew_session_lease(&session_code, &lease_id, &expires_at)
                .await
            {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    is_active.store(false, Ordering::SeqCst);
                    break;
                }
            }
        }
    })
}
