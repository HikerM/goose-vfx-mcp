use etcetera::{choose_app_strategy, AppStrategy, AppStrategyArgs};
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::fs::OpenOptions;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt as _;
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const WINDOWS_DEFAULT_LUMINA_PATH_ROOT: &str = r"D:\Lumina";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsDirectoryStatus {
    Directory,
    NotDirectory,
    Reparse,
    Missing,
    Unavailable,
}

pub struct Paths;

impl Paths {
    fn get_dir(dir_type: DirType) -> PathBuf {
        if let Ok(test_root) = std::env::var("LUMINA_PATH_ROOT") {
            let base = PathBuf::from(test_root);
            match dir_type {
                DirType::Config => base.join("config"),
                DirType::Data => base.join("data"),
                DirType::State => base.join("state"),
                DirType::Plugins => base.join(".agents").join("plugins"),
                DirType::Agents => base.join(".agents").join("agents"),
                DirType::AgentsHome => base.join(".agents"),
            }
        } else {
            let strategy = choose_app_strategy(AppStrategyArgs {
                top_level_domain: "io.github".to_string(),
                author: "HikerM".to_string(),
                app_name: "lumina".to_string(),
            })
            .expect("lumina requires a home dir");

            match dir_type {
                DirType::Config => strategy.config_dir(),
                DirType::Data => strategy.data_dir(),
                DirType::State => strategy.state_dir().unwrap_or(strategy.data_dir()),
                DirType::Plugins => strategy.home_dir().join(".agents").join("plugins"),
                DirType::Agents => strategy.home_dir().join(".agents").join("agents"),
                DirType::AgentsHome => strategy.home_dir().join(".agents"),
            }
        }
    }

    pub fn config_dir() -> PathBuf {
        Self::get_dir(DirType::Config)
    }

    pub fn data_dir() -> PathBuf {
        Self::get_dir(DirType::Data)
    }

    pub fn state_dir() -> PathBuf {
        Self::get_dir(DirType::State)
    }

    pub fn plugins_dir() -> PathBuf {
        Self::get_dir(DirType::Plugins)
    }

    pub fn agents_dir() -> PathBuf {
        Self::get_dir(DirType::Agents)
    }

    pub fn agents_home_dir() -> PathBuf {
        Self::get_dir(DirType::AgentsHome)
    }

    pub fn in_agents_home_dir(subpath: &str) -> PathBuf {
        Self::agents_home_dir().join(subpath)
    }

    pub fn in_state_dir(subpath: &str) -> PathBuf {
        Self::state_dir().join(subpath)
    }

    pub fn in_config_dir(subpath: &str) -> PathBuf {
        Self::config_dir().join(subpath)
    }

    pub fn in_data_dir(subpath: &str) -> PathBuf {
        Self::data_dir().join(subpath)
    }

    pub fn ensure_windows_governed_root() -> Result<(), WindowsLuminaPathRootError> {
        #[cfg(windows)]
        {
            let inspector = FsWindowsPathRootInspector;
            let configured_root = std::env::var("LUMINA_PATH_ROOT").ok();
            if let Some(path) = resolve_lumina_path_root_for_platform(
                GovernedRootPlatform::Windows,
                configured_root.as_deref(),
                &inspector,
            )? {
                unsafe { std::env::set_var("LUMINA_PATH_ROOT", path) };
            }
        }

        Ok(())
    }
}

