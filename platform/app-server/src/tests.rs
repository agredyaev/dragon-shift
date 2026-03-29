use crate::{
    app::{AppConfig, AppState, build_app},
    cache::ensure_session_cached,
    helpers::{build_judge_action_traces, to_client_game_state},
    http::allocate_session_code,
    ws::emit_phase_warning_notices,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderValue, Request, StatusCode},
};
use chrono::Utc;
use domain::{SessionCode, SessionPlayer, WorkshopSession};
use futures_util::{SinkExt, StreamExt};
use persistence::{InMemorySessionStore, PersistenceError, PlayerIdentityMatch, SessionStore};
use protocol::{
    ClientWsMessage, CoordinatorType, DragonStats, NoticeLevel, ServerWsMessage,
    SessionArtifactKind, SessionArtifactRecord, SessionCommand, SessionEnvelope,
    WorkshopCommandRequest, WorkshopCommandResult, WorkshopJoinResult, WorkshopJudgeBundleResult,
};
use security::{DEFAULT_RUST_SESSION_CODE_PREFIX, OriginPolicyOptions, create_origin_policy};
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as WsMessage, client::IntoClientRequest},
};
use tower::util::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct FaultyStore {
    inner: InMemorySessionStore,
    fail_load_session_by_code: AtomicBool,
    fail_save_session: AtomicBool,
    fail_touch_player_identity: AtomicBool,
    fail_create_player_identity: AtomicBool,
    fail_append_session_artifact: AtomicBool,
    load_session_by_code_calls: AtomicUsize,
}

impl FaultyStore {
    fn new() -> Self {
        Self::default()
    }

    fn fail_loads(&self) {
        self.fail_load_session_by_code.store(true, Ordering::SeqCst);
    }

    fn fail_saves(&self) {
        self.fail_save_session.store(true, Ordering::SeqCst);
    }

    fn fail_touches(&self) {
        self.fail_touch_player_identity
            .store(true, Ordering::SeqCst);
    }

    fn fail_identity_creates(&self) {
        self.fail_create_player_identity
            .store(true, Ordering::SeqCst);
    }

    fn fail_artifact_appends(&self) {
        self.fail_append_session_artifact
            .store(true, Ordering::SeqCst);
    }

    fn load_calls(&self) -> usize {
        self.load_session_by_code_calls.load(Ordering::SeqCst)
    }
}

impl SessionStore for FaultyStore {
    fn init(&self) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.init()
    }

    fn health_check(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PersistenceError>> + Send + '_>> {
        self.inner.health_check()
    }

    fn load_session_by_code(
        &self,
        session_code: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<WorkshopSession>, PersistenceError>> + Send + '_>>
    {
        self.load_session_by_code_calls
            .fetch_add(1, Ordering::SeqCst);
        if self.fail_load_session_by_code.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.load_session_by_code(session_code)
    }

    fn save_session(
        &self,
        session: &WorkshopSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self.fail_save_session.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.save_session(session)
    }

    fn append_session_artifact(
        &self,
        artifact: &SessionArtifactRecord,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self.fail_append_session_artifact.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.append_session_artifact(artifact)
    }

    fn list_session_artifacts(
        &self,
        session_id: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<SessionArtifactRecord>, PersistenceError>> + Send + '_>,
    > {
        self.inner.list_session_artifacts(session_id)
    }

    fn create_player_identity(
        &self,
        identity: &persistence::PlayerIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self.fail_create_player_identity.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner.create_player_identity(identity)
    }

    fn find_player_identity(
        &self,
        session_code: &str,
        reconnect_token: &str,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<PlayerIdentityMatch>, PersistenceError>> + Send + '_>,
    > {
        self.inner
            .find_player_identity(session_code, reconnect_token)
    }

    fn touch_player_identity(
        &self,
        reconnect_token: &str,
        last_seen_at: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        if self.fail_touch_player_identity.load(Ordering::SeqCst) {
            return Box::pin(async { Err(PersistenceError::LockPoisoned) });
        }
        self.inner
            .touch_player_identity(reconnect_token, last_seen_at)
    }

    fn revoke_player_identity(
        &self,
        reconnect_token: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), PersistenceError>> + Send + '_>> {
        self.inner.revoke_player_identity(reconnect_token)
    }
}

fn session_player(id: &str, name: &str, joined_at_seconds: i64) -> SessionPlayer {
    SessionPlayer {
        id: id.to_string(),
        name: name.to_string(),
        pet_description: Some(format!("{name}'s workshop dragon")),
        is_host: false,
        is_connected: true,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: chrono::DateTime::from_timestamp(joined_at_seconds, 0).expect("valid timestamp"),
    }
}

fn create_workshop_body(name: &str) -> String {
    serde_json::json!({
        "name": name,
        "config": {
            "phase0Minutes": 5,
            "phase1Minutes": 10,
            "phase2Minutes": 10
        }
    })
    .to_string()
}

fn test_state() -> AppState {
    test_state_with_limits(20, 40)
}

fn test_state_with_limits(create_limit: u32, join_limit: u32) -> AppState {
    let config = Arc::new(AppConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 4100)),
        is_production: false,
        rust_session_code_prefix: DEFAULT_RUST_SESSION_CODE_PREFIX.to_string(),
        origin_policy: create_origin_policy(OriginPolicyOptions {
            allowed_origins: Some("http://localhost:5173"),
            app_origin: None,
            is_production: false,
        })
        .expect("create origin policy"),
        static_assets_dir: std::env::temp_dir().join("dragon-shift-test-static-missing"),
        database_url: None,
        persistence_backend: "memory".to_string(),
    });

    AppState::new(
        config,
        Arc::new(InMemorySessionStore::new()),
        create_limit,
        join_limit,
    )
}

async fn spawn_test_server(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test app");
    });
    (addr, handle)
}

fn ws_request(addr: SocketAddr) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = format!("ws://{addr}/api/workshops/ws")
        .into_client_request()
        .expect("ws client request");
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_static("http://localhost:5173"));
    request
}

