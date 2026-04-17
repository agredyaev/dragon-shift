CREATE TABLE characters (
    character_id TEXT PRIMARY KEY,
    description TEXT NOT NULL,
    neutral_sprite TEXT NOT NULL,
    happy_sprite TEXT NOT NULL,
    angry_sprite TEXT NOT NULL,
    sleepy_sprite TEXT NOT NULL,
    remaining_sprite_regenerations SMALLINT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_characters_created_at ON characters(created_at, character_id);
