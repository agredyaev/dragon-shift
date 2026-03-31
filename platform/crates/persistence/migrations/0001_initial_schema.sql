CREATE TABLE workshop_sessions (
    session_id TEXT PRIMARY KEY,
    session_code TEXT UNIQUE NOT NULL,
    payload JSONB NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_workshop_sessions_code ON workshop_sessions(session_code);

CREATE TABLE session_artifacts (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    payload JSONB NOT NULL
);

CREATE INDEX idx_session_artifacts_session_created
    ON session_artifacts(session_id, created_at, id);

CREATE TABLE player_identities (
    reconnect_token TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    player_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);

CREATE INDEX idx_player_identities_session_id ON player_identities(session_id);
