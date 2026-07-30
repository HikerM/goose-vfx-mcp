use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePoolOptions;

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const SOURCE_SCHEMA_FENCE_ID: &str = goose::verified_source_catalog::schema::SOURCE_SCHEMA_FENCE_ID;

type SourceCatalogDao = goose::verified_source_catalog::SourceCatalogDao;
type SourceCatalogOperation = goose::verified_source_catalog::SourceCatalogOperation;
type SourceCatalogSafeView = goose::verified_source_catalog::SourceCatalogSafeView;
type SourceReleaseV1 = goose::verified_source_catalog::SourceReleaseV1;
type SourceRootKeyV1 = goose::verified_source_catalog::SourceRootKeyV1;
type SourceSignatureV1 = goose::verified_source_catalog::SourceSignatureV1;
type SourceSignedEnvelopeV1 = goose::verified_source_catalog::SourceSignedEnvelopeV1;
type SourceSnapshotV1 = goose::verified_source_catalog::SourceSnapshotV1;
type SourceState = goose::verified_source_catalog::SourceState;
type SourceTrustRootV1 = goose::verified_source_catalog::SourceTrustRootV1;
type SourceUnsignedPayloadV1 = goose::verified_source_catalog::SourceUnsignedPayloadV1;
type StoredSourceOperation = goose::verified_source_catalog::StoredSourceOperation;

async fn bootstrap_source_schema(pool: &sqlx::Pool<sqlx::Sqlite>) -> anyhow::Result<()> {
    goose::verified_source_catalog::bootstrap_source_schema(pool).await
}

async fn ensure_source_schema_compatible(pool: &sqlx::Pool<sqlx::Sqlite>) -> anyhow::Result<()> {
    goose::verified_source_catalog::ensure_source_schema_compatible(pool).await
}

fn parse_signed_envelope(
    raw: &[u8],
) -> anyhow::Result<goose::verified_source_catalog::VerifiedSourceDocument> {
    goose::verified_source_catalog::parse_signed_envelope(raw)
}

#[tokio::test]
async fn same_mcp_version_can_coexist_across_sources() {
    let pool = test_pool().await;
    goose::verified_source_catalog::bootstrap_source_schema(&pool)
        .await
        .unwrap();
    let dao = SourceCatalogDao::new(pool.clone());

    let first = goose::verified_source_catalog::parse_signed_envelope(&envelope_json(
        "source-a",
        "source-a-name",
        "1.0.0",
        false,
        61,
    ))
    .unwrap();
    let second = goose::verified_source_catalog::parse_signed_envelope(&envelope_json(
        "source-b",
        "source-b-name",
        "1.0.0",
        false,
        62,
    ))
    .unwrap();
    dao.store_verified_document(&first, 100).await.unwrap();
    dao.store_verified_document(&second, 101).await.unwrap();

    let first_releases = dao.list_releases_by_source("source-a").await.unwrap();
    let second_releases = dao.list_releases_by_source("source-b").await.unwrap();

    assert_eq!(first_releases.len(), 1);
    assert_eq!(second_releases.len(), 1);
    assert_eq!(first_releases[0].mcp_id, second_releases[0].mcp_id);
    assert_eq!(first_releases[0].version, second_releases[0].version);
    assert_ne!(first_releases[0].source_id, second_releases[0].source_id);
}

#[tokio::test]
async fn revocation_is_source_scoped() {
    let pool = test_pool().await;
    goose::verified_source_catalog::bootstrap_source_schema(&pool)
        .await
        .unwrap();
    let dao = SourceCatalogDao::new(pool.clone());

    let revoked = goose::verified_source_catalog::parse_signed_envelope(&envelope_json(
        "source-a",
        "source-a-name",
        "1.0.0",
        true,
        71,
    ))
    .unwrap();
    let active = goose::verified_source_catalog::parse_signed_envelope(&envelope_json(
        "source-b",
        "source-b-name",
        "1.0.0",
        false,
        72,
    ))
    .unwrap();
    dao.store_verified_document(&revoked, 100).await.unwrap();
    dao.store_verified_document(&active, 101).await.unwrap();

    let revoked_release = dao.list_releases_by_source("source-a").await.unwrap();
    let active_release = dao.list_releases_by_source("source-b").await.unwrap();
    assert!(revoked_release[0].revoked);
    assert_eq!(
        revoked_release[0].release_state,
        goose::verified_source_catalog::ReleaseState::Revoked
    );
    assert!(!active_release[0].revoked);
    assert_eq!(
        active_release[0].release_state,
        goose::verified_source_catalog::ReleaseState::Active
    );
}

