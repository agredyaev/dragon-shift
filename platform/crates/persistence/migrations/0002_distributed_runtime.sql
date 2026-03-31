CREATE TABLE session_leases (
    session_code TEXT PRIMARY KEY,
    lease_id TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE realtime_connections (
    connection_id TEXT PRIMARY KEY,
    session_code TEXT NOT NULL,
    player_id TEXT NOT NULL,
    replica_id TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_realtime_connections_session_player
    ON realtime_connections(session_code, player_id);

CREATE INDEX idx_realtime_connections_session_code
    ON realtime_connections(session_code, connection_id);
