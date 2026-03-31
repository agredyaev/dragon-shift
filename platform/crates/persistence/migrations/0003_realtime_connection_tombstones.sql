CREATE TABLE retired_realtime_connections (
    connection_id TEXT PRIMARY KEY,
    replica_id TEXT NOT NULL,
    retired_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