fn masked_ws_text_frame(payload: &str) -> Vec<u8> {
    let payload = payload.as_bytes();
    let mask = [0x12_u8, 0x34, 0x56, 0x78];
    let mut frame = Vec::with_capacity(payload.len() + 8);
    frame.push(0x81);

    let len = payload.len();
    if len <= 125 {
        frame.push(0x80 | len as u8);
    } else if u16::try_from(len).is_ok() {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        panic!("test websocket payload is unexpectedly large");
    }

    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    frame
}

async fn connect_raw_ws(addr: SocketAddr) -> tokio::net::TcpStream {
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect raw ws tcp stream");
    let request = format!(
        "GET /api/workshops/ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: http://localhost:5173\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write websocket upgrade request");

    let mut response = Vec::new();
    let mut chunk = [0_u8; 256];
    loop {
        let bytes_read = stream
            .read(&mut chunk)
            .await
            .expect("read websocket upgrade response");
        assert!(bytes_read > 0, "websocket upgrade response ended early");
        response.extend_from_slice(&chunk[..bytes_read]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    stream
}

async fn send_raw_ws_message(stream: &mut tokio::net::TcpStream, message: &ClientWsMessage) {
    let frame =
        masked_ws_text_frame(&serde_json::to_string(message).expect("encode websocket message"));
    stream
        .write_all(&frame)
        .await
        .expect("write websocket frame");
    stream.flush().await.expect("flush websocket frame");
}

async fn send_attach_and_reset_connection(addr: SocketAddr, message: &ClientWsMessage) {
    let mut stream = connect_raw_ws(addr).await;
    send_raw_ws_message(&mut stream, message).await;
    let _ = stream.shutdown().await;
    drop(stream);
}

fn test_state_with_static_assets() -> AppState {
    let static_assets_dir =
        std::env::temp_dir().join(format!("dragon-shift-test-static-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&static_assets_dir).expect("create static assets dir");
    std::fs::write(
        static_assets_dir.join("index.html"),
        "<!doctype html><html><body><div id=\"root\">dragon shift</div></body></html>",
    )
    .expect("write static index");

    let mut state = test_state();
    state.config = Arc::new(AppConfig {
        static_assets_dir,
        ..state.config.as_ref().clone()
    });
    state
}

#[tokio::test]
async fn live_endpoint_returns_ok() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/live")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call live endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read live body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse live json");
    assert_eq!(json["status"], "live");
    assert_eq!(json["ok"], true);
}

#[tokio::test]
async fn join_workshop_returns_internal_error_when_cache_load_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());

    let session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        Utc::now(),
        protocol::WorkshopCreateConfig::default(),
    );
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store.fail_loads();

    let app = build_app(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sessionCode":"123456","name":"Bob"}"#))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => assert!(error.error.contains("failed to load session")),
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_does_not_leave_mutated_cache_when_save_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    let host_player = SessionPlayer {
        id: player_id.clone(),
        name: "Alice".to_string(),
        pet_description: Some("Alice's workshop dragon".to_string()),
        is_host: true,
        is_connected: true,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    };
    session.add_player(host_player);
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store
        .inner
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    store.fail_saves();

    let app = build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: session_code.to_string(),
                        reconnect_token: reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode command request"),
                ))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    assert_eq!(cached.phase, protocol::Phase::Lobby);
}

#[tokio::test]
async fn root_path_serves_static_index_when_bundle_exists() {
    let app = build_app(test_state_with_static_assets());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("call root path");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read root body");
    let html = String::from_utf8(body.to_vec()).expect("decode html");
    assert!(html.contains("dragon shift"));
}

#[tokio::test]
async fn workshop_ws_attach_sends_current_state_for_valid_identity() {
    let state = test_state();
    let app = build_app(state.clone());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("state update frame")
        .expect("state update message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::StateUpdate(client_state) => {
            assert_eq!(client_state.session.code, create_success.session_code);
            assert_eq!(
                client_state.current_player_id.as_deref(),
                Some(create_success.player_id.as_str())
            );
        }
        other => panic!("expected state update, got {other:?}"),
    }
    assert_eq!(
        state
            .realtime
            .lock()
            .await
            .session_connection_count(&create_success.session_code),
        1
    );

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn ensure_session_cached_deduplicates_concurrent_loads() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        Utc::now(),
        protocol::WorkshopCreateConfig::default(),
    );
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");

    let first = ensure_session_cached(&state, "123456");
    let second = ensure_session_cached(&state, "123456");
    let (first_result, second_result) = tokio::join!(first, second);

    assert_eq!(first_result.expect("first load"), true);
    assert_eq!(second_result.expect("second load"), true);
    assert_eq!(store.load_calls(), 1);
}

#[tokio::test]
async fn workshop_command_pushes_state_update_to_attached_websocket() {
    let state = test_state();
    let app = build_app(state.clone());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app.clone()).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let _ = socket
        .next()
        .await
        .expect("initial state update frame")
        .expect("initial state update message");

    let command_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode command request"),
                ))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");
    assert_eq!(command_response.status(), StatusCode::OK);

    let message = socket
        .next()
        .await
        .expect("broadcast state update frame")
        .expect("broadcast state update message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::StateUpdate(client_state) => {
            assert_eq!(client_state.phase, protocol::Phase::Phase1);
            assert_eq!(client_state.session.code, create_success.session_code);
        }
        other => panic!("expected state update, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn allocate_session_code_treats_store_errors_as_unavailable() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    store.fail_loads();

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        allocate_session_code(&state),
    )
    .await;

    assert!(
        result.is_err(),
        "allocation should keep retrying while store reads fail"
    );
}

fn test_state_with_store(store: Arc<dyn SessionStore>) -> AppState {
    let mut state = test_state();
    state.store = store;
    state
}

