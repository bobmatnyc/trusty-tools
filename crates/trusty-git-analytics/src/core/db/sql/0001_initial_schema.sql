-- Initial schema for trusty-git-analytics core tables.
-- This migration creates the foundational tables required by the pipeline.
-- See docs/trusty-git-analytics/requirements/database-schema.md for the full schema reference.

CREATE TABLE IF NOT EXISTS authors (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    canonical_name  TEXT NOT NULL,
    canonical_email TEXT NOT NULL,
    aliases         TEXT NOT NULL DEFAULT '[]',
    UNIQUE(canonical_email)
);

CREATE INDEX IF NOT EXISTS idx_authors_canonical_email
    ON authors(canonical_email);

CREATE TABLE IF NOT EXISTS classifications (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    category    TEXT NOT NULL,
    subcategory TEXT,
    ticket_id   TEXT,
    confidence  REAL NOT NULL DEFAULT 0.0,
    method      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_classifications_category
    ON classifications(category);

CREATE TABLE IF NOT EXISTS commits (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    sha               TEXT NOT NULL UNIQUE,
    author_id         INTEGER,
    author_name       TEXT NOT NULL,
    author_email      TEXT NOT NULL,
    timestamp         TEXT NOT NULL,
    message           TEXT NOT NULL,
    repository        TEXT NOT NULL,
    files_changed     INTEGER NOT NULL DEFAULT 0,
    insertions        INTEGER NOT NULL DEFAULT 0,
    deletions         INTEGER NOT NULL DEFAULT 0,
    classification_id INTEGER,
    confidence        REAL,
    is_merge          INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY(author_id) REFERENCES authors(id) ON DELETE SET NULL,
    FOREIGN KEY(classification_id) REFERENCES classifications(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_commits_repository ON commits(repository);
CREATE INDEX IF NOT EXISTS idx_commits_timestamp  ON commits(timestamp);
CREATE INDEX IF NOT EXISTS idx_commits_author_id  ON commits(author_id);

CREATE TABLE IF NOT EXISTS files (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    commit_id   INTEGER NOT NULL,
    path        TEXT NOT NULL,
    change_type TEXT NOT NULL,
    insertions  INTEGER NOT NULL DEFAULT 0,
    deletions   INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY(commit_id) REFERENCES commits(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_files_commit_id ON files(commit_id);
CREATE INDEX IF NOT EXISTS idx_files_path      ON files(path);

CREATE TABLE IF NOT EXISTS pull_requests (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    pr_number    INTEGER NOT NULL,
    title        TEXT NOT NULL,
    author       TEXT NOT NULL,
    state        TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    merged_at    TEXT,
    commit_shas  TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX IF NOT EXISTS idx_pull_requests_pr_number ON pull_requests(pr_number);
CREATE INDEX IF NOT EXISTS idx_pull_requests_state     ON pull_requests(state);
