pub mod discovery;
pub mod formats;
pub mod mcp_servers;

use crate::config::paths::Paths;
use crate::subprocess::SubprocessExt;
use anyhow::{anyhow, bail, Result};
use fs_err as fs;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const INSTALL_METADATA: &str = ".lumina-plugin-install.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginFormat {
    Gemini,
    OpenPlugins,
}

impl std::fmt::Display for PluginFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginFormat::Gemini => write!(f, "gemini"),
            PluginFormat::OpenPlugins => write!(f, "open-plugins"),
        }
    }
}

pub fn plugin_install_dir() -> PathBuf {
    Paths::plugins_dir()
}

pub fn project_plugin_install_dir(project_root: &Path) -> PathBuf {
    project_root.join(".agents").join("plugins")
}

#[derive(Debug, Clone)]
pub struct PluginInstall {
    pub name: String,
    pub version: String,
    pub format: PluginFormat,
    pub source: String,
    pub directory: PathBuf,
    pub skills: Vec<ImportedSkill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSkill {
    pub name: String,
    pub directory: PathBuf,
}

#[derive(Debug, thiserror::Error)]
#[error("format not supported")]
pub struct FormatNotSupported;

#[derive(Debug, Deserialize, Serialize)]
struct InstallMetadata {
    source: String,
    source_type: String,
    #[allow(dead_code)]
    format: String,
}

pub fn installed_plugin_skill_dirs() -> Vec<PathBuf> {
    let plugins_dir = plugin_install_dir();
    let entries = match fs::read_dir(plugins_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut seen = HashSet::new();
    entries
        .flatten()
        .flat_map(|entry| {
            let plugin_dir = entry.path();
            let default_skills_dir = plugin_dir.join("skills");
            let mut skill_dirs = Vec::new();
            if default_skills_dir.is_dir() {
                skill_dirs.push(default_skills_dir);
            }
            skill_dirs.extend(formats::open_plugins::installed_skill_dirs(&plugin_dir));
            skill_dirs
        })
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

pub fn install_plugin(source: &str) -> Result<PluginInstall> {
    install_plugin_at_root(source, &plugin_install_dir())
}

fn install_plugin_at_root(source: &str, install_root: &Path) -> Result<PluginInstall> {
    if source.trim().is_empty() {
        bail!("Plugin source URL must not be empty");
    }

    let temp_dir = tempfile::tempdir()?;
    let checkout_dir = temp_dir.path().join("checkout");
    clone_git_repo(source, &checkout_dir)?;

    install_from_checkout_at_root(source, &checkout_dir, install_root)
}

pub fn update_plugin(name: &str) -> Result<PluginInstall> {
    update_plugin_at_root(&plugin_install_dir(), name)
}

fn update_plugin_at_root(install_root: &Path, name: &str) -> Result<PluginInstall> {
    if name.trim().is_empty() {
        bail!("Plugin name must not be empty");
    }

    let current_install_dir = install_root.join(name);
    if !current_install_dir.is_dir() {
        bail!("Plugin '{}' is not installed", name);
    }

    let metadata = read_install_metadata(&current_install_dir)?;
    if metadata.source_type != "git" {
        bail!(
            "Plugin '{}' was installed from '{}' and cannot be updated with this command",
            name,
            metadata.source_type
        );
    }

    fs::create_dir_all(install_root)?;
    let temp_dir = tempfile::tempdir_in(install_root)?;
    let checkout_dir = temp_dir.path().join("checkout");
    clone_git_repo(&metadata.source, &checkout_dir)?;

    let updated = install_from_checkout_at_root(&metadata.source, &checkout_dir, temp_dir.path())?;
    if updated.name != name {
        bail!(
            "Updated plugin name '{}' does not match installed plugin '{}'",
            updated.name,
            name
        );
    }

    replace_plugin_dir(&updated.directory, &current_install_dir)?;

    Ok(PluginInstall {
        directory: current_install_dir,
        ..updated
    })
}

fn install_from_checkout_at_root(
    source: &str,
    checkout_dir: &Path,
    install_root: &Path,
) -> Result<PluginInstall> {
    match formats::open_plugins::try_install_from_manifest_at_root(
        source,
        checkout_dir,
        install_root,
    ) {
        Ok(install) => return Ok(install),
        Err(err) if err.is::<FormatNotSupported>() => {}
        Err(err) => return Err(err),
    }

    match formats::gemini::try_install_from_manifest_at_root(source, checkout_dir, install_root) {
        Ok(install) => Ok(install),
        Err(err) if err.is::<FormatNotSupported>() => bail!("No supported plugin format found"),
        Err(err) => Err(err),
    }
}

fn clone_git_repo(source: &str, destination: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg(source)
        .arg(destination)
        .set_no_window()
        .output()
        .map_err(|e| anyhow!("Failed to run git clone: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let message = if stderr.is_empty() { stdout } else { stderr };
        bail!("Failed to clone plugin repository: {message}");
    }

    Ok(())
}

fn read_install_metadata(directory: &Path) -> Result<InstallMetadata> {
    let metadata_path = directory.join(INSTALL_METADATA);
    if !metadata_path.is_file() {
        bail!(
            "Plugin at {} does not contain install metadata and cannot be updated",
            directory.display()
        );
    }

    Ok(serde_json::from_str(&fs::read_to_string(metadata_path)?)?)
}

fn write_install_metadata(destination: &Path, source: &str, format: &str) -> Result<()> {
    let metadata = InstallMetadata {
        source: source.to_string(),
        source_type: "git".to_string(),
        format: format.to_string(),
    };
    fs::write(
        destination.join(INSTALL_METADATA),
        serde_json::to_string_pretty(&metadata)?,
    )?;
    Ok(())
}

fn replace_plugin_dir(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("Plugin destination has no parent directory"))?;
    let backup_dir = tempfile::tempdir_in(parent)?;
    let backup_plugin_dir = backup_dir.path().join("plugin");

    fs::rename(destination, &backup_plugin_dir)?;
    if let Err(err) = fs::rename(source, destination) {
        fs::rename(&backup_plugin_dir, destination)?;
        return Err(err.into());
    }

    Ok(())
}

fn copy_dir_all(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_dir_all(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_repo_without_supported_manifest() {
        let install_root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();

        let err = install_from_checkout_at_root(
            "https://example.invalid/repo.git",
            repo.path(),
            install_root.path(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("No supported plugin format found"));
    }

    #[test]
    fn updates_git_backed_plugin() {
        let install_root = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_gemini_plugin(repo.path(), "1.0.0", "Audit code");
        init_git_repo(repo.path());
        commit_git_repo(repo.path(), "initial");
        let source = repo.path().to_path_buf();

        let installed =
            install_plugin_at_root(source.to_str().unwrap(), install_root.path()).unwrap();
        assert_eq!(installed.version, "1.0.0");

        write_gemini_plugin(&source, "2.0.0", "Audit updated code");
        commit_git_repo(&source, "update");

        let updated = update_plugin_at_root(install_root.path(), "test-plugin").unwrap();

        assert_eq!(updated.version, "2.0.0");
        assert_eq!(updated.directory, install_root.path().join("test-plugin"));
        assert_eq!(
            fs::read_to_string(updated.directory.join("skills/audit/SKILL.md")).unwrap(),
            "---\nname: audit\ndescription: Audit updated code\n---\nDo an audit."
        );
    }

    #[test]
    fn update_rejects_non_git_backed_plugin() {
        let install_root = tempfile::tempdir().unwrap();
        let plugin_dir = install_root.path().join("test-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join(INSTALL_METADATA),
            r#"{"source":"/tmp/test-plugin","source_type":"local","format":"gemini"}"#,
        )
        .unwrap();

        let err = update_plugin_at_root(install_root.path(), "test-plugin").unwrap_err();

        assert!(err
            .to_string()
            .contains("cannot be updated with this command"));
    }

    fn write_gemini_plugin(repo: &Path, version: &str, description: &str) {
        fs::write(
            repo.join(formats::gemini::MANIFEST),
            format!(r#"{{"name":"test-plugin","version":"{version}"}}"#),
        )
        .unwrap();
        let skill_dir = repo.join("skills").join("audit");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: audit\ndescription: {description}\n---\nDo an audit."),
        )
        .unwrap();
    }

    fn init_git_repo(repo: &Path) {
        run_git(repo, &["init"]);
        run_git(repo, &["config", "user.email", "lumina@example.com"]);
        run_git(repo, &["config", "user.name", "Lumina"]);
    }

    fn commit_git_repo(repo: &Path, message: &str) {
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "-m", message]);
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .set_no_window()
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
