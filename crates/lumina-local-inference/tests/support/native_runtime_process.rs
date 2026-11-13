use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

#[test]
fn helper_startup_requires_current_host_memory_for_the_installed_workload() {
    use crate::lifecycle::{HardwareMemoryDevice, HardwareResourceSnapshot};
    let mut hardware = HardwareResourceSnapshot {
        observed_at: 100,
        devices: vec![HardwareMemoryDevice {
            index: 0,
            backend: "CPU".into(),
            name: "CPU".into(),
            description: "fixture".into(),
            kind: "cpu".into(),
            total_bytes: 4096,
            free_bytes: 2048,
        }],
        storage_volume: "test".into(),
        disk_free_bytes: 0,
    };
    let admitted = HelperMemoryAdmission::from_observation(Some(2048), &hardware).unwrap();
    assert_eq!(
        (
            admitted.available_bytes,
            admitted.required_bytes,
            admitted.observed_at
        ),
        (2048, 2048, 100)
    );
    for missing in [None, Some(0), Some(2049)] {
        assert!(HelperMemoryAdmission::from_observation(missing, &hardware).is_err());
    }
    hardware.devices[0].free_bytes = 1;
    assert!(HelperMemoryAdmission::from_observation(Some(2048), &hardware).is_err());
    hardware.devices[0].free_bytes = 8192;
    assert!(HelperMemoryAdmission::from_observation(Some(2048), &hardware).is_err());
    hardware.devices[0].free_bytes = 4096;
    hardware.devices[0].kind = "gpu".into();
    assert!(HelperMemoryAdmission::from_observation(Some(2048), &hardware).is_err());
}

fn wait_until_slot_occupied(root: &Path, deadline: Instant) {
    loop {
        for index in 0..RUNTIME_WAIT_SLOTS {
            let name = format!("wait-{index}");
            if root.join(format!("locks/runtime/{name}.lock")).exists() {
                let slot = runtime_lock(root, &name).unwrap();
                if !try_runtime_lock(&slot).unwrap() {
                    return;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "waiter did not claim a queue slot"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn contended_runtime_queue_bounds_waiters_and_admits_after_owner_release() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let owner = acquire_runtime_permit(root).unwrap();
    let cancelled = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(10);
    let occupied: Vec<File> = (0..RUNTIME_WAIT_SLOTS)
        .map(|index| {
            let slot = runtime_lock(root, &format!("wait-{index}")).unwrap();
            assert!(try_runtime_lock(&slot).unwrap());
            slot
        })
        .collect();
    assert!(wait_for_runtime_permit(root, deadline, &cancelled)
        .unwrap_err()
        .to_string()
        .contains("queue is full"));
    drop(occupied);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| wait_for_runtime_permit(root, deadline, &cancelled));
        wait_until_slot_occupied(root, deadline);
        drop(owner);
        let acquired = waiter.join().unwrap().unwrap();
        assert!(acquire_runtime_permit(root).is_err());
        drop(acquired);
    });
    acquire_runtime_permit(root).unwrap();
}

#[test]
fn timeout_and_cancellation_leave_the_active_helper_and_wait_slot_intact() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let _owner = acquire_runtime_permit(root).unwrap();
    let cancelled = AtomicBool::new(false);
    let short = Instant::now() + Duration::from_millis(100);
    assert!(wait_for_runtime_permit(root, short, &cancelled)
        .unwrap_err()
        .to_string()
        .contains("Timed out"));
    let deadline = Instant::now() + Duration::from_secs(10);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| wait_for_runtime_permit(root, deadline, &cancelled));
        wait_until_slot_occupied(root, deadline);
        cancelled.store(true, Ordering::Release);
        assert!(waiter
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
    });
    assert!(acquire_runtime_permit(root).is_err());
    assert!(try_runtime_lock(&runtime_lock(root, "wait-0").unwrap()).unwrap());
}

