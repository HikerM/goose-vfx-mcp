use anyhow::{bail, Context, Result};
use lumina_legacy_migrator::{
    default_lumina_layout, discover_legacy_layouts, migrate, MigrationRequest, StorageLayout,
};
use std::path::PathBuf;

#[derive(Debug, Default, PartialEq, Eq)]
struct Arguments {
    source_roots: Vec<PathBuf>,
    target_root: Option<PathBuf>,
    dry_run: bool,
    migrate_secrets: bool,
}

impl Arguments {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut parsed = Self {
            migrate_secrets: true,
            ..Self::default()
        };
        let mut args = args.into_iter();

        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--source-root" => parsed.source_roots.push(PathBuf::from(
                    args.next().context("--source-root requires a path")?,
                )),
                "--target-root" => {
                    parsed.target_root = Some(PathBuf::from(
                        args.next().context("--target-root requires a path")?,
                    ));
                }
                "--dry-run" => parsed.dry_run = true,
                "--skip-secrets" => parsed.migrate_secrets = false,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                "--version" | "-V" => {
                    println!("{}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                _ => bail!("unknown argument: {argument}"),
            }
        }

        Ok(parsed)
    }
}

fn print_help() {
    println!(
        "Lumina one-time legacy data importer\n\n\
Usage: lumina-import-legacy [OPTIONS]\n\n\
Options:\n  \
  --source-root <PATH>  Import an explicit legacy storage root; may be repeated\n  \
  --target-root <PATH>  Write to an explicit Lumina storage root\n  \
  --dry-run             Report the import plan without writing data\n  \
  --skip-secrets        Do not import credentials into the Lumina credential store\n  \
  -h, --help            Print help\n  \
  -V, --version         Print version"
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = Arguments::parse(std::env::args().skip(1))?;
    let mut sources = discover_legacy_layouts()?;
    for (index, root) in arguments.source_roots.into_iter().enumerate().rev() {
        sources.insert(
            0,
            StorageLayout::from_root(format!("legacy-explicit-{index}"), root),
        );
    }

    let target = match arguments.target_root {
        Some(root) => StorageLayout::from_root("lumina", root),
        None => default_lumina_layout()?,
    };
    let report = migrate(MigrationRequest {
        sources,
        target,
        dry_run: arguments.dry_run,
        migrate_secrets: arguments.migrate_secrets,
    })
    .await?;

    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_offline_import_options() {
        let arguments = Arguments::parse([
            "--source-root".to_string(),
            "C:\\Legacy".to_string(),
            "--target-root".to_string(),
            "D:\\Lumina".to_string(),
            "--dry-run".to_string(),
            "--skip-secrets".to_string(),
        ])
        .unwrap();

        assert_eq!(arguments.source_roots, [PathBuf::from("C:\\Legacy")]);
        assert_eq!(arguments.target_root, Some(PathBuf::from("D:\\Lumina")));
        assert!(arguments.dry_run);
        assert!(!arguments.migrate_secrets);
    }

    #[test]
    fn rejects_missing_option_value() {
        let error = Arguments::parse(["--source-root".to_string()]).unwrap_err();
        assert!(error.to_string().contains("requires a path"));
    }
}
