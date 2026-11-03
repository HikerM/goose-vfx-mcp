CREATE TABLE installation_transfer_credits (
    job_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    bytes INTEGER NOT NULL CHECK(bytes > 0),
    PRIMARY KEY (job_id, artifact_id),
    FOREIGN KEY (job_id, artifact_id) REFERENCES job_files(job_id, artifact_id)
);
PRAGMA user_version = 13;