fn text_config() -> LaunchConfig {
    let fixture: Value = serde_json::from_str(include_str!(
        "../fixtures/lifecycle-vision/frozen-text-v2-plan.json"
    ))
    .unwrap();
    let plan: super::super::InstallPlan = serde_json::from_value(fixture["plan"].clone()).unwrap();
    let workload = plan.admission.unwrap().envelope.workload;
    LaunchConfig {
        installation_id: "argument-fixture".into(),
        version_id: "argument-version".into(),
        manifest_digest: Sha256Digest::of_bytes(b"argument fixture only, not a runtime"),
        runtime: plan.runtime,
        capabilities: BTreeSet::from([super::super::ModelCapability::Text]),
        component_files: BTreeMap::from([(
            ComponentRole::Main,
            vec![RepositoryPath::parse("main.gguf").unwrap()],
        )]),
        parameters: RuntimeParameters::LlamaCpp {
            chat_template: LaunchChatTemplate::Embedded,
            context_length: workload.context_tokens,
            gpu_layers: 0,
            cpu_threads: 4,
            kv_key_type: "f16".into(),
            kv_value_type: "f16".into(),
            batch_size: workload.batch_tokens,
            flash_attention: true,
            workload: Some(workload),
        },
        mode: RuntimeExecutionMode::Helper { port: 32123 },
    }
}

fn assert_option(args: &[OsString], flag: &str, expected: &str) {
    let values = args
        .windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(values, [expected], "{flag}");
}

#[tokio::test]
async fn incomplete_native_operation_requires_a_new_runtime() {
    let gate = NativeRequestGate::default();
    let mut operation = gate.acquire().await.unwrap();
    operation.sending();
    drop(operation);
    assert!(gate.acquire().await.is_err());
    assert!(gate.acquire().await.is_err());
}

#[tokio::test]
async fn cancelling_a_waiter_does_not_release_or_poison_the_active_operation() {
    let gate = NativeRequestGate::default();
    let mut operation = gate.acquire().await.unwrap();
    operation.sending();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), gate.acquire())
            .await
            .is_err()
    );
    operation.finished();
    drop(operation);
    let mut next = gate.acquire().await.unwrap();
    next.sending();
    next.finished();
    drop(next);
    gate.acquire().await.unwrap();
}

#[test]
fn measured_text_arguments_disable_automatic_fit_and_unbudgeted_caches() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("main.gguf"), b"argument-only fixture").unwrap();
    let args = helper_arguments(&text_config(), temp.path(), "bound-instance").unwrap();
    for (flag, value) in [
        ("--host", "127.0.0.1"),
        ("--port", "32123"),
        ("--alias", "bound-instance"),
        ("--ctx-size", "2048"),
        ("--batch-size", "512"),
        ("--ubatch-size", "512"),
        ("--parallel", "1"),
        ("--threads", "4"),
        ("--threads-batch", "4"),
        ("--gpu-layers", "0"),
        ("--cache-type-k", "f16"),
        ("--cache-type-v", "f16"),
        ("--flash-attn", "on"),
        ("--cache-ram", "0"),
        ("--ctx-checkpoints", "0"),
        ("--spec-type", "none"),
        ("--fit", "off"),
        ("--log-verbosity", "3"),
    ] {
        assert_option(&args, flag, value);
    }
    for flag in [
        "--offline",
        "--no-kv-offload",
        "--no-context-shift",
        "--no-warmup",
    ] {
        assert!(args.iter().any(|value| value == flag), "{flag}");
    }
    assert!(!args
        .iter()
        .any(|value| value.to_string_lossy().contains("api-key")));
}

