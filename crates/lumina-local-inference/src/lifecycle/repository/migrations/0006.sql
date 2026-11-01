CREATE TABLE plan_gguf_observations (
    plan_id TEXT NOT NULL REFERENCES installation_plans(id),
    artifact_id TEXT NOT NULL REFERENCES catalog_artifacts(id),
    observation_id TEXT NOT NULL REFERENCES gguf_observations(id),
    PRIMARY KEY (plan_id, artifact_id)
);
PRAGMA user_version = 6;
