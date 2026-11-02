CREATE TABLE installation_failures (
    job_id TEXT NOT NULL,
    job_version INTEGER NOT NULL CHECK(job_version >= 0),
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK(created_at >= 0),
    PRIMARY KEY(job_id, job_version),
    FOREIGN KEY(job_id, job_version) REFERENCES job_events(job_id, version)
);
PRAGMA user_version = 10;