#[tokio::test]
async fn workshop_judge_bundle_returns_bundle_for_completed_session() {
    let state = test_state();
    let app = build_app(state.clone());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let alice_dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("alice dragon id");
    let bob_dragon_id = session
        .players
        .get(&join_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("bob dragon id");
    let session_id = session.id.to_string();
    drop(sessions);

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, create_success.reconnect_token, bob_dragon_id
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, join_success.reconnect_token, alice_dragon_id
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build vote request"),
            )
            .await
            .expect("call submitVote command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let reveal_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build reveal results request"),
             )
             .await
             .expect("call revealVotingResults command");
    assert_eq!(reveal_response.status(), StatusCode::OK);

    state
        .store
        .append_session_artifact(&SessionArtifactRecord {
            id: "artifact-action-1".into(),
            session_id: session_id.clone(),
            phase: protocol::Phase::Phase2,
            step: 2,
            kind: SessionArtifactKind::ActionProcessed,
            player_id: Some(join_success.player_id.clone()),
            created_at: "2026-01-01T00:00:00Z".into(),
            payload: serde_json::json!({
                "dragonId": alice_dragon_id,
                "actionType": "feed",
                "actionValue": "meat",
                "hunger": 88,
                "energy": 100,
                "happiness": 97
            }),
        })
        .await
        .expect("append action artifact");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/judge-bundle")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build judge bundle request"),
        )
        .await
        .expect("call judge bundle endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read judge bundle body");
    let result: WorkshopJudgeBundleResult =
        serde_json::from_slice(&body).expect("parse judge bundle result");
    let success = match result {
        WorkshopJudgeBundleResult::Success(success) => success,
        WorkshopJudgeBundleResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    };

    assert!(success.ok);
    assert_eq!(success.bundle.session_code, session_code);
    assert_eq!(success.bundle.current_phase, protocol::Phase::End);
    assert_eq!(success.bundle.players.len(), 2);
    assert_eq!(success.bundle.dragons.len(), 2);
    let judged_dragon = success
        .bundle
        .dragons
        .iter()
        .find(|dragon| dragon.dragon_id == alice_dragon_id)
        .expect("judged dragon bundle");
    assert_eq!(judged_dragon.creative_vote_count, 1);
    assert_eq!(
        judged_dragon.handover_chain.discovery_observations,
        Vec::<String>::new()
    );
    assert_eq!(judged_dragon.phase2_actions.len(), 1);
    assert_eq!(judged_dragon.phase2_actions[0].player_name, "Bob");
    assert_eq!(judged_dragon.phase2_actions[0].action_type, "feed");
}

#[tokio::test]
async fn create_workshop_endpoint_returns_join_success() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Success(success) => {
            assert!(success.ok);
            assert_eq!(success.coordinator_type, CoordinatorType::Rust);
            assert_eq!(
                success.state.current_player_id.as_deref(),
                Some(success.player_id.as_str())
            );
            assert_eq!(success.state.players.len(), 1);
            let host = success
                .state
                .players
                .get(&success.player_id)
                .expect("host player in state");
            assert_eq!(
                host.pet_description.as_deref(),
                Some("Alice's workshop dragon")
            );
        }
        WorkshopJoinResult::Error(error) => panic!("expected success, got error: {}", error.error),
    }
}

#[tokio::test]
async fn workshop_judge_bundle_rejects_invalid_credentials() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let session_code = match create_result {
        WorkshopJoinResult::Success(success) => success.session_code,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/judge-bundle")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"missing"}}"#,
                    session_code
                )))
                .expect("build request"),
        )
        .await
        .expect("call judge bundle endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read judge bundle body");
    let result: WorkshopJudgeBundleResult =
        serde_json::from_slice(&body).expect("parse judge bundle result");
    match result {
        WorkshopJudgeBundleResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopJudgeBundleResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn create_workshop_endpoint_rejects_empty_host_name() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("   ")))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read error body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Please enter a host name."),
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn create_workshop_endpoint_rejects_forbidden_origin() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header("origin", "https://evil.example.com")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read error body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Origin is not allowed."),
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn create_workshop_endpoint_is_rate_limited_for_repeated_requests() {
    let app = build_app(test_state_with_limits(1, 40));

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build first request"),
        )
        .await
        .expect("call first create workshop");
    assert_eq!(first.status(), StatusCode::OK);

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(create_workshop_body("Bob")))
                .expect("build second request"),
        )
        .await
        .expect("call second create workshop");

    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = to_bytes(second.into_body(), usize::MAX)
        .await
        .expect("read rate limited body");
    let result: WorkshopJoinResult =
        serde_json::from_slice(&body).expect("parse rate limited result");
    match result {
        WorkshopJoinResult::Error(error) => {
            assert_eq!(
                error.error,
                "Too many requests. Please slow down and try again."
            );
        }
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn join_workshop_endpoint_rejects_invalid_code() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sessionCode":"12ab56","name":"Bob"}"#))
                .expect("build request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => {
            assert_eq!(error.error, "Workshop codes must be 6 digits.")
        }
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn join_workshop_endpoint_rejects_missing_session() {
    let app = build_app(test_state());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"sessionCode":"123456","name":"Bob"}"#))
                .expect("build request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Error(error) => assert_eq!(error.error, "Workshop not found."),
        WorkshopJoinResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn join_workshop_endpoint_returns_join_success_for_lobby_session() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let session_code = match create_result {
        WorkshopJoinResult::Success(success) => success.session_code,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse join result");
    match result {
        WorkshopJoinResult::Success(success) => {
            assert!(success.ok);
            assert_eq!(success.coordinator_type, CoordinatorType::Rust);
            assert_eq!(success.state.players.len(), 2);
            assert_eq!(
                success.state.current_player_id.as_deref(),
                Some(success.player_id.as_str())
            );
            let joined = success
                .state
                .players
                .get(&success.player_id)
                .expect("joined player in state");
            assert_eq!(
                joined.pet_description.as_deref(),
                Some("Bob's workshop dragon")
            );
        }
        WorkshopJoinResult::Error(error) => panic!("expected success, got error: {}", error.error),
    }
}

#[tokio::test]
async fn join_workshop_endpoint_reconnects_existing_player_without_name() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call start phase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(&create_success.session_code)
            .expect("session exists");
        let player = session
            .players
            .get_mut(&create_success.player_id)
            .expect("player exists");
        player.is_connected = false;
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build reconnect request"),
        )
        .await
        .expect("call reconnect join workshop");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read reconnect body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse reconnect result");
    match result {
        WorkshopJoinResult::Success(success) => {
            assert!(success.ok);
            assert_eq!(success.player_id, create_success.player_id);
            assert_eq!(success.reconnect_token, create_success.reconnect_token);
            assert_eq!(success.state.phase, protocol::Phase::Phase1);
            assert_eq!(
                success.state.current_player_id.as_deref(),
                Some(create_success.player_id.as_str())
            );
            let player = success
                .state
                .players
                .get(&create_success.player_id)
                .expect("reconnected player in state");
            assert!(player.is_connected);
            assert!(player.current_dragon_id.is_some());
        }
        WorkshopJoinResult::Error(error) => {
            panic!("expected reconnect success, got error: {}", error.error)
        }
    }
}

