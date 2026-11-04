CREATE TABLE installation_blob_admission (
    job_id TEXT NOT NULL REFERENCES lifecycle_jobs(id),
    sha256 TEXT NOT NULL,
    bytes INTEGER NOT NULL CHECK(bytes > 0),
    pinned_blob TEXT REFERENCES managed_blobs(sha256),
    CHECK(pinned_blob IS NULL OR pinned_blob = sha256),
    PRIMARY KEY (job_id, sha256)
);
PRAGMA user_version = 15;
