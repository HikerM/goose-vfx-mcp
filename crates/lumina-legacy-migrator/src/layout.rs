use anyhow::{Context, Result};
use etcetera::{choose_app_strategy, AppStrategy, AppStrategyArgs};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageLayout {
    pub name: String,
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub desktop: PathBuf,
}

impl StorageLayout {
    pub fn from_root(name: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            name: name.into(),
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
            desktop: root.join("desktop"),
        }
    }

    pub fn has_any_content(&self) -> bool {
        [&self.config, &self.data, &self.state, &self.desktop]
            .into_iter()
            .any(|path| path.is_dir())
    }

    fn config_only(name: impl Into<String>, config: PathBuf) -> Self {
        let sentinel = config.join(".lumina-migration-no-runtime-data");
        Self {
            name: name.into(),
            config,
            data: sentinel.join("data"),
            state: sentinel.join("state"),
            desktop: sentinel.join("desktop"),
        }
    }

    fn recipes_only(name: impl Into<String>, recipes: PathBuf) -> Self {
        let mut layout = Self::config_only(name, recipes);
        layout.name = format!("legacy-recipe-path:{}", layout.name);
        layout
    }
}

pub fn default_lumina_layout() -> Result<StorageLayout> {
    if let Some(root) = std::env::var_os("LUMINA_PATH_ROOT") {
        return Ok(StorageLayout::from_root("lumina-env", root));
    }

    #[cfg(target_os = "windows")]
    {
        Ok(StorageLayout::from_root("lumina-windows", r"D:\Lumina"))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let strategy = choose_app_strategy(AppStrategyArgs {
            top_level_domain: "io.github".to_string(),
            author: "HikerM".to_string(),
            app_name: "lumina".to_string(),
        })
        .context("Lumina requires a home directory")?;
        let data = strategy.data_dir();
        Ok(StorageLayout {
            name: "lumina-native".to_string(),
            config: strategy.config_dir(),
            state: strategy.state_dir().unwrap_or_else(|| data.clone()),
            desktop: data.join("desktop"),
            data,
        })
    }
}

pub fn discover_legacy_layouts() -> Result<Vec<StorageLayout>> {
    let mut layouts = Vec::new();
    if let Some(root) = std::env::var_os("GOOSE_PATH_ROOT") {
        layouts.push(StorageLayout::from_root("legacy-env", root));
    }

    #[cfg(target_os = "windows")]
    layouts.push(StorageLayout::from_root("legacy-windows", r"D:\Goose"));

    #[cfg(target_os = "windows")]
    if let Some(program_data) = std::env::var_os("PROGRAMDATA") {
        layouts.push(StorageLayout::config_only(
            "legacy-windows-program-data",
            PathBuf::from(program_data).join("goose"),
        ));
    }

    #[cfg(not(target_os = "windows"))]
    layouts.push(StorageLayout::config_only(
        "legacy-system-config",
        PathBuf::from("/etc/goose"),
    ));

    if let Some(home) = home_directory() {
        layouts.push(StorageLayout::config_only(
            "legacy-user-dot-directory",
            home.join(".goose"),
        ));
        layouts.push(StorageLayout::config_only(
            "legacy-user-config-directory",
            home.join(".config").join("goose"),
        ));
    }

    if let Some(recipe_paths) = std::env::var_os("GOOSE_RECIPE_PATH") {
        for (index, path) in std::env::split_paths(&recipe_paths).enumerate() {
            if path.is_dir() {
                layouts.push(StorageLayout::recipes_only(
                    format!("environment-{index}"),
                    path,
                ));
            }
        }
    }

    let strategy = choose_app_strategy(AppStrategyArgs {
        top_level_domain: "Block".to_string(),
        author: "Block".to_string(),
        app_name: "goose".to_string(),
    })
    .context("legacy migration requires a home directory")?;
    let data = strategy.data_dir();
    layouts.push(StorageLayout {
        name: "legacy-native".to_string(),
        config: strategy.config_dir(),
        state: strategy.state_dir().unwrap_or_else(|| data.clone()),
        desktop: data.join("desktop"),
        data,
    });

    let mut seen = BTreeSet::new();
    layouts.retain(|layout| {
        seen.insert(format!(
            "{}|{}|{}|{}",
            layout.config.display(),
            layout.data.display(),
            layout.state.display(),
            layout.desktop.display()
        ))
    });
    Ok(layouts)
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}