#[tokio::test]
async fn a_newer_snapshot_cannot_clear_an_existing_release_revocation() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    let dao = SourceCatalogDao::new(pool);

    let revoked = parse_signed_envelope(&envelope_json_at(
        "source-revocation-retention",
        "source-revocation-retention-name",
        "1.0.0",
        true,
        73,
        123,
    ))
    .unwrap();
    let later_without_revocation = parse_signed_envelope(&envelope_json_at(
        "source-revocation-retention",
        "source-revocation-retention-name",
        "1.1.0",
        false,
        73,
        124,
    ))
    .unwrap();

    dao.store_verified_document(&revoked, 100).await.unwrap();
    dao.store_verified_document(&later_without_revocation, 101)
        .await
        .unwrap();

    let safe_view = dao
        .list_safe_view("source-revocation-retention")
        .await
        .unwrap();
    assert_eq!(safe_view.source.issued_at_ms, 124);
    assert_eq!(safe_view.source.revocation_count, 1);
    assert_eq!(safe_view.releases.len(), 1);
    assert_eq!(safe_view.releases[0].version, "1.1.0");
    assert!(safe_view.releases[0].revoked);
    assert_eq!(
        safe_view.releases[0].release_state,
        goose::verified_source_catalog::ReleaseState::Revoked
    );
}

#[tokio::test]
async fn schema_fence_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query("UPDATE source_schema_fence SET schema_hash = ?")
        .bind("0".repeat(64))
        .execute(&pool)
        .await
        .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn bootstrap_does_not_create_shared_schema_version() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();

    let schema_version_tables = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(schema_version_tables, 0);
    ensure_source_schema_compatible(&pool).await.unwrap();
}