#[tokio::test]
async fn join_workshop_endpoint_rejects_invalid_reconnect_token() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let session_code = match create_result {
        WorkshopJoinResult::Success(success) => success.session_code,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"missing"}}"#,
                    session_code
                )))
                .expect("build reconnect request"),
        )
        .await
        .expect("call reconnect join workshop");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read reconnect body");
    let result: WorkshopJoinResult = serde_json::from_slice(&body).expect("parse reconnect result");
    match result {
        WorkshopJoinResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopJoinResult::Success(_) => panic!("expected reconnect error response"),
    }
}

#[tokio::test]
async fn workshop_command_saves_observation_during_phase1() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call start phase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(
                         r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitObservation","payload":{{"text":"Calms down at dusk"}}}}"#,
                         create_success.session_code, create_success.reconnect_token
                     )))
                     .expect("build observation request"),
             )
             .await
             .expect("call observation command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read observation body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse observation result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected observation success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    let dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.as_ref())
        .expect("current dragon id");
    let dragon = session.dragons.get(dragon_id).expect("dragon exists");
    assert_eq!(
        dragon.discovery_observations,
        vec!["Calms down at dusk".to_string()]
    );
}

#[tokio::test]
async fn workshop_command_records_action_artifact_during_phase2() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    create_success.session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
            create_success.session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            create_success.session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["Rule 1","Rule 2","Rule 3"]}}"#,
            create_success.session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["Rule A","Rule B","Rule C"]}}"#,
            create_success.session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            create_success.session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(
                         r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"action","payload":{{"type":"sleep"}}}}"#,
                         create_success.session_code, create_success.reconnect_token
                     )))
                     .expect("build action request"),
             )
             .await
             .expect("call action command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read action body");
    let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse action result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected action success, got error: {}", error.error)
        }
    }

    let artifacts = state
        .store
        .list_session_artifacts(&create_success.state.session.id)
        .await
        .expect("list artifacts");
    let action_artifact = artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.kind == SessionArtifactKind::ActionProcessed)
        .expect("action artifact exists");
    assert_eq!(action_artifact.phase, protocol::Phase::Phase2);
    assert_eq!(
        action_artifact
            .payload
            .get("actionType")
            .and_then(|value: &serde_json::Value| value.as_str()),
        Some("sleep")
    );
    assert!(
        action_artifact
            .payload
            .get("dragonId")
            .and_then(|value: &serde_json::Value| value.as_str())
            .is_some()
    );
}

#[tokio::test]
async fn workshop_command_rejects_invalid_credentials() {
    let app = build_app(test_state());

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(r#"{"sessionCode":"123456","reconnectToken":"missing","command":"startPhase1"}"#))
                     .expect("build request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Session identity is invalid or expired.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_start_phase1() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can start the workshop.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_starts_phase1_for_single_player_host() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Phase1);
    assert_eq!(session.dragons.len(), 1);
}

#[tokio::test]
async fn workshop_command_rejects_start_handover_outside_phase1() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Handover can only begin during Phase 1.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_start_handover() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let host_start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(host_start_phase1_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can trigger handover.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_starts_handover_for_host_in_phase1() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build handover request"),
        )
        .await
        .expect("call handover command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Handover);
}

#[tokio::test]
async fn workshop_command_rejects_submit_tags_outside_handover() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["a","b","c"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(
                error.error,
                "Handover notes can only be saved during handover."
            );
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_invalid_submit_tags_payload() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":{{"tags":["a"]}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Handover notes must be sent as a list.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_saves_submit_tags_in_handover_phase() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["first","second","third","fourth"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    let dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("current dragon id");
    let dragon = session.dragons.get(&dragon_id).expect("dragon exists");
    assert_eq!(dragon.handover_tags, vec!["first", "second", "third"]);
}

#[tokio::test]
async fn workshop_command_rejects_start_phase2_outside_handover() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Phase 2 can only begin from handover.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_start_phase2() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can begin Phase 2.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_start_phase2_when_tags_are_missing() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Still waiting on: Alice.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_starts_phase2_when_handover_is_complete() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let submit_tags_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["first","second","third"]}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call submitTags command");
    assert_eq!(submit_tags_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Phase2);
}

#[tokio::test]
async fn workshop_command_rejects_end_game_outside_phase2() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Voting can only begin from Phase 2.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_end_game() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let start_phase1_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build start phase1 request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_phase1_response.status(), StatusCode::OK);

    let start_handover_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build start handover request"),
        )
        .await
        .expect("call startHandover command");
    assert_eq!(start_handover_response.status(), StatusCode::OK);

    let submit_tags_host_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#, session_code, create_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call host submitTags command");
    assert_eq!(submit_tags_host_response.status(), StatusCode::OK);

    let submit_tags_join_response = app
             .clone()
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#, session_code, join_success.reconnect_token)))
                     .expect("build submit tags request"),
             )
             .await
             .expect("call join submitTags command");
    assert_eq!(submit_tags_join_response.status(), StatusCode::OK);

    let start_phase2_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build start phase2 request"),
        )
        .await
        .expect("call startPhase2 command");
    assert_eq!(start_phase2_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can end the workshop.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_starts_voting_when_host_ends_multiplayer_phase2() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build command request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
                    session_code, create_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call endGame command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Voting);
    assert!(session.voting.is_some());
}