#[test]
fn vision_arguments_bind_projector_encoder_limits_and_microbatch() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("main.gguf"), b"argument-only main").unwrap();
    std::fs::write(
        temp.path().join("projector.gguf"),
        b"argument-only projector",
    )
    .unwrap();
    let mut config = text_config();
    config.component_files.insert(
        ComponentRole::VisionProjector,
        vec![RepositoryPath::parse("projector.gguf").unwrap()],
    );
    let RuntimeParameters::LlamaCpp {
        workload: Some(workload),
        context_length,
        ..
    } = &mut config.parameters
    else {
        unreachable!()
    };
    let vision = super::super::VisionResourceWorkload::measured_qwen35_cpu();
    workload.vision = Some(vision.clone());
    *context_length = 4096;
    let args = helper_arguments(&config, temp.path(), "vision-instance").unwrap();
    assert_option(
        &args,
        "--image-min-tokens",
        &vision.native_min_image_tokens.to_string(),
    );
    assert_option(
        &args,
        "--image-max-tokens",
        &vision.native_max_image_tokens.to_string(),
    );
    assert_option(
        &args,
        "--mtmd-batch-max-tokens",
        &vision.image_batch_tokens.to_string(),
    );
    assert_option(
        &args,
        "--ubatch-size",
        &vision.microbatch_tokens.to_string(),
    );
    assert_option(
        &args,
        "--mmproj",
        &temp
            .path()
            .join("projector.gguf")
            .canonicalize()
            .unwrap()
            .to_string_lossy(),
    );
    assert!(args.iter().any(|value| value == "--no-mmproj-offload"));
}

#[test]
fn vector_arguments_bind_the_measured_task_without_chat_or_projector_options() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("main.gguf"),
        b"argument fixture, not model weights",
    )
    .unwrap();
    for profile in [
        VectorRuntimeProfile::Qwen3Embedding06bQ8,
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
    ] {
        let mut config = text_config();
        config.parameters = profile.launch_parameters();
        config.capabilities = BTreeSet::from([profile.capability()]);
        let args = helper_arguments(&config, temp.path(), "vector-instance").unwrap();
        for (flag, value) in [
            ("--alias", "vector-instance"),
            ("--ctx-size", "512"),
            ("--batch-size", "512"),
            ("--ubatch-size", "512"),
            ("--parallel", "1"),
            ("--threads", "4"),
            ("--threads-batch", "4"),
            ("--gpu-layers", "0"),
            ("--flash-attn", "on"),
            ("--cache-type-k", "f16"),
            ("--cache-type-v", "f16"),
            ("--cache-ram", "0"),
            ("--ctx-checkpoints", "0"),
            ("--cache-reuse", "0"),
            ("--spec-type", "none"),
            ("--fit", "off"),
        ] {
            assert_option(&args, flag, value);
        }
        for flag in [
            "--offline",
            "--no-webui",
            "--no-warmup",
            "--no-cache-prompt",
            "--no-cache-idle-slots",
        ] {
            assert!(args.iter().any(|value| value == flag), "{flag}");
        }
        for flag in ["--mmproj", "--chat-template", "--draft-model"] {
            assert!(!args.iter().any(|value| value == flag), "{flag}");
        }
        assert_eq!(
            args.iter().any(|value| value == "--embedding"),
            profile == VectorRuntimeProfile::Qwen3Embedding06bQ8
        );
        assert_eq!(
            args.iter().any(|value| value == "--reranking"),
            profile == VectorRuntimeProfile::BgeRerankerV2M3Q4Km
        );
        if profile == VectorRuntimeProfile::Qwen3Embedding06bQ8 {
            assert_option(&args, "--pooling", "last");
            assert_option(&args, "--embd-normalize", "2");
        }
    }
}

#[test]
fn vector_properties_reject_wrong_process_identity_or_execution_settings() {
    let model = Path::new("fixed-model.gguf");
    let expected = serde_json::json!({
        "model_alias":"bound-alias", "model_path":model,
        "default_generation_settings":{"n_ctx":512,"params":{"speculative.types":"none"}},
        "total_slots":1,"modalities":{"vision":false},
    });
    for profile in [
        VectorRuntimeProfile::Qwen3Embedding06bQ8,
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
    ] {
        let workload = profile.resource_workload();
        validate_model_properties(&expected, &workload, model, "bound-alias").unwrap();
        for (pointer, value) in [
            ("/model_alias", serde_json::json!("another-process")),
            ("/model_path", serde_json::json!("other-model.gguf")),
            (
                "/default_generation_settings/n_ctx",
                serde_json::json!(2048),
            ),
            (
                "/default_generation_settings/params/speculative.types",
                serde_json::json!("draft-mtp"),
            ),
            ("/total_slots", serde_json::json!(2)),
            ("/modalities/vision", serde_json::json!(true)),
        ] {
            let mut changed = expected.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                validate_model_properties(&changed, &workload, model, "bound-alias").is_err(),
                "{pointer}"
            );
        }
    }
}

