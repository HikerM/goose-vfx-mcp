CREATE TABLE catalog_models (id TEXT PRIMARY KEY, digest TEXT NOT NULL, payload TEXT NOT NULL);
CREATE TABLE catalog_snapshots (
    model_id TEXT PRIMARY KEY REFERENCES catalog_models(id),
    digest TEXT NOT NULL
);
CREATE TABLE model_snapshots (
    model_id TEXT NOT NULL REFERENCES catalog_models(id),
    snapshot_id TEXT NOT NULL REFERENCES repository_snapshots(id),
    PRIMARY KEY (model_id, snapshot_id)
);
CREATE TABLE catalog_artifacts (
    id TEXT PRIMARY KEY,
    snapshot_id TEXT NOT NULL REFERENCES repository_snapshots(id),
    path TEXT NOT NULL,
    digest TEXT NOT NULL,
    payload TEXT NOT NULL,
    UNIQUE (snapshot_id, path)
);
CREATE TABLE model_artifacts (
    model_id TEXT NOT NULL REFERENCES catalog_models(id),
    artifact_id TEXT NOT NULL REFERENCES catalog_artifacts(id),
    PRIMARY KEY (model_id, artifact_id)
);
CREATE TABLE catalog_variants (
    id TEXT PRIMARY KEY,
    model_id TEXT NOT NULL REFERENCES catalog_models(id),
    digest TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE TABLE variant_artifacts (
    variant_id TEXT NOT NULL REFERENCES catalog_variants(id),
    artifact_id TEXT NOT NULL REFERENCES catalog_artifacts(id),
    PRIMARY KEY (variant_id, artifact_id)
);
CREATE TABLE catalog_components (id TEXT PRIMARY KEY, digest TEXT NOT NULL, payload TEXT NOT NULL);
CREATE TABLE model_components (
    model_id TEXT NOT NULL REFERENCES catalog_models(id),
    component_id TEXT NOT NULL REFERENCES catalog_components(id),
    PRIMARY KEY (model_id, component_id)
);
CREATE TABLE component_artifacts (
    component_id TEXT NOT NULL REFERENCES catalog_components(id),
    artifact_id TEXT NOT NULL REFERENCES catalog_artifacts(id),
    PRIMARY KEY (component_id, artifact_id)
);
CREATE INDEX catalog_variants_model ON catalog_variants(model_id, id);
PRAGMA user_version = 2;
