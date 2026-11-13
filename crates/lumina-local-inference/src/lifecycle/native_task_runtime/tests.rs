use super::*;
use fs2::FileExt;
use std::fs::OpenOptions;
use std::io::Write;

fn spawn_descendant() -> std::io::Result<std::process::ExitStatus> {
    Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "lifecycle::native_task_runtime::tests::controlled_task_process",
            "--ignored",
            "--nocapture",
        ])
        .env("LUMINA_TASK_TEST_MODE", "descendant")
        .status()
}

fn fixture() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/native-task-unit");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap()
}

#[test]
#[ignore = "controlled subprocess for finite task owner tests"]
fn controlled_task_process() {
    let root = PathBuf::from(std::env::var_os("LUMINA_TASK_TEST_ROOT").expect("fixture root"));
    let output =
        PathBuf::from(std::env::var_os("LUMINA_TASK_TEST_OUTPUT").expect("fixture output"));
    let mode = std::env::var("LUMINA_TASK_TEST_MODE").unwrap();
    if mode == "descendant" {
        std::fs::write(root.join("descendant-ran"), b"unexpected child execution").unwrap();
        return;
    }
    if mode == "allow-descendant" {
        assert!(spawn_descendant().unwrap().success());
        return;
    }
    let alive = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(root.join("alive.lock"))
        .unwrap();
    FileExt::lock_exclusive(&alive).unwrap();
    std::fs::write(root.join("ready"), b"ready").unwrap();
    match mode.as_str() {
        "success" => {
            let scratch = PathBuf::from(std::env::var_os("TEMP").unwrap());
            assert_eq!(std::env::var_os("TMP").unwrap(), scratch.as_os_str());
            assert_eq!(std::env::var_os("TMPDIR").unwrap(), scratch.as_os_str());
            assert!(matches!(scratch.components().next(),
                Some(std::path::Component::Prefix(p)) if matches!(p.kind(),std::path::Prefix::Disk(_))));
            assert_eq!(
                scratch.canonicalize().unwrap().parent().unwrap(),
                output.canonicalize().unwrap()
            );
            let tokenizer = scratch.join("audiocpp-gguf").join("model");
            std::fs::create_dir_all(&tokenizer).unwrap();
            std::fs::write(tokenizer.join("tokenizer.model"), b"test tokenizer").unwrap();
            std::fs::write(output.join("output.txt"), b"finite task output").unwrap();
            println!("controlled stdout");
            eprintln!("controlled stderr");
        }
        "failure" => {
            eprintln!("controlled failure");
            std::process::exit(17);
        }
        "flood" => {
            std::io::stdout()
                .write_all(&vec![b'x'; 1024 * 1024 + 65536])
                .unwrap();
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(120));
        }
        "oversize" => {
            std::fs::write(output.join("output.txt"), vec![b'x'; 128 * 1024]).unwrap();
            std::thread::sleep(Duration::from_secs(120));
        }
        "oversize-scratch" => {
            let nested = std::env::temp_dir().join("sidecars");
            std::fs::create_dir(&nested).unwrap();
            std::fs::write(nested.join("weights"), [0; 90]).unwrap();
            std::fs::write(output.join("output.txt"), [0; 20]).unwrap();
            std::thread::sleep(Duration::from_secs(120));
        }
        "wait" => std::thread::sleep(Duration::from_secs(120)),
        "try-descendant" => {
            let status = spawn_descendant();
            assert!(
                !status.is_ok_and(|status| status.success()),
                "descendant unexpectedly ran inside restricted job"
            );
        }
        _ => panic!("unknown controlled mode"),
    }
}

struct DropWitness {
    root: PathBuf,
    child_was_gone: Arc<AtomicBool>,
    dropped: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for DropWitness {
    fn drop(&mut self) {
        let gone = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.root.join("alive.lock"))
            .is_ok_and(|file| FileExt::try_lock_exclusive(&file).is_ok());
        self.child_was_gone.store(gone, Ordering::Release);
        if let Some(sender) = self.dropped.take() {
            let _ = sender.send(());
        }
    }
}

fn setup(
    root: &Path,
    mode: &str,
    retained: Vec<Box<dyn Send>>,
) -> (
    Command,
    process::ProcessOwnership,
    Arc<output::GuardedTaskDirectory>,
) {
    let directory = Arc::new(output::GuardedTaskDirectory::create(root).unwrap());
    let asset = root.join("asset");
    std::fs::write(&asset, b"controlled asset").unwrap();
    let assets = RuntimeAssetGuard::acquire([asset]).unwrap();
    let publication = publication_lease(root);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "lifecycle::native_task_runtime::tests::controlled_task_process",
            "--ignored",
            "--nocapture",
        ])
        .env("LUMINA_TASK_TEST_ROOT", root)
        .env("LUMINA_TASK_TEST_OUTPUT", directory.path())
        .env("LUMINA_TASK_TEST_MODE", mode)
        .env("TMP", root)
        .env("TEMP", root);
    (
        command,
        process::ProcessOwnership {
            assets,
            publication,
            retained,
        },
        directory,
    )
}