#[test]
fn argument_builder_rejects_unmeasured_split_main_and_unbound_parameters() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("main.gguf"), b"fixture").unwrap();
    let mut config = text_config();
    config
        .component_files
        .get_mut(&ComponentRole::Main)
        .unwrap()
        .push(RepositoryPath::parse("second.gguf").unwrap());
    assert!(helper_arguments(&config, temp.path(), "alias").is_err());
    config.parameters = RuntimeParameters::Specialized {
        schema_id: "vector-diagnostic".into(),
        settings: BTreeMap::new(),
    };
    assert!(helper_arguments(&config, temp.path(), "alias").is_err());
}

#[test]
fn inline_template_digest_is_verified_before_native_execution() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("main.gguf"), b"fixture").unwrap();
    let mut config = text_config();
    let RuntimeParameters::LlamaCpp { chat_template, .. } = &mut config.parameters else {
        unreachable!()
    };
    *chat_template = LaunchChatTemplate::Inline {
        template: "changed".into(),
        digest: Sha256Digest::of_bytes(b"original"),
    };
    assert!(helper_arguments(&config, temp.path(), "alias")
        .unwrap_err()
        .to_string()
        .contains("template identity"));
}

#[test]
fn binary_identity_and_single_store_runtime_permit_are_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("helper-bytes");
    std::fs::write(&binary, b"not an executable").unwrap();
    verify_binary(&binary, &Sha256Digest::of_bytes(b"not an executable")).unwrap();
    assert!(verify_binary(&binary, &Sha256Digest::of_bytes(b"different runtime")).is_err());
    let permit = acquire_runtime_permit(temp.path()).unwrap();
    assert!(acquire_runtime_permit(temp.path()).is_err());
    drop(permit);
    acquire_runtime_permit(temp.path()).unwrap();
}

#[test]
#[ignore = "controlled child process for owner lifetime tests"]
fn child_holds_liveness_lock() {
    let root = PathBuf::from(
        std::env::var_os("LUMINA_OWNER_TEST_CHILD_ROOT").expect("spawned child root"),
    );
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(root.join("child-alive.lock"))
        .unwrap();
    FileExt::lock_exclusive(&lock).unwrap();
    std::fs::write(root.join("child-ready"), b"locked").unwrap();
    std::thread::sleep(Duration::from_secs(120));
}

