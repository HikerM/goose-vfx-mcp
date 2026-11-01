ALTER TABLE lifecycle_jobs ADD COLUMN accepted_at INTEGER NOT NULL DEFAULT 0 CHECK(accepted_at >= 0);
UPDATE lifecycle_jobs SET accepted_at = COALESCE(
    (SELECT MIN(created_at) FROM job_events WHERE job_id = lifecycle_jobs.id), updated_at
);
UPDATE job_events SET payload = json_set(payload, '$.accepted_at',
    (SELECT accepted_at FROM lifecycle_jobs WHERE id = job_events.job_id)
);
CREATE TABLE job_files (
    job_id TEXT NOT NULL REFERENCES lifecycle_jobs(id),
    artifact_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version >= 0),
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (job_id, artifact_id)
);
CREATE TABLE job_file_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    file_version INTEGER NOT NULL,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (job_id, artifact_id) REFERENCES job_files(job_id, artifact_id),
    UNIQUE (job_id, artifact_id, file_version)
);
CREATE INDEX job_file_event_cursor ON job_file_events(job_id, sequence);
PRAGMA user_version = 3;
