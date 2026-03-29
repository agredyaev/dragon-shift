mod app;
mod cache;
mod helpers;
mod http;
#[cfg(test)]
mod tests;
mod ws;

use std::sync::Arc;

use app::{AppState, build_app, build_session_store, load_config};
use tracing::info;
use ws::{broadcast_session_state, emit_phase_warning_notices};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Arc::new(load_config().expect("load app config"));
    let store = build_session_store(&config)
        .await
        .expect("build session store");
    store.init().await.expect("init session store");

    let state = AppState::new(config.clone(), store, 20, 40);

    if let Some(database_url) = state.config.database_url.as_deref() {
        let listener_state = state.clone();
        let database_url = database_url.to_string();
        tokio::spawn(async move {
            loop {
                match sqlx::postgres::PgListener::connect(&database_url).await {
                    Ok(mut listener) => {
                        if let Err(error) = listener.listen("session_updates").await {
                            tracing::warn!(%error, "PgListener failed to subscribe, retrying in 5s");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue;
                        }
                        loop {
                            match listener.recv().await {
                                Ok(notification) => {
                                    let session_code = notification.payload();
                                    listener_state.sessions.lock().await.remove(session_code);
                                    broadcast_session_state(&listener_state, session_code, None)
                                        .await;
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "PgListener recv error, reconnecting in 5s");
                                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "PgListener connect failed, retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });
    }

    let ticker_state = state.clone();
    let app = build_app(state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            emit_phase_warning_notices(&ticker_state).await;
        }
    });

    info!(bind_addr = %config.bind_addr, "starting platform app-server");

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .expect("bind listener");
    axum::serve(listener, app).await.expect("serve app");
}
