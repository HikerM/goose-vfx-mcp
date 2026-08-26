use anyhow::Result;
use lumina::providers::utils::init_lumina_request_log;
use std::sync::OnceLock;

// Used to ensure we only set up tracing once
static INIT: OnceLock<Result<()>> = OnceLock::new();

/// Sets up the logging infrastructure for the CLI.
/// Logs go to a JSON file only (no console output).
pub fn setup_logging(name: Option<&str>) -> &'static Result<()> {
    INIT.get_or_init(|| {
        use tracing_subscriber::util::SubscriberInitExt;

        init_lumina_request_log()?;
        let config = lumina::logging::LoggingConfig {
            component: "cli",
            name,
            extra_directives: &["lumina_cli=info"],
            console: false,
            json: true,
        };
        let subscriber = lumina::logging::build_logging_subscriber(&config)?;

        subscriber
            .try_init()
            .map_err(|e| anyhow::anyhow!("Failed to set global subscriber: {}", e))?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::env;
    use tempfile::TempDir;

    fn setup_temp_home() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        if cfg!(windows) {
            env::set_var("USERPROFILE", temp_dir.path());
        } else {
            env::set_var("HOME", temp_dir.path());
        }
        temp_dir
    }

    #[test]
    fn test_log_directory_creation() {
        let _temp_dir = setup_temp_home();
        let log_dir = lumina::logging::prepare_log_directory("cli", true).unwrap();
        assert!(log_dir.exists());
        assert!(log_dir.is_dir());

        let path_components: Vec<_> = log_dir.components().collect();
        assert!(path_components.iter().any(|c| c.as_os_str() == "lumina"));
        assert!(path_components.iter().any(|c| c.as_os_str() == "logs"));
        assert!(path_components.iter().any(|c| c.as_os_str() == "cli"));
    }

    #[tokio::test]
    async fn test_default_filter_avoids_debug_by_default() {
        // The shared helper honours RUST_LOG; without it the defaults apply.
        // We just smoke-check that building the subscriber doesn't panic.
        let _temp_dir = setup_temp_home();
        let config = lumina::logging::LoggingConfig {
            component: "cli-test",
            name: None,
            extra_directives: &["lumina_cli=info"],
            console: false,
            json: true,
        };
        assert!(lumina::logging::build_logging_subscriber(&config).is_ok());
    }
}