enum DirType {
    Config,
    Data,
    State,
    Plugins,
    Agents,
    AgentsHome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GovernedRootPlatform {
    Windows,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsLuminaPathRootErrorKind {
    InvalidPath,
    DriveUnavailable,
    NotWritable,
    ReparseRisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsLuminaPathRootError {
    kind: WindowsLuminaPathRootErrorKind,
    message: &'static str,
}

impl WindowsLuminaPathRootError {
    pub const fn kind(self) -> WindowsLuminaPathRootErrorKind {
        self.kind
    }

    #[cfg(test)]
    pub const fn new_for_test(kind: WindowsLuminaPathRootErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }
}

impl std::fmt::Display for WindowsLuminaPathRootError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for WindowsLuminaPathRootError {}

fn invalid_windows_lumina_path_root() -> WindowsLuminaPathRootError {
    WindowsLuminaPathRootError {
        kind: WindowsLuminaPathRootErrorKind::InvalidPath,
        message:
            "Lumina can only use a regular folder on the local D drive for managed MCP storage on Windows. Choose a folder on D and restart Lumina.",
    }
}

fn unavailable_windows_lumina_path_root() -> WindowsLuminaPathRootError {
    WindowsLuminaPathRootError {
        kind: WindowsLuminaPathRootErrorKind::DriveUnavailable,
        message:
            "Lumina needs a local D drive before managed MCP storage can start on Windows. Reconnect or create the D drive and restart Lumina.",
    }
}

fn unwritable_windows_lumina_path_root() -> WindowsLuminaPathRootError {
    WindowsLuminaPathRootError {
        kind: WindowsLuminaPathRootErrorKind::NotWritable,
        message:
            "Lumina cannot write to its managed MCP storage folder on Windows. Choose or create a writable folder on D and restart Lumina.",
    }
}

fn reparse_windows_lumina_path_root() -> WindowsLuminaPathRootError {
    WindowsLuminaPathRootError {
        kind: WindowsLuminaPathRootErrorKind::ReparseRisk,
        message:
            "Lumina cannot use a linked or redirected folder for managed MCP storage on Windows. Choose a regular folder on D and restart Lumina.",
    }
}

fn resolve_lumina_path_root_for_platform(
    platform: GovernedRootPlatform,
    configured_root: Option<&str>,
    inspector: &impl WindowsPathRootInspector,
) -> Result<Option<PathBuf>, WindowsLuminaPathRootError> {
    match platform {
        GovernedRootPlatform::Other => Ok(configured_root
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)),
        GovernedRootPlatform::Windows => {
            let candidate = configured_root
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(WINDOWS_DEFAULT_LUMINA_PATH_ROOT);
            let normalized = validate_windows_lumina_path_root(candidate)?;
            inspector.ensure_existing_chain_safe(&normalized)?;
            let path = PathBuf::from(&normalized);
            inspector.ensure_writable_directory(&normalized, &path)?;
            Ok(Some(path))
        }
    }
}

fn validate_windows_lumina_path_root(value: &str) -> Result<String, WindowsLuminaPathRootError> {
    if value.starts_with(r"\\") || value.starts_with("//") {
        return Err(invalid_windows_lumina_path_root());
    }

    let normalized = value.replace('/', "\\");
    let bytes = normalized.as_bytes();
    if bytes.len() < 4
        || !bytes[0].eq_ignore_ascii_case(&b'd')
        || bytes[1] != b':'
        || bytes[2] != b'\\'
    {
        return Err(invalid_windows_lumina_path_root());
    }

    let remainder = &normalized[3..];
    if remainder.is_empty() || remainder.contains(':') {
        return Err(invalid_windows_lumina_path_root());
    }

    let mut segments = Vec::new();
    for segment in remainder.split('\\') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.ends_with([' ', '.'])
        {
            return Err(invalid_windows_lumina_path_root());
        }
        segments.push(segment);
    }

    if segments.is_empty() {
        return Err(invalid_windows_lumina_path_root());
    }

    Ok(format!(r"D:\{}", segments.join("\\")))
}

trait WindowsPathRootInspector {
    fn ensure_existing_chain_safe(
        &self,
        normalized_root: &str,
    ) -> Result<(), WindowsLuminaPathRootError>;

    fn ensure_writable_directory(
        &self,
        normalized_root: &str,
        path: &Path,
    ) -> Result<(), WindowsLuminaPathRootError>;
}

#[cfg(windows)]
struct FsWindowsPathRootInspector;

#[cfg(windows)]
impl WindowsPathRootInspector for FsWindowsPathRootInspector {
    fn ensure_existing_chain_safe(
        &self,
        normalized_root: &str,
    ) -> Result<(), WindowsLuminaPathRootError> {
        inspect_windows_directory_chain(normalized_root, true, lookup_windows_directory_status)
    }

