pub const SOURCE_SCHEMA_MIN_VERSION: i64 = 20;
pub const SOURCE_SCHEMA_MAX_VERSION: i64 = 21;
pub const SOURCE_SCHEMA_FENCE_ID: &str = "verified_source_catalog/v1";

pub const SOURCE_ONLY_TABLES: &[&str] = &[
    "source_catalog_documents",
    "source_root_policies",
    "source_root_keys",
    "source_signatures",
    "source_releases",
    "source_revocations",
    "source_operations",
    "source_stop_flags",
    "source_schema_ledger",
    "source_schema_fence",
];

pub const SOURCE_ONLY_INDEXES: &[&str] = &["source_operations_status", "source_releases_lookup"];

pub const SOURCE_ONLY_TRIGGERS: &[&str] = &[];

pub const SOURCE_ONLY_VIEWS: &[&str] = &[];