#[tokio::test]
async fn workshop_command_rejects_submit_vote_outside_voting() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"dragon-a"}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Voting is not active right now.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_invalid_submit_vote_payload() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragon":"dragon-a"}}}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Voting is not active right now.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_self_vote_in_voting() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let bob_dragon_id = session
        .players
        .get(&join_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("bob dragon id");
    drop(sessions);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, join_success.reconnect_token, bob_dragon_id)))
                     .expect("build command request"),
             )
             .await
             .expect("call submitVote command");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "You cannot vote for your own dragon.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_accepts_valid_vote_in_voting() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let bob_dragon_id = session
        .players
        .get(&join_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("bob dragon id");
    drop(sessions);

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#, session_code, create_success.reconnect_token, bob_dragon_id)))
                     .expect("build command request"),
             )
             .await
             .expect("call submitVote command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }
}

#[tokio::test]
async fn workshop_command_rejects_reveal_results_outside_voting() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, create_success.session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Results can only be revealed during voting.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_non_host_reveal_results() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, join_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can reveal voting results.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_rejects_reveal_results_while_votes_are_pending() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Wait until every eligible player has voted.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_reveals_voting_results_after_all_votes() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let session_code = create_success.session_code.clone();

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startHandover"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["one","two","three"]}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitTags","payload":["four","five","six"]}}"#,
            session_code, join_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase2"}}"#,
            session_code, create_success.reconnect_token
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"endGame"}}"#,
            session_code, create_success.reconnect_token
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build setup request"),
            )
            .await
            .expect("call setup command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    let alice_dragon_id = session
        .players
        .get(&create_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("alice dragon id");
    let bob_dragon_id = session
        .players
        .get(&join_success.player_id)
        .and_then(|player| player.current_dragon_id.clone())
        .expect("bob dragon id");
    drop(sessions);

    for request_body in [
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, create_success.reconnect_token, bob_dragon_id
        ),
        format!(
            r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"submitVote","payload":{{"dragonId":"{}"}}}}"#,
            session_code, join_success.reconnect_token, alice_dragon_id
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workshops/command")
                    .header("content-type", "application/json")
                    .body(Body::from(request_body))
                    .expect("build vote request"),
            )
            .await
            .expect("call submitVote command");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
             .oneshot(
                 Request::builder()
                     .method("POST")
                     .uri("/api/workshops/command")
                     .header("content-type", "application/json")
                     .body(Body::from(format!(r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"revealVotingResults"}}"#, session_code, create_success.reconnect_token)))
                     .expect("build command request"),
             )
             .await
             .expect("call revealVotingResults command");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_code).expect("session exists");
    assert_eq!(session.phase, protocol::Phase::End);
    assert!(session.players.values().all(|player| player.score >= 0));
}

#[tokio::test]
async fn workshop_command_rejects_non_host_reset_game() {
    let app = build_app(test_state());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let session_code = match create_result {
        WorkshopJoinResult::Success(success) => success.session_code,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };
    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"resetGame"}}"#,
                    session_code, join_success.reconnect_token
                )))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read command body");
    let result: WorkshopCommandResult =
        serde_json::from_slice(&body).expect("parse command result");
    match result {
        WorkshopCommandResult::Error(error) => {
            assert_eq!(error.error, "Only the host can reset the workshop.");
        }
        WorkshopCommandResult::Success(_) => panic!("expected error response"),
    }
}

#[tokio::test]
async fn workshop_command_reset_game_returns_session_to_lobby() {
    let state = test_state();
    let app = build_app(state.clone());
    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let start_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"startPhase1"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build start request"),
        )
        .await
        .expect("call startPhase1 command");
    assert_eq!(start_response.status(), StatusCode::OK);

    let reset_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}","command":"resetGame"}}"#,
                    create_success.session_code, create_success.reconnect_token
                )))
                .expect("build reset request"),
        )
        .await
        .expect("call reset command");

    assert_eq!(reset_response.status(), StatusCode::OK);
    let body = to_bytes(reset_response.into_body(), usize::MAX)
        .await
        .expect("read reset body");
    let result: WorkshopCommandResult = serde_json::from_slice(&body).expect("parse reset result");
    match result {
        WorkshopCommandResult::Success(success) => assert!(success.ok),
        WorkshopCommandResult::Error(error) => {
            panic!("expected success, got error: {}", error.error)
        }
    }

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&create_success.session_code)
        .expect("session exists");
    assert_eq!(session.phase, protocol::Phase::Lobby);
    assert!(session.dragons.is_empty());
    assert!(
        session
            .players
            .values()
            .all(|player| player.current_dragon_id.is_none())
    );
}