fn controlled_child(root: &Path) -> GuardedChild {
    let asset_path = root.join("runtime-asset");
    std::fs::write(&asset_path, b"small process ownership fixture").unwrap();
    let assets = RuntimeAssetGuard::acquire([asset_path]).unwrap();
    let permit = acquire_runtime_permit(root).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "lifecycle::native_runtime::process_tests::child_holds_liveness_lock",
            "--ignored",
            "--nocapture",
        ])
        .env("LUMINA_OWNER_TEST_CHILD_ROOT", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(windows)]
    let job = windows_job::KillOnCloseJob::new().unwrap();
    let mut owner = GuardedChild {
        child: Some(command.spawn().unwrap()),
        exit_status: None,
        assets: Some(assets),
        permit: Some(permit),
        #[cfg(windows)]
        job,
    };
    #[cfg(windows)]
    owner.job.assign(owner.child.as_ref().unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("child-ready").exists() {
        assert!(
            owner.try_wait().unwrap().is_none(),
            "controlled child exited before locking its liveness file"
        );
        assert!(
            Instant::now() < deadline,
            "controlled child did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    owner
}

fn asset_is_exclusively_available(root: &Path) -> bool {
    let file = File::open(root.join("runtime-asset")).unwrap();
    FileExt::try_lock_exclusive(&file).is_ok()
}

fn assert_child_exited_and_guards_released(root: &Path) {
    let liveness = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("child-alive.lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&liveness).expect("child must exit before owner releases guards");
    assert!(asset_is_exclusively_available(root));
    acquire_runtime_permit(root).unwrap();
}

#[test]
fn explicit_stop_reaps_child_before_releasing_assets_and_permit() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = controlled_child(temp.path());
    assert!(!asset_is_exclusively_available(temp.path()));
    assert!(acquire_runtime_permit(temp.path()).is_err());
    let status = owner.stop().unwrap();
    assert_eq!(owner.stop().unwrap(), status);
    assert!(owner.child.is_none());
    assert_child_exited_and_guards_released(temp.path());
}

#[test]
fn error_unwind_reaps_child_and_only_then_releases_runtime_assets() {
    let temp = tempfile::tempdir().unwrap();
    let result: Result<()> = (|| {
        let _owner = controlled_child(temp.path());
        assert!(!asset_is_exclusively_available(temp.path()));
        bail!("controlled startup failure after child creation")
    })();
    assert!(result.is_err());
    assert_child_exited_and_guards_released(temp.path());
}

#[tokio::test]
async fn cancelled_blocking_preparation_drops_its_eventual_child_owner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    struct DropWitness(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for DropWitness {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let preparation = tokio::task::spawn_blocking(move || {
        let owner = controlled_child(&root);
        ready_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        (owner, DropWitness(Some(dropped_tx)))
    });
    ready_rx.await.unwrap();
    assert!(!asset_is_exclusively_available(temp.path()));
    drop(preparation);
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), dropped_rx)
        .await
        .unwrap()
        .unwrap();
    assert_child_exited_and_guards_released(temp.path());
}

#[test]
fn preparation_cancellation_is_visible_to_the_blocking_owner() {
    let cancellation = PreparationCancellation(Arc::new(AtomicBool::new(false)));
    let observer = Arc::clone(&cancellation.0);
    drop(cancellation);
    assert!(observer.load(Ordering::Acquire));
}

#[cfg(windows)]
#[test]
#[ignore = "controlled parent process for abrupt-exit job tests"]
fn owner_parent_holds_child() {
    let root = PathBuf::from(
        std::env::var_os("LUMINA_OWNER_TEST_PARENT_ROOT").expect("spawned parent root"),
    );
    let owner = controlled_child(&root);
    std::fs::write(
        root.join("owner-ready.tmp"),
        owner.child.as_ref().unwrap().id().to_string(),
    )
    .unwrap();
    std::fs::rename(root.join("owner-ready.tmp"), root.join("owner-ready")).unwrap();
    std::thread::sleep(Duration::from_secs(120));
    drop(owner);
}

#[cfg(windows)]
#[test]
fn windows_parent_termination_closes_job_and_reaps_child() {
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    }

    let temp = tempfile::tempdir().unwrap();
    let job = windows_job::KillOnCloseJob::new().unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lifecycle::native_runtime::process_tests::owner_parent_holds_child",
            "--ignored",
            "--nocapture",
        ])
        .env("LUMINA_OWNER_TEST_PARENT_ROOT", temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap();
    let mut parent = GuardedChild {
        child: Some(child),
        exit_status: None,
        assets: None,
        permit: None,
        job,
    };
    parent.job.assign(parent.child.as_ref().unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let ready = temp.path().join("owner-ready");
    while !ready.exists() {
        assert!(
            parent.try_wait().unwrap().is_none(),
            "controlled parent exited before assigning its child"
        );
        assert!(
            Instant::now() < deadline,
            "controlled parent did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid: u32 = std::fs::read_to_string(ready).unwrap().parse().unwrap();
    let raw = unsafe { OpenProcess(0x0010_0000, 0, pid) };
    assert!(
        !raw.is_null(),
        "child process must exist before parent is killed"
    );
    let child_handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    assert_eq!(
        unsafe { WaitForSingleObject(child_handle.as_raw_handle(), 0) },
        0x0000_0102
    );
    assert!(!asset_is_exclusively_available(temp.path()));
    assert!(acquire_runtime_permit(temp.path()).is_err());
    parent.stop().unwrap();
    // The outer job remains open here. Only the abruptly killed parent's own
    // kill-on-close job can account for the inner process reaching signaled.
    assert_eq!(
        unsafe { WaitForSingleObject(child_handle.as_raw_handle(), 10_000) },
        0
    );
    assert_child_exited_and_guards_released(temp.path());
}
