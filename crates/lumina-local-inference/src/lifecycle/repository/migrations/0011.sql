CREATE TABLE installation_storage_events (
    job_id TEXT NOT NULL REFERENCES lifecycle_jobs(id),
    version INTEGER NOT NULL CHECK(version >= 0),
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY(job_id, version)
);
CREATE TABLE installation_storage_reservations (
    job_id TEXT PRIMARY KEY,
    version INTEGER NOT NULL CHECK(version >= 0),
    volume_id TEXT,
    reserved_bytes INTEGER NOT NULL CHECK(reserved_bytes >= 0),
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    FOREIGN KEY(job_id, version) REFERENCES installation_storage_events(job_id, version)
);
CREATE INDEX installation_storage_volume ON installation_storage_reservations(volume_id,reserved_bytes);
PRAGMA user_version = 11;