#[tokio::test]
async fn shared_schema_version_presence_absence_and_future_version_are_ignored() {
    let pool = test_pool().await;
    sqlx::query(
        r#"CREATE TABLE schema_version(
            version INTEGER PRIMARY KEY,
            applied_at_ms INTEGER NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO schema_version(version, applied_at_ms) VALUES (999, 0)")
        .execute(&pool)
        .await
        .unwrap();

    bootstrap_source_schema(&pool).await.unwrap();
    let shared_version = sqlx::query_scalar::<_, i64>("SELECT MAX(version) FROM schema_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(shared_version, 999);
    ensure_source_schema_compatible(&pool).await.unwrap();

    sqlx::query("DROP TABLE schema_version")
        .execute(&pool)
        .await
        .unwrap();
    ensure_source_schema_compatible(&pool).await.unwrap();
}

#[tokio::test]
async fn ddl_tampering_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query("DROP INDEX source_releases_lookup")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE INDEX source_releases_lookup ON source_releases(source_id, version)")
        .execute(&pool)
        .await
        .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn non_source_index_on_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query("CREATE INDEX release_lookup_shadow ON source_releases(version)")
        .execute(&pool)
        .await
        .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn trigger_on_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        r#"CREATE TRIGGER release_audit
           AFTER INSERT ON source_releases
           BEGIN
               SELECT NEW.release_id;
           END"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn single_quoted_trigger_on_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        r#"CREATE TRIGGER release_audit
           AFTER INSERT ON 'source_releases'
           BEGIN
               SELECT NEW.release_id;
           END"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn foreign_key_to_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        r#"CREATE TABLE release_aliases (
            alias_id INTEGER PRIMARY KEY,
            source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id)
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn single_quoted_foreign_key_to_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        r#"CREATE TABLE release_aliases (
            alias_id INTEGER PRIMARY KEY,
            source_id TEXT NOT NULL REFERENCES 'source_catalog_documents'(source_id)
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn trigger_body_referencing_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        r#"CREATE TABLE local_events (
            event_id INTEGER PRIMARY KEY,
            note TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TRIGGER local_events_source_shadow
           AFTER INSERT ON local_events
           BEGIN
               INSERT INTO source_operations(
                   source_id, operation_id, operation_kind, operation_status,
                   last_error, created_at_ms, updated_at_ms
               ) VALUES ('source-shadow', 'op-shadow', 'verify_snapshot', 'pending', NULL, 0, 0);
           END"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn single_quoted_trigger_body_referencing_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        r#"CREATE TABLE local_events (
            event_id INTEGER PRIMARY KEY,
            note TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TRIGGER local_events_source_shadow
           AFTER INSERT ON local_events
           BEGIN
               INSERT INTO 'source_operations'(
                   source_id, operation_id, operation_kind, operation_status,
                   last_error, created_at_ms, updated_at_ms
               ) VALUES ('source-shadow', 'op-shadow', 'verify_snapshot', 'pending', NULL, 0, 0);
           END"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn view_referencing_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        "CREATE VIEW release_projection AS SELECT release_id, version FROM source_releases",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn single_quoted_view_referencing_source_table_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        "CREATE VIEW release_projection AS SELECT release_id, version FROM 'source_releases'",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn source_names_inside_literals_and_comments_do_not_fail_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        r#"CREATE TABLE local_events (
            event_id INTEGER PRIMARY KEY,
            note TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE VIEW harmless_projection AS
           SELECT l.event_id, 'source_releases' AS literal_name
           FROM local_events l
           JOIN local_events r ON l.note = 'source_catalog_documents'
           WHERE r.note <> 'source_operations'"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TRIGGER harmless_commentary
           AFTER INSERT ON local_events
           BEGIN
               -- source_releases
               /* source_operations */
               INSERT INTO local_events(note)
               VALUES ('source_schema_fence');
           END"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    ensure_source_schema_compatible(&pool).await.unwrap();
}

#[tokio::test]
async fn bootstrap_source_schema_repairs_legacy_v20_source_only_fixture() {
    let pool = test_pool().await;
    bootstrap_legacy_v20_source_only_fixture(&pool).await;
    assert!(ensure_source_schema_compatible(&pool).await.is_err());

    bootstrap_source_schema(&pool).await.unwrap();

    let ledger_versions =
        sqlx::query_scalar::<_, i64>("SELECT version FROM source_schema_ledger ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(ledger_versions, vec![20, 21]);

    let schema_hash: String =
        sqlx::query_scalar("SELECT schema_hash FROM source_schema_fence WHERE fence_id = ?")
            .bind(SOURCE_SCHEMA_FENCE_ID)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(schema_hash.len(), 64);
    assert!(!schema_hash.is_empty());

    ensure_source_schema_compatible(&pool).await.unwrap();
}

#[tokio::test]
async fn forged_fence_row_fails_closed() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO source_schema_fence(fence_id, min_version, max_version, schema_hash) VALUES (?, ?, ?, ?)",
    )
    .bind("verified_source_catalog/forged")
    .bind(20_i64)
    .bind(21_i64)
    .bind("f".repeat(64))
    .execute(&pool)
    .await
    .unwrap();

    assert!(ensure_source_schema_compatible(&pool).await.is_err());
}

#[tokio::test]
async fn safe_dto_redacts_unsafe_values_and_rejects_unsafe_input() {
    let pool = test_pool().await;
    bootstrap_source_schema(&pool).await.unwrap();
    let dao = SourceCatalogDao::new(pool.clone());

    let document = parse_signed_envelope(&envelope_json(
        "source-safe",
        "https://example.test/internal-code",
        "1.0.0",
        false,
        81,
    ))
    .unwrap();
    dao.store_verified_document(&document, 100).await.unwrap();
    dao.set_source_status("source-safe", SourceState::Active, 101)
        .await
        .unwrap();
    dao.record_operation(&StoredSourceOperation {
        source_id: "source-safe".to_string(),
        operation_id: "op-1".to_string(),
        operation_kind: SourceCatalogOperation::VerifySnapshot,
        operation_state: goose::verified_source_catalog::dao::OperationState::Applied,
        last_error: Some("internal digest credential handle".to_string()),
        created_at_ms: 102,
        updated_at_ms: 103,
    })
    .await
    .unwrap();

    let safe_view = dao.list_safe_view("source-safe").await.unwrap();
    let json = String::from_utf8(safe_view.to_json_bytes()).unwrap();
    assert!(json.contains("\"source_name\":\"redacted\""));
    assert!(!json.contains("https://example.test/internal-code"));
    for forbidden in [
        "\"url\"",
        "\"digest\"",
        "\"proof\"",
        "\"kid\"",
        "\"signature\"",
        "\"anchor\"",
        "\"witness\"",
        "\"credential\"",
        "\"handle\"",
        "\"provenance\"",
        "\"last_error\"",
    ] {
        assert!(
            !json.contains(forbidden),
            "safe dto leaked forbidden token {forbidden}: {json}"
        );
    }

    let bad_url_json = json.replace(
        "\"version\":\"1.0.0\"",
        "\"version\":\"https://example.test/secret\"",
    );
    assert!(SourceCatalogSafeView::from_json_bytes(bad_url_json.as_bytes()).is_err());

    let bad_forbidden_json = json.replace("\"platform\":\"linux\"", "\"platform\":\"credential\"");
    assert!(SourceCatalogSafeView::from_json_bytes(bad_forbidden_json.as_bytes()).is_err());

    let bad_nested_json = json.replace(
        "\"source_id\":\"source-safe\"",
        "\"source_id\":{\"nested\":\"x\"}",
    );
    assert!(SourceCatalogSafeView::from_json_bytes(bad_nested_json.as_bytes()).is_err());

    let bad_extra_json = json.replace(
        "\"release_state\":\"active\"",
        "\"release_state\":\"active\",\"extra\":\"x\"",
    );
    assert!(SourceCatalogSafeView::from_json_bytes(bad_extra_json.as_bytes()).is_err());

    let bad_negative_issued_json = json.replace("\"issued_at_ms\":123", "\"issued_at_ms\":-1");
    assert!(SourceCatalogSafeView::from_json_bytes(bad_negative_issued_json.as_bytes()).is_err());

    let bad_negative_release_count_json =
        json.replace("\"release_count\":1", "\"release_count\":-1");
    assert!(
        SourceCatalogSafeView::from_json_bytes(bad_negative_release_count_json.as_bytes()).is_err()
    );

    let bad_negative_revocation_count_json =
        json.replace("\"revocation_count\":0", "\"revocation_count\":-1");
    assert!(
        SourceCatalogSafeView::from_json_bytes(bad_negative_revocation_count_json.as_bytes())
            .is_err()
    );

    let bad_negative_created_json = json.replace("\"created_at_ms\":102", "\"created_at_ms\":-1");
    assert!(SourceCatalogSafeView::from_json_bytes(bad_negative_created_json.as_bytes()).is_err());

    let bad_negative_updated_json = json.replace("\"updated_at_ms\":103", "\"updated_at_ms\":-1");
    assert!(SourceCatalogSafeView::from_json_bytes(bad_negative_updated_json.as_bytes()).is_err());

    let bad_huge_issued_json = json.replace(
        "\"issued_at_ms\":123",
        "\"issued_at_ms\":18446744073709551616",
    );
    assert!(SourceCatalogSafeView::from_json_bytes(bad_huge_issued_json.as_bytes()).is_err());

    if usize::BITS < 63 {
        let overflowing_count = (usize::MAX as u64 + 1).to_string();
        let bad_overflow_release_count_json = json.replace(
            "\"release_count\":1",
            &format!("\"release_count\":{overflowing_count}"),
        );
        assert!(
            SourceCatalogSafeView::from_json_bytes(bad_overflow_release_count_json.as_bytes())
                .is_err()
        );
    }
}

fn envelope_json(
    source_id: &str,
    source_name: &str,
    version: &str,
    revoked: bool,
    seed: u8,
) -> Vec<u8> {
    envelope_json_at(source_id, source_name, version, revoked, seed, 123)
}

fn envelope_json_at(
    source_id: &str,
    source_name: &str,
    version: &str,
    revoked: bool,
    seed: u8,
    issued_at_ms: i64,
) -> Vec<u8> {
    let current = root_key(seed);
    let payload = SourceUnsignedPayloadV1 {
        schema_version: 1,
        source_id: source_id.to_string(),
        source_name: source_name.to_string(),
        issued_at_ms,
        root: SourceTrustRootV1 {
            quorum: 1,
            keys: vec![current.clone()],
            previous: None,
        },
        snapshot: SourceSnapshotV1 {
            releases: vec![SourceReleaseV1 {
                release_id: "release-a".to_string(),
                mcp_id: "pkg/example".to_string(),
                version: version.to_string(),
                manifest_kind: "manifest".to_string(),
                platform: "linux".to_string(),
                architecture: "x86_64".to_string(),
                variant: "gnu".to_string(),
            }],
            revocations: if revoked {
                vec![goose::verified_source_catalog::SourceRevocationV1 {
                    release_id: "release-a".to_string(),
                    reason_code: "withdrawn".to_string(),
                    revoked_at_ms: 400,
                }]
            } else {
                Vec::new()
            },
        },
    };
    let canonical_payload = payload.to_canonical_json_bytes();
    let key = signing_key(seed);
    let signature = key.sign(&canonical_payload);
    let envelope = SourceSignedEnvelopeV1 {
        payload,
        signatures: vec![SourceSignatureV1 {
            kid: current.kid,
            sig_b64u: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        }],
    };
    envelope.to_canonical_json_bytes()
}

fn root_key(seed: u8) -> SourceRootKeyV1 {
    let signing = signing_key(seed);
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(&signing.verifying_key().to_bytes());
    SourceRootKeyV1 {
        kid: format!("ed25519-spki-sha256:{}", sha256_hex(&der)),
        spki_der_b64u: URL_SAFE_NO_PAD.encode(der),
    }
}

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

async fn test_pool() -> sqlx::Pool<sqlx::Sqlite> {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        out.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
    }
    out
}

async fn bootstrap_legacy_v20_source_only_fixture(pool: &sqlx::Pool<sqlx::Sqlite>) {
    for statement in LEGACY_SOURCE_SCHEMA_V20_STATEMENTS {
        sqlx::query(statement).execute(pool).await.unwrap();
    }
}

const LEGACY_SOURCE_SCHEMA_V20_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE source_catalog_documents (
        source_id TEXT PRIMARY KEY,
        source_name TEXT NOT NULL,
        schema_version INTEGER NOT NULL CHECK(schema_version = 1),
        issued_at_ms INTEGER NOT NULL,
        source_status TEXT NOT NULL CHECK(source_status IN ('active','suspended','revoked')),
        raw_bytes BLOB NOT NULL,
        canonical_payload_bytes BLOB NOT NULL,
        canonical_envelope_bytes BLOB NOT NULL,
        raw_digest TEXT NOT NULL CHECK(length(raw_digest) = 64),
        canonical_digest TEXT NOT NULL CHECK(length(canonical_digest) = 64),
        signed_digest TEXT NOT NULL CHECK(length(signed_digest) = 64),
        binding_digest TEXT NOT NULL CHECK(length(binding_digest) = 64),
        document_digest TEXT NOT NULL CHECK(length(document_digest) = 64),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE source_root_policies (
        source_id TEXT PRIMARY KEY REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        current_quorum INTEGER NOT NULL CHECK(current_quorum > 0),
        previous_quorum INTEGER CHECK(previous_quorum IS NULL OR previous_quorum > 0),
        has_previous INTEGER NOT NULL CHECK(has_previous IN (0,1))
    )"#,
    r#"CREATE TABLE source_root_keys (
        source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        root_set TEXT NOT NULL CHECK(root_set IN ('current','previous')),
        kid TEXT NOT NULL,
        spki_der_b64u TEXT NOT NULL,
        key_order INTEGER NOT NULL CHECK(key_order >= 0),
        PRIMARY KEY(source_id, root_set, kid)
    )"#,
    r#"CREATE TABLE source_signatures (
        source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        kid TEXT NOT NULL,
        sig_b64u TEXT NOT NULL,
        signature_order INTEGER NOT NULL CHECK(signature_order >= 0),
        PRIMARY KEY(source_id, kid)
    )"#,
    r#"CREATE TABLE source_releases (
        source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        release_id TEXT NOT NULL,
        mcp_id TEXT NOT NULL,
        version TEXT NOT NULL,
        manifest_kind TEXT NOT NULL,
        platform TEXT NOT NULL,
        architecture TEXT NOT NULL,
        variant TEXT NOT NULL,
        release_status TEXT NOT NULL CHECK(release_status IN ('active','revoked','superseded')),
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(source_id, release_id),
        UNIQUE(source_id, mcp_id, version, manifest_kind, platform, architecture, variant)
    )"#,
    r#"CREATE TABLE source_revocations (
        source_id TEXT NOT NULL,
        release_id TEXT NOT NULL,
        reason_code TEXT NOT NULL,
        revoked_at_ms INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(source_id, release_id),
        FOREIGN KEY(source_id, release_id) REFERENCES source_releases(source_id, release_id) ON DELETE RESTRICT
    )"#,
    r#"CREATE TABLE source_operations (
        source_id TEXT NOT NULL REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        operation_id TEXT NOT NULL,
        operation_kind TEXT NOT NULL CHECK(operation_kind IN ('ingest_snapshot','verify_snapshot','activate_source','revoke_release')),
        operation_status TEXT NOT NULL CHECK(operation_status IN ('pending','applied','failed','stopped')),
        last_error TEXT,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(source_id, operation_id)
    )"#,
    r#"CREATE TABLE source_stop_flags (
        source_id TEXT PRIMARY KEY REFERENCES source_catalog_documents(source_id) ON DELETE CASCADE,
        stop_required INTEGER NOT NULL CHECK(stop_required IN (0,1)),
        reason TEXT,
        updated_at_ms INTEGER NOT NULL
    )"#,
];