fn publication_lease(root: &Path) -> Arc<process::PublicationLease> {
    let (admission, snapshot_digest) = super::super::task_resources::controlled_test_admission();
    Arc::new(process::PublicationLease::new(
        acquire_runtime_permit(root).unwrap(),
        admission,
        snapshot_digest,
    ))
}

fn assert_released(root: &Path) {
    let alive = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("alive.lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&alive).unwrap();
    let file = File::open(root.join("asset")).unwrap();
    FileExt::try_lock_exclusive(&file).unwrap();
    acquire_runtime_permit(root).unwrap();
}

async fn ready(root: &Path) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !root.join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("controlled process reached its liveness lock");
}

#[test]
fn only_fixed_finite_jobs_prevent_a_descendant_from_running() {
    let temp = fixture();
    let (command, ownership, directory) = setup(temp.path(), "try-descendant", Vec::new());
    let captured = process::run_process(
        command,
        ownership,
        directory,
        Instant::now() + Duration::from_secs(30),
        1024,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!temp.path().join("descendant-ran").exists());
    drop(captured);
    assert_released(temp.path());

    let mut normal = Command::new(std::env::current_exe().unwrap());
    normal
        .args([
            "--exact",
            "lifecycle::native_task_runtime::tests::controlled_task_process",
            "--ignored",
            "--nocapture",
        ])
        .env("LUMINA_TASK_TEST_MODE", "allow-descendant")
        .env("LUMINA_TASK_TEST_ROOT", temp.path())
        .env("LUMINA_TASK_TEST_OUTPUT", temp.path())
        .env("TMP", temp.path())
        .env("TEMP", temp.path());
    let asset = RuntimeAssetGuard::acquire([temp.path().join("asset")]).unwrap();
    // The ordinary server owner still starts a runnable process in its existing job.
    let mut owner = GuardedChild::spawn(
        &mut normal,
        asset,
        Arc::new(acquire_runtime_permit(temp.path()).unwrap()),
        Vec::new(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = owner.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(temp.path().join("descendant-ran").exists());
}

#[test]
fn finite_success_captures_logs_and_holds_original_output_handle() {
    let temp = fixture();
    let (command, ownership, directory) = setup(temp.path(), "success", Vec::new());
    let snapshot_digest = ownership.publication.snapshot_digest.clone();
    let path = directory.path().join("output.txt");
    let scratch = directory.scratch();
    let captured = process::run_process(
        command,
        ownership,
        Arc::clone(&directory),
        Instant::now() + Duration::from_secs(30),
        1024,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(scratch
        .join("audiocpp-gguf/model/tokenizer.model")
        .is_file());
    assert!(acquire_runtime_permit(temp.path()).is_err());
    let mut result = output::collect_outputs(
        directory,
        vec!["output.txt".into()],
        1024,
        captured,
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.snapshot_digest(), &snapshot_digest);
    assert!(acquire_runtime_permit(temp.path()).is_err());
    assert!(String::from_utf8_lossy(&result.stdout).contains("controlled stdout"));
    assert!(String::from_utf8_lossy(&result.stderr).contains("controlled stderr"));
    assert_eq!(
        result.files_mut()[0].read_range(0, 1024).unwrap(),
        b"finite task output"
    );
    let mut reader = result.files()[0].try_clone_reader().unwrap();
    let mut copied = Vec::new();
    reader.read_to_end(&mut copied).unwrap();
    assert_eq!(copied, b"finite task output");
    drop(reader);
    assert!(acquire_runtime_permit(temp.path()).is_err());
    assert_eq!(
        result.files()[0].sha256(),
        &Sha256Digest::of_bytes(b"finite task output")
    );
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    assert!(std::fs::rename(path.parent().unwrap(), temp.path().join("replaced")).is_err());
    assert!(result.files_mut()[0]
        .read_range(0, 1024 * 1024 + 1)
        .is_err());
    drop(result);
    assert!(!scratch.exists());
    assert_released(temp.path());
    assert!(!path.exists());
}

#[tokio::test]
async fn cancelling_polled_future_keeps_input_guard_and_store_busy_until_child_exit() {
    let temp = fixture();
    let child_gone = Arc::new(AtomicBool::new(false));
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let witness = DropWitness {
        root: temp.path().into(),
        child_was_gone: Arc::clone(&child_gone),
        dropped: Some(dropped_tx),
    };
    let (command, ownership, directory) = setup(temp.path(), "wait", vec![Box::new(witness)]);
    let task = tokio::spawn(run_cancellable(
        Duration::from_secs(60),
        std::future::pending(),
        move |deadline, cancelled| {
            process::run_process(command, ownership, directory, deadline, 1024, cancelled)
        },
    ));
    ready(temp.path()).await;
    assert!(acquire_runtime_permit(temp.path()).is_err());
    let asset = File::open(temp.path().join("asset")).unwrap();
    assert!(FileExt::try_lock_exclusive(&asset).is_err());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(20), dropped_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(child_gone.load(Ordering::Acquire));
    tokio::time::timeout(Duration::from_secs(20), async {
        while acquire_runtime_permit(temp.path()).is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_released(temp.path());
}

#[tokio::test]
async fn explicit_cancellation_returns_after_the_child_and_permit_are_released() {
    let temp = fixture();
    let (command, ownership, directory) = setup(temp.path(), "wait", Vec::new());
    let (cancel, signal) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run_cancellable(
        Duration::from_secs(60),
        async { signal.await.unwrap() },
        move |deadline, cancelled| {
            process::run_process(command, ownership, directory, deadline, 1024, cancelled)
        },
    ));
    ready(temp.path()).await;
    cancel.send(()).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert!(error.to_string().contains("cancelled"));
    assert_released(temp.path());
    acquire_runtime_permit(temp.path()).unwrap();
}

#[test]
fn deadline_expiry_reaps_the_controlled_process() {
    let temp = fixture();
    let (command, ownership, directory) = setup(temp.path(), "wait", Vec::new());
    let error = process::run_process(
        command,
        ownership,
        directory,
        Instant::now() + Duration::from_secs(2),
        1024,
        &AtomicBool::new(false),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("timed out"));
    // A deadline can expire before the child writes its marker under heavy load.
    if temp.path().join("alive.lock").exists() {
        assert_released(temp.path());
    }
    acquire_runtime_permit(temp.path()).unwrap();
}

#[test]
fn stdout_limit_is_bounded_and_terminates_the_writer() {
    let temp = fixture();
    let (command, ownership, directory) = setup(temp.path(), "flood", Vec::new());
    let error = process::run_process(
        command,
        ownership,
        directory,
        Instant::now() + Duration::from_secs(30),
        1024,
        &AtomicBool::new(false),
    )
    .err()
    .unwrap();
    let failure = error.downcast_ref::<NativeTaskFailure>().unwrap();
    assert!(failure.reason.contains("log exceeded"));
    assert_eq!(failure.stdout.len(), 1024 * 1024);
    assert!(failure.stderr.len() <= 1024 * 1024);
    assert_released(temp.path());
}

#[test]
fn total_output_limit_and_nonzero_exit_never_return_success() {
    for (mode, reason) in [
        ("oversize", "output byte limit"),
        ("oversize-scratch", "temporary disk budget"),
        ("failure", "exit status"),
    ] {
        let temp = fixture();
        let (command, ownership, directory) = setup(temp.path(), mode, Vec::new());
        let error = process::run_process(
            command,
            ownership,
            directory,
            Instant::now() + Duration::from_secs(30),
            1024,
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
        let failure = error.downcast_ref::<NativeTaskFailure>().unwrap();
        assert!(failure.reason.contains(reason), "{}", failure.reason);
        if mode == "failure" {
            assert_eq!(failure.exit_code, Some(17));
        }
        assert_released(temp.path());
    }
}

#[test]
fn collection_rejects_extra_files_missing_outputs_and_hardlinks() {
    let temp = fixture();
    let directory = Arc::new(output::GuardedTaskDirectory::create(temp.path()).unwrap());
    std::fs::write(directory.path().join("other.txt"), b"not requested").unwrap();
    assert!(output::collect_outputs(
        Arc::clone(&directory),
        vec!["output.txt".into()],
        1024,
        process::CapturedProcess {
            stdout: vec![],
            stderr: vec![],
            publication: publication_lease(temp.path()),
        },
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .is_err());
    std::fs::remove_file(directory.path().join("other.txt")).unwrap();
    std::fs::write(temp.path().join("original"), b"external source").unwrap();
    std::fs::hard_link(
        temp.path().join("original"),
        directory.path().join("output.txt"),
    )
    .unwrap();
    assert!(output::collect_outputs(
        directory,
        vec!["output.txt".into()],
        1024,
        process::CapturedProcess {
            stdout: vec![],
            stderr: vec![],
            publication: publication_lease(temp.path()),
        },
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .is_err());
    assert_eq!(
        std::fs::read(temp.path().join("original")).unwrap(),
        b"external source"
    );
}