    fn ensure_writable_directory(
        &self,
        normalized_root: &str,
        path: &Path,
    ) -> Result<(), WindowsLuminaPathRootError> {
        std::fs::create_dir_all(path).map_err(|_| unwritable_windows_lumina_path_root())?;
        // Re-check the full chain after creation because a previously missing ancestor could
        // have been materialized through a reparse boundary between the initial scan and mkdir.
        inspect_windows_directory_chain(normalized_root, false, lookup_windows_directory_status)?;

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let probe = path.join(format!(
            ".lumina-root-write-check-{}-{suffix}",
            std::process::id()
        ));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .map_err(|_| unwritable_windows_lumina_path_root())?;
        std::fs::remove_file(&probe).map_err(|_| unwritable_windows_lumina_path_root())?;
        Ok(())
    }
}

#[cfg(windows)]
fn ensure_existing_directory_is_safe(path: &Path) -> Result<(), WindowsLuminaPathRootError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| unavailable_windows_lumina_path_root())?;
    if !metadata.is_dir() {
        return Err(unavailable_windows_lumina_path_root());
    }
    if is_reparse_point(&metadata) {
        return Err(reparse_windows_lumina_path_root());
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
struct FsWindowsPathRootInspector;

#[cfg(not(windows))]
impl WindowsPathRootInspector for FsWindowsPathRootInspector {
    fn ensure_existing_chain_safe(
        &self,
        _normalized_root: &str,
    ) -> Result<(), WindowsLuminaPathRootError> {
        Ok(())
    }

    fn ensure_writable_directory(
        &self,
        _normalized_root: &str,
        _path: &Path,
    ) -> Result<(), WindowsLuminaPathRootError> {
        Ok(())
    }
}

#[cfg(windows)]
fn lookup_windows_directory_status(path: &Path) -> WindowsDirectoryStatus {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() => WindowsDirectoryStatus::NotDirectory,
        Ok(metadata) if is_reparse_point(&metadata) => WindowsDirectoryStatus::Reparse,
        Ok(_) => WindowsDirectoryStatus::Directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            WindowsDirectoryStatus::Missing
        }
        Err(_) => WindowsDirectoryStatus::Unavailable,
    }
}

