# Lumina legacy migration contract

Lumina uses its own product, package, protocol, configuration, storage, and release namespaces. The names in the **Legacy inputs** column are accepted only by the migration layer so existing users can move their local data once; they are not Lumina runtime or publishing identities.

| Surface | Lumina identity | Legacy inputs |
| --- | --- | --- |
| Product and executable | `Lumina`, `lumina`, `luminad` | `Goose`, `goose`, `goosed` |
| Rust packages and modules | `lumina*` | `goose*` |
| JavaScript workspace packages | distributor-owned Lumina npm scope | upstream Goose packages |
| Deep links | `lumina://` | `goose://` |
| Environment variables | `LUMINA_*` | `GOOSE_*` |
| Application identifier | `io.github.hikerm.Lumina` | `io.github.block.Goose` |
| Config and data roots | platform-native `io.github/HikerM/lumina` and `D:\Lumina` | platform-native `Block/Block/goose` and `D:\Goose` |
| Project instructions | `.luminahints` | `.goosehints` |
| Managed extension label | `io.github.hikerm.lumina.managed` | `ai.block.goose.managed` |

## Migration rules

1. Lumina always writes the new namespace. It never creates new files, registry entries, URLs, processes, package metadata, or telemetry events under a legacy name.
2. Import is explicit, local, idempotent, and non-destructive. Legacy files are copied or transformed into Lumina storage and are never deleted automatically.
3. A migration marker records the imported source, schema version, timestamp, and result. Re-running Lumina must not duplicate sessions, secrets, schedules, recipes, or extensions.
4. Lumina values win when both namespaces contain the same logical record. A conflict is reported to the user instead of silently overwriting current Lumina data.
5. Secrets are moved through the operating-system credential store API. They must not be copied into logs, migration reports, or plaintext configuration.
6. Legacy deep links may be parsed by the migration compatibility boundary, but Lumina does not register the legacy URI scheme or advertise it.
7. The compatibility boundary has tests and an announced removal policy. New product features may not depend on legacy constants.
8. Signed MCP, evidence, sidecar and commit-root state is never treated as valid Lumina state. The migrator archives it under `data/legacy/v1`; the user must re-authorize extensions and rebuild evidence in Lumina's new cryptographic domain.

The supported entry point is `lumina migrate`. Use `lumina migrate --dry-run` first to inspect planned imports without creating the target directory. The migration lock and marker make repeated execution safe; source data is left unchanged.

## Completeness gate

A release is migration-complete only when provider settings, credentials, sessions, schedules, recipes, extensions, MCP configuration, local inference assets, project instructions, and desktop preferences have an automated migration or an explicit unsupported-state message. The release must also prove that a clean install contains no operational dependency on an upstream service, update channel, telemetry project, OAuth client, package, protocol, or distribution identifier.
