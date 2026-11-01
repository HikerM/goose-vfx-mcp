CREATE TABLE gguf_observations (
    id TEXT PRIMARY KEY,
    artifact_id TEXT NOT NULL REFERENCES catalog_artifacts(id),
    observation_version TEXT NOT NULL,
    observation_digest TEXT NOT NULL,
    header_digest TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 67108864),
    observed_at INTEGER NOT NULL
);
CREATE INDEX gguf_observations_artifact ON gguf_observations(artifact_id, observation_version, observed_at, id);
PRAGMA user_version = 5;
