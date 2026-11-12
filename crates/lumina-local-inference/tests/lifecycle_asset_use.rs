use fs2::FileExt;
use lumina_local_inference::lifecycle::RuntimeAssetGuard;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/runtime-asset-tests");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        #[cfg(windows)]
        let root = {
            let path = root.to_str().unwrap();
            PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(path))
        };
        Self {
            temp: tempfile::Builder::new()
                .prefix("asset-use-")
                .tempdir_in(root)
                .unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"runtime asset bytes").unwrap();
        path
    }

    fn child(&self, asset: &Path) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "runtime_asset_guard_child",
                "--nocapture",
            ])
            .env("LUMINA_ASSET_TEST_PATH", asset)
            .env("LUMINA_ASSET_TEST_ROOT", self.temp.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        command.spawn().unwrap()
    }
}

fn assert_exclusive_blocked(file: &File) {
    let error = FileExt::try_lock_exclusive(file).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        fs2::lock_contended_error().raw_os_error()
    );
}

fn wait_ready(child: &mut Child, path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "Asset guard child exited before acquiring its lock"
        );
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Asset guard child did not report ready");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn finish_child(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Asset guard child did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn shared_guards_canonicalize_deduplicate_sort_and_release_on_last_drop() {
    let fixture = Fixture::new();
    let a = fixture.file("a.gguf");
    let b = fixture.file("b.gguf");
    std::fs::create_dir(fixture.path("nested")).unwrap();
    let permissions = std::fs::metadata(&a).unwrap().permissions();
    let first = RuntimeAssetGuard::acquire([
        b.clone(),
        a.clone(),
        a.clone(),
        fixture.path("nested/../b.gguf"),
    ])
    .unwrap();
    let mut expected = vec![a.canonicalize().unwrap(), b.canonicalize().unwrap()];
    expected.sort();
    assert_eq!(first.paths(), expected.as_slice());
    let second = RuntimeAssetGuard::acquire([a.clone()]).unwrap();
    let probe = File::open(&a).unwrap();
    FileExt::try_lock_shared(&probe).unwrap();
    FileExt::unlock(&probe).unwrap();
    assert_exclusive_blocked(&probe);
    drop(first);
    assert_exclusive_blocked(&probe);
    drop(second);
    FileExt::try_lock_exclusive(&probe).unwrap();
    drop(probe);
    assert_eq!(std::fs::read(&a).unwrap(), b"runtime asset bytes");
    assert_eq!(
        std::fs::metadata(&a).unwrap().permissions().readonly(),
        permissions.readonly()
    );
}

#[test]
fn invalid_later_asset_releases_files_already_acquired() {
    let fixture = Fixture::new();
    let valid = fixture.file("a.gguf");
    let directory = fixture.path("z-directory");
    std::fs::create_dir(&directory).unwrap();
    let error = RuntimeAssetGuard::acquire([valid.clone(), directory]).unwrap_err();
    assert!(error.to_string().contains("not a regular file"));
    let probe = File::open(&valid).unwrap();
    FileExt::try_lock_exclusive(&probe).unwrap();
    drop(probe);
    assert!(RuntimeAssetGuard::acquire([valid.clone(), fixture.path("missing.gguf")]).is_err());
    let probe = File::open(valid).unwrap();
    FileExt::try_lock_exclusive(&probe).unwrap();
}

#[test]
fn exclusive_conflict_fails_without_leaking_earlier_shared_locks() {
    let fixture = Fixture::new();
    let first = fixture.file("a.gguf");
    let busy = fixture.file("z.gguf");
    let exclusive = File::open(&busy).unwrap();
    FileExt::try_lock_exclusive(&exclusive).unwrap();
    let error = RuntimeAssetGuard::acquire([busy.clone(), first.clone()]).unwrap_err();
    assert!(error.to_string().contains("shared lock is unavailable"));
    let probe = File::open(&first).unwrap();
    FileExt::try_lock_exclusive(&probe).unwrap();
    drop(probe);
    drop(exclusive);
    let guard = RuntimeAssetGuard::acquire([first, busy]).unwrap();
    assert_eq!(guard.paths().len(), 2);
}

