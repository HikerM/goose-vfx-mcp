CREATE TABLE installation_commit_journals (
    version_id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL UNIQUE REFERENCES lifecycle_jobs(id),
    manifest_digest TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version >= 0),
    digest TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE installations (id TEXT PRIMARY KEY);
CREATE TABLE installation_versions (
    version_id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL REFERENCES installations(id),
    job_id TEXT NOT NULL UNIQUE REFERENCES lifecycle_jobs(id),
    model_id TEXT NOT NULL REFERENCES catalog_models(id),
    variant_id TEXT NOT NULL REFERENCES catalog_variants(id),
    manifest_digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    registered_at INTEGER NOT NULL,
    UNIQUE (version_id, installation_id)
);
CREATE TABLE installation_activations (
    installation_id TEXT PRIMARY KEY REFERENCES installations(id),
    version_id TEXT NOT NULL UNIQUE,
    FOREIGN KEY (version_id, installation_id) REFERENCES installation_versions(version_id, installation_id)
);
CREATE TABLE managed_blobs (
    sha256 TEXT PRIMARY KEY,
    size_bytes INTEGER NOT NULL CHECK(size_bytes > 0)
);
CREATE TABLE installation_blob_refs (
    version_id TEXT NOT NULL REFERENCES installation_versions(version_id),
    artifact_id TEXT NOT NULL REFERENCES catalog_artifacts(id),
    sha256 TEXT NOT NULL REFERENCES managed_blobs(sha256),
    PRIMARY KEY (version_id, artifact_id)
);
CREATE INDEX installation_versions_owner ON installation_versions(installation_id, registered_at, version_id);
CREATE INDEX blob_version_references ON installation_blob_refs(sha256, version_id);
PRAGMA user_version = 4;
