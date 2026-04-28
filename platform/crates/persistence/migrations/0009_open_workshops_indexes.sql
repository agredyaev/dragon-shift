CREATE INDEX idx_workshop_sessions_phase_created_code
    ON workshop_sessions ((payload->>'phase'), (payload->>'created_at'), session_code);

CREATE INDEX idx_workshop_sessions_owner_account_id
    ON workshop_sessions ((payload->>'owner_account_id'));

CREATE INDEX idx_workshop_sessions_reserved_host_account_id
    ON workshop_sessions ((payload->>'reserved_host_account_id'));

CREATE INDEX idx_session_artifacts_judge_bundle_session_created
    ON session_artifacts (session_id, created_at DESC, id DESC)
    WHERE payload->>'kind' = 'judge_bundle_generated';
