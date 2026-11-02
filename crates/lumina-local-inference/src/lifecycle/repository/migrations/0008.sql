CREATE TABLE installation_control_receipts (
    idempotency_key TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES lifecycle_jobs(id),
    result_version INTEGER NOT NULL CHECK(result_version >= 0),
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    FOREIGN KEY (job_id, result_version) REFERENCES job_events(job_id, version)
);
CREATE INDEX installation_control_receipts_job ON installation_control_receipts(job_id, result_version);
PRAGMA user_version = 8;
