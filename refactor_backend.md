# Architecture Plan: Async Connection Pool and PostgreSQL Pub/Sub

## 1. Methodology: Test-Driven Development (TDD)
Implementation must follow a strict TDD approach. Tests for edge cases and race conditions must be written and observed failing before any application code is modified.

### 1.1. Edge Case Tests
*   **Database Disconnection:** Simulate a database connection drop during `save_session`. Verify that the connection pool recovers and subsequent requests succeed.
*   **Listener Disconnection:** Simulate a connection drop for the `PgListener`. Verify that the background task reconnects and resumes receiving `NOTIFY` events without missing subsequent broadcasts.
*   **Invalid Payloads:** Ensure `sqlx::types::Json` correctly rejects malformed JSON payloads at the database boundary.

### 1.2. Race Condition Tests
*   **Thundering Herd:** Simulate multiple pods receiving a `NOTIFY` simultaneously. Verify that cache invalidation does not result in redundant, concurrent database queries for the same session state.
*   **Simultaneous Updates:** Simulate concurrent HTTP requests mutating the same session. Verify that database transactions prevent lost updates and that `NOTIFY` events reflect the final committed state.
*   **Pub/Sub Ordering:** Verify that `NOTIFY` events are processed in the order they are committed, ensuring clients do not receive stale state updates after a newer state has been broadcast.

## 2. Dependency Management
Migrate from the synchronous `postgres` crate to the asynchronous `sqlx` crate. Introduce `async-trait` for object-safe asynchronous trait methods.

### Files to Modify:
*   `platform/Cargo.toml`
    *   Remove `postgres`.
    *   Add `sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "postgres", "chrono", "uuid", "json"] }`.
    *   Add `async-trait = "0.1"`.
*   `platform/crates/persistence/Cargo.toml`
    *   Replace `postgres` with `sqlx.workspace = true`.
    *   Add `async-trait.workspace = true`.
*   `platform/app-server/Cargo.toml`
    *   Add `sqlx = { workspace = true, features = ["postgres"] }`.
    *   Add `async-trait.workspace = true`.

## 3. Asynchronous Trait Definition
Convert `SessionStore` methods to asynchronous functions while maintaining object safety.

### Files to Modify:
*   `platform/crates/persistence/src/lib.rs`
    *   Apply `#[async_trait::async_trait]` to `pub trait SessionStore`.
    *   Add the `async` keyword to all trait methods (`init`, `save_session`, etc.).

## 4. Connection Pool and Implementation
Replace blocking mutexes with an asynchronous connection pool. Handle JSON mapping explicitly. Update all trait implementors.

### Files to Modify:
*   `platform/crates/persistence/src/lib.rs`
    *   **InMemorySessionStore:** Apply `#[async_trait::async_trait]` and `async` to all methods.
    *   **PostgresSessionStore:**
        *   Replace `Mutex<Client>` with `pool: sqlx::PgPool`.
        *   Rewrite `connect` to initialize `sqlx::postgres::PgPoolOptions`.
        *   Remove `with_client` and `std::thread::scope` logic.
        *   Refactor SQL execution to use `sqlx::query(...)` and `.execute(&self.pool).await`.
        *   Wrap `serde_json::Value` payloads in `sqlx::types::Json` during query binding and extraction.

## 5. App Server Integration
Update the application server to await asynchronous persistence calls.

### Files to Modify:
*   `platform/app-server/src/main.rs`
    *   Append `.await` to all `state.store` method invocations.
    *   Await `PostgresSessionStore::connect` during application startup.

## 6. PostgreSQL Pub/Sub Implementation
Utilize PostgreSQL `LISTEN/NOTIFY` for cross-pod WebSocket coordination.

### Files to Modify:
*   `platform/crates/persistence/src/lib.rs`
    *   Modify `PostgresSessionStore::save_session` to execute within a transaction.
    *   Execute `let mut tx = self.pool.begin().await?`.
    *   Execute the session `UPDATE` query on `&mut tx`.
    *   Execute `NOTIFY session_updates, '<session_code>'` on `&mut tx`.
    *   Commit the transaction (`tx.commit().await?`).
*   `platform/app-server/src/main.rs`
    *   Implement a background `tokio::spawn` task during server startup.
    *   Conditionally spawn the task only `if let Some(database_url) = state.config.database_url.as_deref()`.
    *   Instantiate `sqlx::postgres::PgListener::connect(database_url).await`.
    *   Implement a blocking event loop using `while let Ok(notification) = listener.recv().await`.
    *   Upon receiving a notification, extract the `session_code`.
    *   Invalidate the local cache (`state.sessions.lock().await.remove(session_code)`).
    *   Invoke `broadcast_session_state(&state, session_code, None).await` to push the updated state to local WebSocket clients.