CREATE TABLE installation_inputs (
    job_id TEXT PRIMARY KEY REFERENCES lifecycle_jobs(id),
    version_id TEXT NOT NULL UNIQUE,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL
);
PRAGMA user_version = 7;