#[tokio::test]
async fn workshop_ws_close_marks_player_offline_and_reassigns_host() {
    let state = test_state();
    let app = build_app(state.clone());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    create_success.session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");
    let join_body = to_bytes(join_response.into_body(), usize::MAX)
        .await
        .expect("read join body");
    let join_result: WorkshopJoinResult =
        serde_json::from_slice(&join_body).expect("parse join result");
    let join_success = match join_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected join success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let _ = socket
        .next()
        .await
        .expect("state update frame")
        .expect("state update message");
    assert_eq!(
        state
            .realtime
            .lock()
            .await
            .session_connection_count(&create_success.session_code),
        1
    );

    let _ = socket.close(None).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        state
            .realtime
            .lock()
            .await
            .session_connection_count(&create_success.session_code),
        0
    );

    let (session_id, host_player, guest_player) = {
        let sessions = state.sessions.lock().await;
        let session = sessions
            .get(&create_success.session_code)
            .expect("session exists after disconnect");
        (
            session.id.to_string(),
            session
                .players
                .get(&create_success.player_id)
                .expect("host player exists")
                .clone(),
            session
                .players
                .get(&join_success.player_id)
                .expect("guest player exists")
                .clone(),
        )
    };

    assert!(!host_player.is_connected);
    assert!(!host_player.is_host);
    assert!(guest_player.is_host);
    assert!(guest_player.is_connected);

    let artifacts = state
        .store
        .list_session_artifacts(&session_id)
        .await
        .expect("list session artifacts");
    let disconnect_artifact = artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.kind == SessionArtifactKind::PlayerLeft)
        .expect("player left artifact");
    assert_eq!(
        disconnect_artifact.player_id.as_deref(),
        Some(create_success.player_id.as_str())
    );
    assert_eq!(
        disconnect_artifact
            .payload
            .get("sessionCode")
            .and_then(|value: &serde_json::Value| value.as_str()),
        Some(create_success.session_code.as_str())
    );

    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_rejects_invalid_identity() {
    let app = build_app(test_state());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: "123456".to_string(),
        player_id: "player-1".to_string(),
        reconnect_token: "missing".to_string(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => {
            assert_eq!(message, "Session identity is invalid or expired.");
        }
        other => panic!("expected error message, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_attach_keeps_cache_state_when_artifact_append_fails_after_save() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(SessionPlayer {
        id: player_id.clone(),
        name: "Alice".to_string(),
        pet_description: Some("Alice's workshop dragon".to_string()),
        is_host: true,
        is_connected: false,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    });
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store
        .inner
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    store.fail_artifact_appends();

    let app = build_app(state.clone());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: session_code.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => {
            assert!(message.contains("failed to append reconnect artifact"))
        }
        other => panic!("expected error message, got {other:?}"),
    }

    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    let player = cached.players.get(&player_id).expect("player exists");
    assert!(
        player.is_connected,
        "cache should keep the committed reconnect state"
    );

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_attach_restores_cache_state_when_save_fails() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(SessionPlayer {
        id: player_id.clone(),
        name: "Alice".to_string(),
        pet_description: Some("Alice's workshop dragon".to_string()),
        is_host: true,
        is_connected: false,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    });
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store
        .inner
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    store.fail_saves();

    let app = build_app(state.clone());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: session_code.to_string(),
        player_id: player_id.clone(),
        reconnect_token: reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");

    let message = socket
        .next()
        .await
        .expect("error frame")
        .expect("error message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Error { message } => assert!(message.contains("failed to save session")),
        other => panic!("expected error message, got {other:?}"),
    }

    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    let player = cached.players.get(&player_id).expect("player exists");
    assert!(
        !player.is_connected,
        "cache should roll back when reconnect save fails"
    );

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_attach_does_not_leave_registration_when_initial_state_send_fails() {
    let state = test_state();
    let app = build_app(state.clone());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let (addr, server_handle) = spawn_test_server(app).await;
    send_attach_and_reset_connection(
        addr,
        &ClientWsMessage::AttachSession(SessionEnvelope {
            session_code: create_success.session_code.clone(),
            player_id: create_success.player_id.clone(),
            reconnect_token: create_success.reconnect_token.clone(),
            coordinator_type: Some(CoordinatorType::Rust),
        }),
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        state
            .realtime
            .lock()
            .await
            .session_connection_count(&create_success.session_code),
        0,
        "failed initial state send should not leave a realtime registration behind"
    );

    server_handle.abort();
}

#[tokio::test]
async fn workshop_ws_failed_reattach_restores_replaced_registration() {
    let state = test_state();
    let app = build_app(state.clone());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });

    let (addr, server_handle) = spawn_test_server(app).await;
    let mut first_stream = connect_raw_ws(addr).await;
    send_raw_ws_message(&mut first_stream, &attach_message).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let first_connection_id = {
        let registrations = state
            .realtime
            .lock()
            .await
            .session_registrations(&create_success.session_code);
        assert_eq!(
            registrations.len(),
            1,
            "first attach should register the raw websocket connection"
        );
        registrations[0].connection_id.clone()
    };

    let (mut second_socket, _) = connect_async(ws_request(addr))
        .await
        .expect("connect replacement ws");
    second_socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send replacement attach");
    let _ = second_socket
        .next()
        .await
        .expect("replacement state frame")
        .expect("replacement state message");

    let replacement_connection_id = {
        let registrations = state
            .realtime
            .lock()
            .await
            .session_registrations(&create_success.session_code);
        assert_eq!(
            registrations.len(),
            1,
            "replacement attach should keep a single registration"
        );
        let connection_id = registrations[0].connection_id.clone();
        assert_ne!(
            connection_id, first_connection_id,
            "replacement attach should take over the player slot"
        );
        connection_id
    };

    send_raw_ws_message(&mut first_stream, &attach_message).await;
    let _ = first_stream.shutdown().await;
    drop(first_stream);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let registrations = state
        .realtime
        .lock()
        .await
        .session_registrations(&create_success.session_code);
    assert_eq!(
        registrations.len(),
        1,
        "failed re-attach should preserve the prior live registration"
    );
    assert_eq!(registrations[0].connection_id, replacement_connection_id);

    let _ = second_socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn reconnect_join_keeps_cache_state_when_touch_fails_after_save() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let player_id = "player-1".to_string();
    let reconnect_token = "token-1".to_string();
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(SessionPlayer {
        id: player_id.clone(),
        name: "Alice".to_string(),
        pet_description: Some("Alice's workshop dragon".to_string()),
        is_host: true,
        is_connected: false,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    });
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    store
        .inner
        .create_player_identity(&persistence::PlayerIdentity {
            session_id: session.id.to_string(),
            player_id: player_id.clone(),
            reconnect_token: reconnect_token.clone(),
            created_at: timestamp.to_rfc3339(),
            last_seen_at: timestamp.to_rfc3339(),
        })
        .await
        .expect("seed identity");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    store.fail_touches();

    let app = build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","reconnectToken":"{}"}}"#,
                    session_code, reconnect_token
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    let player = cached.players.get(&player_id).expect("player exists");
    assert!(
        player.is_connected,
        "cache should keep the committed reconnect state"
    );
}