fn inspect_windows_directory_chain(
    normalized_root: &str,
    allow_missing_tail: bool,
    mut lookup: impl FnMut(&Path) -> WindowsDirectoryStatus,
) -> Result<(), WindowsLuminaPathRootError> {
    let mut current = PathBuf::from(r"D:\");
    match lookup(&current) {
        WindowsDirectoryStatus::Directory => {}
        WindowsDirectoryStatus::Reparse => return Err(reparse_windows_lumina_path_root()),
        WindowsDirectoryStatus::Missing | WindowsDirectoryStatus::Unavailable => {
            return Err(unavailable_windows_lumina_path_root())
        }
        WindowsDirectoryStatus::NotDirectory => return Err(unwritable_windows_lumina_path_root()),
    }

    for segment in normalized_root[3..].split('\\') {
        current.push(segment);
        match lookup(&current) {
            WindowsDirectoryStatus::Directory => {}
            WindowsDirectoryStatus::Reparse => return Err(reparse_windows_lumina_path_root()),
            WindowsDirectoryStatus::NotDirectory => {
                return Err(unwritable_windows_lumina_path_root())
            }
            WindowsDirectoryStatus::Missing if allow_missing_tail => break,
            WindowsDirectoryStatus::Missing => return Err(unwritable_windows_lumina_path_root()),
            WindowsDirectoryStatus::Unavailable => {
                return Err(unavailable_windows_lumina_path_root())
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeWindowsPathRootInspector {
        existing_chain_error: Option<WindowsLuminaPathRootError>,
        writable_error: Option<WindowsLuminaPathRootError>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl WindowsPathRootInspector for FakeWindowsPathRootInspector {
        fn ensure_existing_chain_safe(
            &self,
            _normalized_root: &str,
        ) -> Result<(), WindowsLuminaPathRootError> {
            self.calls.borrow_mut().push("existing");
            self.existing_chain_error.map_or(Ok(()), Err)
        }

        fn ensure_writable_directory(
            &self,
            _normalized_root: &str,
            _path: &Path,
        ) -> Result<(), WindowsLuminaPathRootError> {
            self.calls.borrow_mut().push("writable");
            self.writable_error.map_or(Ok(()), Err)
        }
    }

    #[test]
    fn windows_defaults_to_governed_d_root_when_unset() {
        let resolved = resolve_lumina_path_root_for_platform(
            GovernedRootPlatform::Windows,
            None,
            &FakeWindowsPathRootInspector::default(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(resolved, PathBuf::from(WINDOWS_DEFAULT_LUMINA_PATH_ROOT));
    }

    #[test]
    fn windows_accepts_explicit_d_subdirectories() {
        let resolved = resolve_lumina_path_root_for_platform(
            GovernedRootPlatform::Windows,
            Some(r"d:/Lumina Team/storage"),
            &FakeWindowsPathRootInspector::default(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(resolved, PathBuf::from(r"D:\Lumina Team\storage"));
    }

    #[test]
    fn windows_checks_existing_chain_before_writable_probe() {
        let inspector = FakeWindowsPathRootInspector::default();

        let _ = resolve_lumina_path_root_for_platform(
            GovernedRootPlatform::Windows,
            Some(r"D:\Lumina\storage"),
            &inspector,
        )
        .unwrap();

        assert_eq!(*inspector.calls.borrow(), vec!["existing", "writable"]);
    }

    #[test]
    fn windows_rejects_c_relative_unc_and_drive_root_paths() {
        for candidate in [r"C:\Lumina", r"Lumina", r"\\server\share\lumina", r"D:\"] {
            let error = resolve_lumina_path_root_for_platform(
                GovernedRootPlatform::Windows,
                Some(candidate),
                &FakeWindowsPathRootInspector::default(),
            )
            .unwrap_err();

            assert_eq!(error.kind(), WindowsLuminaPathRootErrorKind::InvalidPath);
        }
    }

    #[test]
    fn windows_rejects_unavailable_d_drive_and_unwritable_targets() {
        let unavailable = FakeWindowsPathRootInspector {
            existing_chain_error: Some(unavailable_windows_lumina_path_root()),
            writable_error: None,
            ..Default::default()
        };
        let unavailable_error = resolve_lumina_path_root_for_platform(
            GovernedRootPlatform::Windows,
            None,
            &unavailable,
        )
        .unwrap_err();
        assert_eq!(
            unavailable_error.kind(),
            WindowsLuminaPathRootErrorKind::DriveUnavailable
        );

        let unwritable = FakeWindowsPathRootInspector {
            existing_chain_error: None,
            writable_error: Some(unwritable_windows_lumina_path_root()),
            ..Default::default()
        };
        let unwritable_error = resolve_lumina_path_root_for_platform(
            GovernedRootPlatform::Windows,
            Some(r"D:\Lumina\storage"),
            &unwritable,
        )
        .unwrap_err();
        assert_eq!(
            unwritable_error.kind(),
            WindowsLuminaPathRootErrorKind::NotWritable
        );
    }

    #[test]
    fn windows_rejects_reparse_risks() {
        let inspector = FakeWindowsPathRootInspector {
            existing_chain_error: Some(reparse_windows_lumina_path_root()),
            writable_error: None,
            ..Default::default()
        };

        let error = resolve_lumina_path_root_for_platform(
            GovernedRootPlatform::Windows,
            Some(r"D:\Lumina\storage"),
            &inspector,
        )
        .unwrap_err();

        assert_eq!(error.kind(), WindowsLuminaPathRootErrorKind::ReparseRisk);
    }

    #[test]
    fn windows_post_create_chain_rejects_missing_or_reparse_ancestors() {
        let missing =
            inspect_windows_directory_chain(r"D:\Lumina\storage", false, |path| {
                match path.to_string_lossy().as_ref() {
                    r"D:\" => WindowsDirectoryStatus::Directory,
                    r"D:\Lumina" => WindowsDirectoryStatus::Missing,
                    _ => WindowsDirectoryStatus::Directory,
                }
            })
            .unwrap_err();
        assert_eq!(missing.kind(), WindowsLuminaPathRootErrorKind::NotWritable);

        let reparse =
            inspect_windows_directory_chain(r"D:\Lumina\storage", false, |path| {
                match path.to_string_lossy().as_ref() {
                    r"D:\" => WindowsDirectoryStatus::Directory,
                    r"D:\Lumina" => WindowsDirectoryStatus::Reparse,
                    _ => WindowsDirectoryStatus::Directory,
                }
            })
            .unwrap_err();
        assert_eq!(reparse.kind(), WindowsLuminaPathRootErrorKind::ReparseRisk);
    }

    #[test]
    fn non_windows_keeps_existing_root_and_does_not_default() {
        let configured = resolve_lumina_path_root_for_platform(
            GovernedRootPlatform::Other,
            Some("/tmp/lumina"),
            &FakeWindowsPathRootInspector::default(),
        )
        .unwrap();
        let unset = resolve_lumina_path_root_for_platform(
            GovernedRootPlatform::Other,
            None,
            &FakeWindowsPathRootInspector::default(),
        )
        .unwrap();

        assert_eq!(configured, Some(PathBuf::from("/tmp/lumina")));
        assert_eq!(unset, None);
    }
}
