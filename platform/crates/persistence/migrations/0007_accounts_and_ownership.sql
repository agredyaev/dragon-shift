CREATE TABLE accounts (
    account_id TEXT PRIMARY KEY,
    hero TEXT NOT NULL,
    name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_login_at TEXT
);

CREATE UNIQUE INDEX accounts_name_lower_idx ON accounts (LOWER(name));

ALTER TABLE characters
    ADD COLUMN owner_account_id TEXT NULL REFERENCES accounts(account_id) ON DELETE CASCADE;

CREATE INDEX characters_owner_idx ON characters (owner_account_id)
    WHERE owner_account_id IS NOT NULL;
