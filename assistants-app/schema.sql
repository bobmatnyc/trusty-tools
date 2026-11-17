-- Assistants app schema (Slice 1.2). Applied by db.init_db(); idempotent.

CREATE TABLE IF NOT EXISTS assistants (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL UNIQUE,
    instructions TEXT    NOT NULL DEFAULT '',
    -- Bedrock requires the inference profile, not the bare model ID.
    model        TEXT    NOT NULL DEFAULT 'us.anthropic.claude-sonnet-5-5',
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    assistant_id INTEGER NOT NULL REFERENCES assistants (id) ON DELETE CASCADE,
    role         TEXT    NOT NULL CHECK (role IN ('user', 'assistant')),
    content      TEXT    NOT NULL,
    tokens_in    INTEGER NOT NULL DEFAULT 0,
    tokens_out   INTEGER NOT NULL DEFAULT 0,
    cost_usd     REAL    NOT NULL DEFAULT 0.0,
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_messages_assistant_id ON messages (assistant_id);
