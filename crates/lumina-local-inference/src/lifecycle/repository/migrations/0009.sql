CREATE TABLE installation_file_retry_events (
    job_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version >= 0),
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY(job_id, artifact_id, version),
    FOREIGN KEY(job_id, artifact_id) REFERENCES job_files(job_id, artifact_id)
);
CREATE TABLE installation_file_retries (
    job_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version >= 0),
    next_retry_at INTEGER,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY(job_id, artifact_id),
    FOREIGN KEY(job_id, artifact_id, version) REFERENCES installation_file_retry_events(job_id, artifact_id, version)
);
CREATE INDEX installation_file_retry_due ON installation_file_retries(job_id, next_retry_at);
PRAGMA user_version = 9;