#[test]
fn hard_linked_external_files_are_accepted_and_lock_the_same_asset() {
    let fixture = Fixture::new();
    let original = fixture.file("original.gguf");
    let alias = fixture.path("hard-link.gguf");
    std::fs::hard_link(&original, &alias).unwrap();
    let guard = RuntimeAssetGuard::acquire([original.clone(), alias.clone()]).unwrap();
    assert_eq!(guard.paths().len(), 2);
    let shared = RuntimeAssetGuard::acquire([alias.clone()]).unwrap();
    let probe = File::open(&original).unwrap();
    assert_exclusive_blocked(&probe);
    drop(guard);
    assert_exclusive_blocked(&probe);
    drop(shared);
    FileExt::try_lock_exclusive(&probe).unwrap();
    drop(probe);
    assert_eq!(std::fs::read(original).unwrap(), b"runtime asset bytes");
    assert_eq!(std::fs::read(alias).unwrap(), b"runtime asset bytes");
}

#[cfg(any(unix, windows))]
#[test]
fn symbolic_directory_aliases_resolve_to_the_original_asset() {
    let fixture = Fixture::new();
    let original = fixture.file("assets/model.gguf");
    let alias = fixture.path("asset-alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(fixture.path("assets"), &alias).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&alias)
            .arg(fixture.path("assets"))
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Could not create test junction: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let through_alias = alias.join("model.gguf");
    let guard = RuntimeAssetGuard::acquire([through_alias.clone(), original.clone()]).unwrap();
    assert_eq!(guard.paths(), &[original.canonicalize().unwrap()]);
    let probe = File::open(through_alias).unwrap();
    assert_exclusive_blocked(&probe);
    drop(guard);
    FileExt::try_lock_exclusive(&probe).unwrap();
}

#[test]
fn another_process_holds_assets_until_normal_or_forced_process_exit() {
    for force_exit in [false, true] {
        let fixture = Fixture::new();
        let asset = fixture.file("model.gguf");
        let mut child = fixture.child(&asset);
        wait_ready(&mut child, &fixture.path("ready"));
        let probe = File::open(&asset).unwrap();
        assert_exclusive_blocked(&probe);
        let shared = RuntimeAssetGuard::acquire([asset.clone()]).unwrap();
        drop(shared);
        if force_exit {
            child.kill().unwrap();
        } else {
            std::fs::write(fixture.path("exit"), b"exit without running destructors").unwrap();
        }
        let output = finish_child(child);
        if !force_exit {
            assert!(
                output.status.success(),
                "Asset guard child failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        FileExt::try_lock_exclusive(&probe).unwrap();
        drop(probe);
        assert_eq!(std::fs::read(asset).unwrap(), b"runtime asset bytes");
    }
}

#[cfg(windows)]
#[test]
fn windows_read_handles_prevent_write_rename_and_delete_until_drop() {
    let fixture = Fixture::new();
    let asset = fixture.file("model.gguf");
    let moved = fixture.path("moved.gguf");
    let guard = RuntimeAssetGuard::acquire([asset.clone()]).unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&asset)
        .is_err());
    assert!(std::fs::rename(&asset, &moved).is_err());
    assert!(std::fs::remove_file(&asset).is_err());
    assert_eq!(std::fs::read(&asset).unwrap(), b"runtime asset bytes");
    drop(guard);
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&asset)
            .unwrap(),
    );
    std::fs::rename(&asset, &moved).unwrap();
    std::fs::remove_file(&moved).unwrap();
}

#[test]
#[ignore = "subprocess helper for runtime asset lifetime tests"]
fn runtime_asset_guard_child() {
    let asset = PathBuf::from(std::env::var_os("LUMINA_ASSET_TEST_PATH").expect("child asset"));
    let root = PathBuf::from(std::env::var_os("LUMINA_ASSET_TEST_ROOT").expect("child root"));
    let _guard = RuntimeAssetGuard::acquire([asset]).unwrap();
    std::fs::write(root.join("ready"), b"shared lock held").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !root.join("exit").exists() {
        assert!(
            Instant::now() < deadline,
            "Asset guard child exit signal timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Exit while the guard is still live to exercise OS cleanup, not Rust Drop.
    std::process::exit(0);
}