#[tokio::test]
async fn fresh_join_keeps_cache_state_when_identity_create_fails_after_save() {
    let store = Arc::new(FaultyStore::new());
    let state = test_state_with_store(store.clone());
    let session_code = "123456";
    let timestamp = Utc::now();

    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode(session_code.into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    session.add_player(SessionPlayer {
        id: "host-1".to_string(),
        name: "Alice".to_string(),
        pet_description: Some("Alice's workshop dragon".to_string()),
        is_host: true,
        is_connected: true,
        is_ready: false,
        score: 0,
        current_dragon_id: None,
        achievements: Vec::new(),
        joined_at: timestamp,
    });
    store
        .inner
        .save_session(&session)
        .await
        .expect("seed session");
    state
        .sessions
        .lock()
        .await
        .insert(session_code.to_string(), session.clone());
    store.fail_identity_creates();

    let app = build_app(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/join")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"sessionCode":"{}","name":"Bob"}}"#,
                    session_code
                )))
                .expect("build join request"),
        )
        .await
        .expect("call join workshop");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let sessions = state.sessions.lock().await;
    let cached = sessions.get(session_code).expect("session remains cached");
    assert_eq!(
        cached.players.len(),
        2,
        "cache should keep the committed join state after save succeeds"
    );
}

#[tokio::test]
async fn workshop_ws_replies_with_pong_for_ping_message() {
    let app = build_app(test_state());
    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientWsMessage::Ping)
                .expect("encode ping")
                .into(),
        ))
        .await
        .expect("send ping message");

    let message = socket
        .next()
        .await
        .expect("pong frame")
        .expect("pong message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    assert_eq!(server_message, ServerWsMessage::Pong);

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[tokio::test]
async fn phase_timer_broadcasts_warning_notice_at_thirty_seconds_remaining() {
    let state = test_state();
    let app = build_app(state.clone());

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops")
                .header("content-type", "application/json")
                .body(Body::from(create_workshop_body("Alice")))
                .expect("build create request"),
        )
        .await
        .expect("call create workshop");
    let create_body = to_bytes(create_response.into_body(), usize::MAX)
        .await
        .expect("read create body");
    let create_result: WorkshopJoinResult =
        serde_json::from_slice(&create_body).expect("parse create result");
    let create_success = match create_result {
        WorkshopJoinResult::Success(success) => success,
        WorkshopJoinResult::Error(error) => {
            panic!("expected create success, got error: {}", error.error)
        }
    };

    let command_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/workshops/command")
                .header("origin", "http://localhost:5173")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&WorkshopCommandRequest {
                        session_code: create_success.session_code.clone(),
                        reconnect_token: create_success.reconnect_token.clone(),
                        coordinator_type: Some(CoordinatorType::Rust),
                        command: SessionCommand::StartPhase1,
                        payload: None,
                    })
                    .expect("encode command request"),
                ))
                .expect("build command request"),
        )
        .await
        .expect("call command endpoint");
    assert_eq!(command_response.status(), StatusCode::OK);

    let (addr, server_handle) = spawn_test_server(app).await;
    let (mut socket, _) = connect_async(ws_request(addr)).await.expect("connect ws");
    let attach_message = ClientWsMessage::AttachSession(SessionEnvelope {
        session_code: create_success.session_code.clone(),
        player_id: create_success.player_id.clone(),
        reconnect_token: create_success.reconnect_token.clone(),
        coordinator_type: Some(CoordinatorType::Rust),
    });
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&attach_message)
                .expect("encode attach")
                .into(),
        ))
        .await
        .expect("send attach");
    let _ = socket
        .next()
        .await
        .expect("initial state update frame")
        .expect("initial state update message");

    {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(&create_success.session_code)
            .expect("cached session");
        session.phase_started_at = Utc::now()
            - chrono::Duration::seconds((session.config.phase1_minutes as i64 * 60) - 30);
        session.warned_for_current_phase = false;
    }

    emit_phase_warning_notices(&state).await;

    let message = socket
        .next()
        .await
        .expect("warning notice frame")
        .expect("warning notice message");
    let payload = match message {
        WsMessage::Text(payload) => payload,
        other => panic!("expected text frame, got {other:?}"),
    };
    let server_message: ServerWsMessage =
        serde_json::from_str(&payload).expect("parse server ws message");
    match server_message {
        ServerWsMessage::Notice(notice) => {
            assert_eq!(notice.level, NoticeLevel::Warning);
            assert_eq!(notice.title, "Phase ending soon");
            assert!(notice.message.contains("Phase 1 ends in 30 seconds."));
        }
        other => panic!("expected warning notice, got {other:?}"),
    }

    let _ = socket.close(None).await;
    server_handle.abort();
}

