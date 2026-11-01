CREATE TABLE repository_snapshots (
    id TEXT PRIMARY KEY,
    repository TEXT NOT NULL,
    revision TEXT NOT NULL,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE installation_plans (
    id TEXT PRIMARY KEY,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE TABLE plan_snapshots (
    plan_id TEXT NOT NULL REFERENCES installation_plans(id),
    snapshot_id TEXT NOT NULL REFERENCES repository_snapshots(id),
    PRIMARY KEY (plan_id, snapshot_id)
);
CREATE TABLE lifecycle_jobs (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE,
    plan_id TEXT NOT NULL REFERENCES installation_plans(id),
    plan_digest TEXT NOT NULL,
    stage TEXT NOT NULL,
    status TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0 CHECK(version >= 0),
    generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0),
    lease_owner TEXT,
    lease_until INTEGER,
    updated_at INTEGER NOT NULL
);
CREATE TABLE job_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES lifecycle_jobs(id),
    version INTEGER NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (job_id, version)
);
CREATE INDEX job_events_cursor ON job_events(job_id, sequence);
CREATE TABLE capability_evidence (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    installation_id TEXT NOT NULL,
    capability TEXT NOT NULL,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE INDEX capability_evidence_installation ON capability_evidence(installation_id, id);
PRAGMA user_version = 1;
