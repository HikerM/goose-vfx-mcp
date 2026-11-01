use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use crate::lifecycle::RepositoryPath;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ObservedRepositoryPath(String);

impl ObservedRepositoryPath {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        ensure!(
            !value.is_empty()
                && value.len() <= 16 * 1024
                && !value.contains('\0')
                && value
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
            "Invalid observed Git repository path"
        );
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn materializable_path(&self) -> Result<RepositoryPath> {
        RepositoryPath::parse(self.0.clone())
    }
}

impl TryFrom<String> for ObservedRepositoryPath {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}

impl From<ObservedRepositoryPath> for String {
    fn from(value: ObservedRepositoryPath) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryPathDiagnostic {
    pub path: ObservedRepositoryPath,
    pub issue: RepositoryPathIssue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryPathIssue {
    NonPortable,
}