#[test]
fn build_judge_action_traces_groups_action_artifacts_by_dragon() {
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp"),
        protocol::WorkshopCreateConfig {
            phase0_minutes: 5,
            phase1_minutes: 10,
            phase2_minutes: 10,
            image_generator_token: None,
            image_generator_model: None,
            judge_token: None,
            judge_model: None,
        },
    );
    session.add_player(session_player("p1", "Alice", 10));

    let artifacts = vec![
        SessionArtifactRecord {
            id: "artifact-1".into(),
            session_id: session.id.to_string(),
            phase: protocol::Phase::Phase2,
            step: 2,
            kind: SessionArtifactKind::ActionProcessed,
            player_id: Some("p1".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            payload: serde_json::json!({
                "dragonId": "dragon-a",
                "actionType": "feed",
                "actionValue": "meat",
                "hunger": 90,
                "energy": 100,
                "happiness": 95
            }),
        },
        SessionArtifactRecord {
            id: "artifact-2".into(),
            session_id: session.id.to_string(),
            phase: protocol::Phase::Phase2,
            step: 2,
            kind: SessionArtifactKind::ActionProcessed,
            player_id: Some("p1".into()),
            created_at: "2026-01-01T00:00:01Z".into(),
            payload: serde_json::json!({
                "dragonId": "dragon-a",
                "actionType": "play",
                "actionValue": "fetch"
            }),
        },
        SessionArtifactRecord {
            id: "artifact-3".into(),
            session_id: session.id.to_string(),
            phase: protocol::Phase::Phase2,
            step: 2,
            kind: SessionArtifactKind::ActionProcessed,
            player_id: Some("p1".into()),
            created_at: "2026-01-01T00:00:02Z".into(),
            payload: serde_json::json!({
                "dragonId": "dragon-b",
                "actionType": "sleep"
            }),
        },
    ];

    let traces = build_judge_action_traces(&session, &artifacts);

    let dragon_traces = traces.get("dragon-a").expect("dragon a traces");
    assert_eq!(dragon_traces.len(), 2);
    assert_eq!(dragon_traces[0].player_name, "Alice");
    assert_eq!(dragon_traces[0].action_type, "feed");
    assert_eq!(dragon_traces[0].action_value.as_deref(), Some("meat"));
    assert_eq!(
        dragon_traces[0].resulting_stats,
        Some(DragonStats {
            hunger: 90,
            energy: 100,
            happiness: 95
        })
    );
    assert_eq!(dragon_traces[1].action_type, "play");
    assert_eq!(dragon_traces[1].action_value.as_deref(), Some("fetch"));
    assert!(dragon_traces[1].resulting_stats.is_none());

    let second_dragon_traces = traces.get("dragon-b").expect("dragon b traces");
    assert_eq!(second_dragon_traces.len(), 1);
    assert_eq!(second_dragon_traces[0].action_type, "sleep");
}

#[test]
fn build_judge_action_traces_uses_unknown_fallbacks_for_missing_player_or_payload() {
    let session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp"),
        protocol::WorkshopCreateConfig::default(),
    );

    let artifacts = vec![SessionArtifactRecord {
        id: "artifact-1".into(),
        session_id: session.id.to_string(),
        phase: protocol::Phase::Phase2,
        step: 2,
        kind: SessionArtifactKind::ActionProcessed,
        player_id: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        payload: serde_json::json!({
            "dragonId": "dragon-a"
        }),
    }];

    let traces = build_judge_action_traces(&session, &artifacts);

    let dragon_traces = traces.get("dragon-a").expect("dragon a traces");
    assert_eq!(dragon_traces.len(), 1);
    assert_eq!(dragon_traces[0].player_id, "unknown");
    assert_eq!(dragon_traces[0].player_name, "Unknown");
    assert_eq!(dragon_traces[0].action_type, "unknown");
    assert_eq!(dragon_traces[0].action_value, None);
    assert_eq!(dragon_traces[0].resulting_stats, None);
}

#[test]
fn to_client_game_state_includes_dragons_and_voting_details() {
    let timestamp = chrono::DateTime::from_timestamp(1, 0).expect("valid timestamp");
    let mut session = WorkshopSession::new(
        Uuid::new_v4(),
        SessionCode("123456".into()),
        timestamp,
        protocol::WorkshopCreateConfig::default(),
    );
    let mut alice = session_player("p1", "Alice", 1);
    alice.is_host = true;
    let bob = session_player("p2", "Bob", 2);
    session.add_player(alice);
    session.add_player(bob);

    let assignments = vec![
        domain::Phase1Assignment {
            player_id: "p1".into(),
            dragon_id: "dragon-p1".into(),
        },
        domain::Phase1Assignment {
            player_id: "p2".into(),
            dragon_id: "dragon-p2".into(),
        },
    ];
    session.begin_phase1(&assignments).expect("begin phase1");
    session.record_discovery_observation("p1", "Calms down at dusk");
    session.record_discovery_observation("p2", "Rejects fruit at night");
    session.transition_to(protocol::Phase::Handover)
        .expect("enter handover");
    session.save_handover_tags("p1", vec!["Rule 1".into(), "Rule 2".into(), "Rule 3".into()]);
    session.save_handover_tags("p2", vec!["Rule A".into(), "Rule B".into(), "Rule C".into()]);
    session.enter_phase2().expect("enter phase2");
    session
        .apply_action("p1", domain::PlayerAction::Sleep)
        .expect("apply action");
    session.enter_voting().expect("enter voting");
    let bob_dragon_id = session
        .players
        .get("p2")
        .and_then(|player| player.current_dragon_id.clone())
        .expect("bob dragon");
    session
        .submit_vote("p1", &bob_dragon_id)
        .expect("submit vote");

    let client_state = to_client_game_state(&session, "p1");

    assert_eq!(client_state.phase, protocol::Phase::Voting);
    assert_eq!(client_state.dragons.len(), 2);
    let alice_current_dragon_id = client_state
        .players
        .get("p1")
        .and_then(|player| player.current_dragon_id.as_deref())
        .expect("alice current dragon")
        .to_string();
    let alice_current_dragon = client_state
        .dragons
        .get(&alice_current_dragon_id)
        .expect("alice current dragon details");
    assert!(!alice_current_dragon
        .condition_hint
        .as_deref()
        .unwrap_or_default()
        .is_empty());
    assert_eq!(alice_current_dragon.handover_tags.len(), 3);
    assert_eq!(alice_current_dragon.last_action, protocol::DragonAction::Idle);
    assert_eq!(alice_current_dragon.last_emotion, protocol::DragonEmotion::Angry);
    let original_dragon = client_state.dragons.get("dragon-p1").expect("original dragon");
    assert_eq!(original_dragon.discovery_observations.len(), 1);
    assert!(client_state
        .voting
        .as_ref()
        .is_some_and(|voting| voting.eligible_count == 2));
    assert!(client_state
        .voting
        .as_ref()
        .is_some_and(|voting| voting.submitted_count == 1));
    assert!(client_state
        .voting
        .as_ref()
        .is_some_and(|voting| voting.current_player_vote_dragon_id.as_deref() == Some(bob_dragon_id.as_str())));
}
