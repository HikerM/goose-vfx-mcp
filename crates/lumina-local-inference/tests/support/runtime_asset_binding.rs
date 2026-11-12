use super::*;
use crate::backend::LocalGenerationRequest;
use crate::local_model_registry::{ChatTemplate, SamplingConfig};
use crate::ModelCacheKey;
use fs2::FileExt;
use std::any::Any;
use std::fs::File;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

struct Fixture {
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/runtime-binding-tests");
        std::fs::create_dir_all(&root).unwrap();
        Self {
            temp: tempfile::Builder::new()
                .prefix("binding-")
                .tempdir_in(root)
                .unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn gguf(&self, name: &str, split: Option<(u16, u16)>) -> PathBuf {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        let metadata_count = if split.is_some() { 3u64 } else { 0 };
        bytes.extend(metadata_count.to_le_bytes());
        if let Some((index, count)) = split {
            for (key, kind, value) in [
                ("split.no", 2u32, index.to_le_bytes().to_vec()),
                ("split.count", 2u32, count.to_le_bytes().to_vec()),
                ("split.tensors.count", 5u32, 0i32.to_le_bytes().to_vec()),
            ] {
                bytes.extend((key.len() as u64).to_le_bytes());
                bytes.extend(key.as_bytes());
                bytes.extend(kind.to_le_bytes());
                bytes.extend(value);
            }
        }
        self.file(name, &bytes)
    }

    fn resolved(&self, main: PathBuf) -> ResolvedModelPaths {
        ResolvedModelPaths {
            model_path: main,
            shard_paths: Vec::new(),
            context_limit: 8192,
            settings: ModelSettings {
                context_size: Some(8192),
                ..ModelSettings::default()
            },
            mmproj_path: None,
            backend_id: None,
            draft_model_path: None,
            draft_shard_paths: Vec::new(),
        }
    }

    fn gguf_metadata(&self, name: &str, entries: &[(&[u8], u32, Vec<u8>)]) -> PathBuf {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend((entries.len() as u64).to_le_bytes());
        for (key, kind, value) in entries {
            bytes.extend((key.len() as u64).to_le_bytes());
            bytes.extend(*key);
            bytes.extend(kind.to_le_bytes());
            bytes.extend(value);
        }
        self.file(name, &bytes)
    }
}

fn gguf_string(bytes: &[u8]) -> Vec<u8> {
    let mut value = (bytes.len() as u64).to_le_bytes().to_vec();
    value.extend(bytes);
    value
}

fn is_locked(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    FileExt::try_lock_exclusive(&file)
        .is_err_and(|error| error.raw_os_error() == fs2::lock_contended_error().raw_os_error())
}

fn assert_released(paths: &[PathBuf]) {
    for path in paths {
        let probe = File::open(path).unwrap();
        FileExt::try_lock_exclusive(&probe)
            .unwrap_or_else(|error| panic!("Asset remained locked: {}: {error}", path.display()));
    }
}

#[derive(Default)]
struct Evidence {
    loads: AtomicUsize,
    locked_at_load: AtomicBool,
    locked_after_gate: AtomicBool,
    drops: AtomicUsize,
    locked_at_drop: AtomicBool,
}

struct LoadGate {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

struct FakeBackend {
    id: &'static str,
    assets: Vec<PathBuf>,
    evidence: Arc<Evidence>,
    fail: bool,
    gate: Option<LoadGate>,
}

impl FakeBackend {
    fn new(id: &'static str, assets: Vec<PathBuf>) -> Self {
        Self {
            id,
            assets,
            evidence: Arc::new(Evidence::default()),
            fail: false,
            gate: None,
        }
    }
}

struct FakeLoadedModel {
    assets: Vec<PathBuf>,
    evidence: Arc<Evidence>,
}

impl BackendLoadedModel for FakeLoadedModel {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Drop for FakeLoadedModel {
    fn drop(&mut self) {
        self.evidence.locked_at_drop.store(
            self.assets.iter().all(|path| is_locked(path)),
            Ordering::SeqCst,
        );
        self.evidence.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl LocalInferenceBackend for FakeBackend {
    fn id(&self) -> &'static str {
        self.id
    }

    fn load_model(
        &self,
        _model_id: &str,
        _resolved: &ResolvedModelPaths,
        _settings: &ModelSettings,
    ) -> Result<Box<dyn BackendLoadedModel>, ProviderError> {
        self.evidence.loads.fetch_add(1, Ordering::SeqCst);
        let protected = self.assets.iter().all(|path| is_locked(path));
        self.evidence
            .locked_at_load
            .store(protected, Ordering::SeqCst);
        assert!(
            protected,
            "Backend was called before all assets were protected"
        );
        if let Some(gate) = &self.gate {
            gate.entered.send(()).unwrap();
            gate.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            self.evidence.locked_after_gate.store(
                self.assets.iter().all(|path| is_locked(path)),
                Ordering::SeqCst,
            );
        }
        if self.fail {
            return Err(ProviderError::ExecutionError(
                "planned backend failure".into(),
            ));
        }
        Ok(Box::new(FakeLoadedModel {
            assets: self.assets.clone(),
            evidence: self.evidence.clone(),
        }))
    }

    fn generate(
        &self,
        _loaded: &mut dyn BackendLoadedModel,
        _request: LocalGenerationRequest<'_>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::ExecutionError(
            "Test backend does not generate".into(),
        ))
    }

    fn available_memory_bytes(&self) -> u64 {
        0
    }
}

#[test]
fn native_loading_protects_unregistered_main_siblings() {
    let fixture = Fixture::new();
    let main = fixture.gguf("main-00001-of-00002.gguf", Some((0, 2)));
    let main_sibling = fixture.gguf("main-00002-of-00002.gguf", Some((1, 2)));
    let projector = fixture.gguf("projector.gguf", None);
    let mut resolved = fixture.resolved(main.clone());
    resolved.mmproj_path = Some(projector.clone());
    assert!(resolved.shard_paths.is_empty());
    let paths = vec![main, main_sibling, projector];
    let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, paths.clone());
    let handle =
        LoadedModelHandle::load(&backend, "native", &resolved, &resolved.settings).unwrap();
    assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 1);
    assert!(backend.evidence.locked_at_load.load(Ordering::SeqCst));
    assert_eq!(handle._assets.paths().len(), paths.len());
    assert!(paths.iter().all(|path| is_locked(path)));
    drop(handle);
    assert_released(&paths);
}

#[test]
fn missing_native_sibling_fails_before_backend_and_releases_initial_guards() {
    let fixture = Fixture::new();
    let main = fixture.gguf("main-00001-of-00002.gguf", Some((0, 2)));
    let projector = fixture.gguf("projector.gguf", None);
    let existing = vec![main.clone(), projector.clone()];
    let mut resolved = fixture.resolved(main);
    resolved.mmproj_path = Some(projector);
    let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, existing.clone());
    let error = LoadedModelHandle::load(&backend, "missing", &resolved, &resolved.settings)
        .err()
        .expect("Missing sibling must fail");
    assert!(error.to_string().contains("main-00002-of-00002.gguf"));
    assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 0);
    assert_released(&existing);
}

#[test]
fn llama_loading_ignores_unused_mlx_non_gguf_and_missing_drafts() {
    let fixture = Fixture::new();
    let main = fixture.gguf("model.gguf", None);
    let mlx_weights = fixture.file("mlx-draft/model.safetensors", b"MLX weights");
    let non_gguf = fixture.file("draft.bin", b"not GGUF");
    let unused_shard = fixture.file("draft-shard.bin", b"unused shard");
    let exclusive = File::open(&unused_shard).unwrap();
    FileExt::try_lock_exclusive(&exclusive).unwrap();
    for draft in [
        fixture.path("mlx-draft"),
        non_gguf.clone(),
        fixture.path("missing-draft.gguf"),
    ] {
        let mut resolved = fixture.resolved(main.clone());
        resolved.draft_model_path = Some(draft);
        resolved.draft_shard_paths = vec![
            unused_shard.clone(),
            fixture.path("missing-draft-shard.gguf"),
        ];
        let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, vec![main.clone()]);
        let handle =
            LoadedModelHandle::load(&backend, "unused-draft", &resolved, &resolved.settings)
                .unwrap();
        assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 1);
        assert_eq!(handle._assets.paths(), &[main.canonicalize().unwrap()]);
        assert_released(&[mlx_weights.clone(), non_gguf.clone()]);
        drop(handle);
        assert_released(std::slice::from_ref(&main));
    }
    drop(exclusive);
    assert_released(&[unused_shard]);
}

#[test]
fn split_looking_filename_without_split_metadata_does_not_lock_a_sibling() {
    let fixture = Fixture::new();
    let main = fixture.gguf("unsplit-00001-of-00002.gguf", None);
    let unrelated = fixture.gguf("unsplit-00002-of-00002.gguf", None);
    let exclusive = File::open(&unrelated).unwrap();
    FileExt::try_lock_exclusive(&exclusive).unwrap();
    let resolved = fixture.resolved(main.clone());
    let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, vec![main.clone()]);
    let handle =
        LoadedModelHandle::load(&backend, "unsplit", &resolved, &resolved.settings).unwrap();
    assert_eq!(handle._assets.paths(), &[main.canonicalize().unwrap()]);
    assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 1);
    drop(handle);
    drop(exclusive);
    assert_released(&[main, unrelated]);
}

#[test]
fn mlx_wrapper_and_generation_draft_guard_protect_their_own_directories() {
    let fixture = Fixture::new();
    let paths = vec![
        fixture.file("mlx/model.safetensors", b"weight bytes"),
        fixture.file("mlx/config.json", b"{}"),
        fixture.file("mlx/tokenizer/tokenizer.json", b"{}"),
        fixture.file("mlx/tokenizer/vocab.txt", b"token"),
        fixture.file("draft/model.safetensors", b"draft weights"),
        fixture.file("draft/tokenizer.json", b"{}"),
        fixture.file("draft/nested/special_tokens_map.json", b"{}"),
    ];
    for main_is_file in [false, true] {
        let main = if main_is_file {
            paths[0].clone()
        } else {
            fixture.path("mlx")
        };
        let mut resolved = fixture.resolved(main);
        resolved.draft_model_path = Some(fixture.path("draft"));
        resolved
            .draft_shard_paths
            .push(fixture.path("missing-unused-draft-shard"));
        let backend = FakeBackend::new(crate::mlx::MLX_BACKEND_ID, paths[..4].to_vec());
        let handle =
            LoadedModelHandle::load(&backend, "mlx", &resolved, &resolved.settings).unwrap();
        assert_eq!(handle._assets.paths().len(), 4);
        assert!(backend.evidence.locked_at_load.load(Ordering::SeqCst));
        assert_released(&paths[4..]);
        let draft_guard = acquire_mlx_assets(&fixture.path("draft")).unwrap();
        assert_eq!(draft_guard.paths().len(), 3);
        assert!(paths.iter().all(|path| is_locked(path)));
        drop(draft_guard);
        assert_released(&paths[4..]);
        assert!(paths[..4].iter().all(|path| is_locked(path)));
        drop(handle);
        assert_released(&paths);
    }
}

#[test]
fn native_split_scan_skips_non_utf8_and_large_metadata_before_locking_siblings() {
    let fixture = Fixture::new();
    let mut string_array = 8u32.to_le_bytes().to_vec();
    string_array.extend(2u64.to_le_bytes());
    string_array.extend(gguf_string(&[0xff, 0xfe]));
    string_array.extend(gguf_string(b"token"));
    let main = fixture.gguf_metadata(
        "main-00001-of-00002.gguf",
        &[
            (b"custom.\xff", 8, gguf_string(&[0xff, 0xfe])),
            (b"tokenizer.ggml.tokens", 9, string_array),
            (
                b"custom.large",
                8,
                gguf_string(&vec![b'x'; 8 * 1024 * 1024 + 1]),
            ),
            (b"split.count", 2, 2u16.to_le_bytes().to_vec()),
            (b"split.no", 2, 0u16.to_le_bytes().to_vec()),
        ],
    );
    let sibling = fixture.gguf("main-00002-of-00002.gguf", Some((1, 2)));
    let paths = vec![main.clone(), sibling];
    let resolved = fixture.resolved(main);
    let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, paths.clone());
    let handle = LoadedModelHandle::load(
        &backend,
        "metadata-compatible",
        &resolved,
        &resolved.settings,
    )
    .unwrap();
    assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 1);
    assert_eq!(handle._assets.paths().len(), 2);
    assert!(paths.iter().all(|path| is_locked(path)));
    drop(handle);
    assert_released(&paths);
}

#[test]
fn native_single_file_split_count_does_not_require_other_split_fields() {
    let fixture = Fixture::new();
    for count in [0u16, 1] {
        let main = fixture.gguf_metadata(
            "model.gguf",
            &[(b"split.count", 2, count.to_le_bytes().to_vec())],
        );
        let resolved = fixture.resolved(main.clone());
        let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, vec![main.clone()]);
        let handle =
            LoadedModelHandle::load(&backend, "single", &resolved, &resolved.settings).unwrap();
        assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 1);
        drop(handle);
        assert_released(&[main]);
    }
}

#[test]
fn native_split_scan_matches_c_string_keys_when_discovering_siblings() {
    let fixture = Fixture::new();
    let main = fixture.gguf_metadata(
        "main-00001-of-00002.gguf",
        &[
            (b"split.count\0suffix", 2, 2u16.to_le_bytes().to_vec()),
            (b"split.no\0suffix", 2, 0u16.to_le_bytes().to_vec()),
        ],
    );
    let sibling = fixture.gguf("main-00002-of-00002.gguf", Some((1, 2)));
    let paths = vec![main.clone(), sibling];
    let resolved = fixture.resolved(main);
    let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, paths.clone());
    let handle =
        LoadedModelHandle::load(&backend, "c-string-keys", &resolved, &resolved.settings).unwrap();
    assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 1);
    assert!(paths.iter().all(|path| is_locked(path)));
    drop(handle);
    assert_released(&paths);
}

#[test]
fn native_split_scan_does_not_apply_the_managed_header_budget() {
    let fixture = Fixture::new();
    let string_bytes = 64u64 * 1024 * 1024 + 1;
    let main = fixture.gguf_metadata(
        "model.gguf",
        &[(b"custom.large", 8, string_bytes.to_le_bytes().to_vec())],
    );
    let file = std::fs::OpenOptions::new().write(true).open(&main).unwrap();
    file.set_len(file.metadata().unwrap().len() + string_bytes)
        .unwrap();
    drop(file);
    let resolved = fixture.resolved(main.clone());
    let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, vec![main.clone()]);
    let handle =
        LoadedModelHandle::load(&backend, "large-header", &resolved, &resolved.settings).unwrap();
    assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 1);
    drop(handle);
    assert_released(&[main]);
}

#[test]
fn invalid_native_metadata_fails_before_backend_and_releases_guards() {
    let mut truncated_string = 3u64.to_le_bytes().to_vec();
    truncated_string.push(b'x');
    let mut overflow_array = 12u32.to_le_bytes().to_vec();
    overflow_array.extend(u64::MAX.to_le_bytes());
    let mut nested_array = 9u32.to_le_bytes().to_vec();
    nested_array.extend(0u64.to_le_bytes());
    let mut truncated_array = 8u32.to_le_bytes().to_vec();
    truncated_array.extend(1u64.to_le_bytes());
    truncated_array.extend(100u64.to_le_bytes());
    type MetadataEntries<'a> = Vec<(&'a [u8], u32, Vec<u8>)>;
    let cases: Vec<(&str, MetadataEntries<'_>)> = vec![
        (
            "truncated string",
            vec![(b"custom.string", 8, truncated_string)],
        ),
        (
            "oversized string",
            vec![(b"custom.string", 8, u64::MAX.to_le_bytes().to_vec())],
        ),
        ("unknown type", vec![(b"custom.invalid", 99, vec![])]),
        ("nested array", vec![(b"custom.array", 9, nested_array)]),
        ("overflow array", vec![(b"custom.array", 9, overflow_array)]),
        (
            "truncated array",
            vec![(b"custom.array", 9, truncated_array)],
        ),
        (
            "count type",
            vec![(b"split.count", 4, 2u32.to_le_bytes().to_vec())],
        ),
        (
            "missing index",
            vec![(b"split.count", 2, 2u16.to_le_bytes().to_vec())],
        ),
        (
            "index type",
            vec![
                (b"split.count", 2, 2u16.to_le_bytes().to_vec()),
                (b"split.no", 4, 0u32.to_le_bytes().to_vec()),
            ],
        ),
        (
            "not first shard",
            vec![
                (b"split.count", 2, 2u16.to_le_bytes().to_vec()),
                (b"split.no", 2, 1u16.to_le_bytes().to_vec()),
            ],
        ),
        (
            "duplicate split count",
            vec![
                (b"split.count", 2, 2u16.to_le_bytes().to_vec()),
                (b"split.count", 2, 2u16.to_le_bytes().to_vec()),
            ],
        ),
    ];
    for (name, entries) in cases {
        let fixture = Fixture::new();
        let main = fixture.gguf_metadata("invalid.gguf", &entries);
        let companion = fixture.file("companion.bin", b"protected before metadata scan");
        let paths = vec![main.clone(), companion.clone()];
        let mut resolved = fixture.resolved(main);
        resolved.mmproj_path = Some(companion);
        let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, paths.clone());
        let error = LoadedModelHandle::load(&backend, name, &resolved, &resolved.settings)
            .err()
            .expect(name);
        assert!(
            error.to_string().contains("GGUF"),
            "Unexpected error for {name}: {error}"
        );
        assert_eq!(
            backend.evidence.loads.load(Ordering::SeqCst),
            0,
            "Invalid metadata reached backend: {name}"
        );
        assert_released(&paths);
    }
}

#[test]
fn backend_destructor_runs_while_asset_guard_is_still_alive() {
    let fixture = Fixture::new();
    let main = fixture.gguf("model.gguf", None);
    let projector = fixture.gguf("projector.gguf", None);
    let mut resolved = fixture.resolved(main.clone());
    resolved.mmproj_path = Some(projector.clone());
    let paths = vec![main, projector];
    let backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, paths.clone());
    let mut handle =
        LoadedModelHandle::load(&backend, "drop-order", &resolved, &resolved.settings).unwrap();
    assert!(handle.as_mut().as_any_mut().is::<FakeLoadedModel>());
    assert_eq!(backend.evidence.drops.load(Ordering::SeqCst), 0);
    drop(handle);
    assert_eq!(backend.evidence.drops.load(Ordering::SeqCst), 1);
    assert!(backend.evidence.locked_at_drop.load(Ordering::SeqCst));
    assert_released(&paths);
}

#[test]
fn backend_load_failure_releases_all_protected_assets() {
    let fixture = Fixture::new();
    let main = fixture.gguf("model.gguf", None);
    let projector = fixture.gguf("projector.gguf", None);
    let mut resolved = fixture.resolved(main.clone());
    resolved.mmproj_path = Some(projector.clone());
    let paths = vec![main, projector];
    let mut backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, paths.clone());
    backend.fail = true;
    let error = LoadedModelHandle::load(&backend, "failed", &resolved, &resolved.settings)
        .err()
        .expect("Backend failure must propagate");
    assert!(error.to_string().contains("planned backend failure"));
    assert_eq!(backend.evidence.loads.load(Ordering::SeqCst), 1);
    assert!(backend.evidence.locked_at_load.load(Ordering::SeqCst));
    assert_eq!(backend.evidence.drops.load(Ordering::SeqCst), 0);
    assert_released(&paths);
}

struct ObservedLoadResult {
    loaded: Option<LoadedModelHandle>,
    released: mpsc::Sender<()>,
}

impl Drop for ObservedLoadResult {
    fn drop(&mut self) {
        drop(self.loaded.take());
        let _ = self.released.send(());
    }
}

#[test]
fn cancelling_load_awaiter_keeps_assets_until_blocking_backend_really_returns() {
    let fixture = Fixture::new();
    let main = fixture.gguf("model.gguf", None);
    let resolved = fixture.resolved(main.clone());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (released_tx, released_rx) = mpsc::channel();
    let mut backend = FakeBackend::new(crate::llamacpp::LLAMACPP_BACKEND_ID, vec![main.clone()]);
    backend.gate = Some(LoadGate {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    });
    let evidence = backend.evidence.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let awaiter = runtime.spawn(async move {
        tokio::task::spawn_blocking(move || {
            let loaded =
                LoadedModelHandle::load(&backend, "cancelled", &resolved, &resolved.settings)
                    .unwrap();
            ObservedLoadResult {
                loaded: Some(loaded),
                released: released_tx,
            }
        })
        .await
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    awaiter.abort();
    let cancelled = runtime.block_on(awaiter);
    assert!(matches!(cancelled, Err(error) if error.is_cancelled()));
    assert!(is_locked(&main));
    assert_eq!(evidence.loads.load(Ordering::SeqCst), 1);
    assert_eq!(evidence.drops.load(Ordering::SeqCst), 0);
    release_tx.send(()).unwrap();
    released_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(evidence.locked_after_gate.load(Ordering::SeqCst));
    assert!(evidence.locked_at_drop.load(Ordering::SeqCst));
    assert_eq!(evidence.drops.load(Ordering::SeqCst), 1);
    assert_released(&[main]);
    runtime.shutdown_timeout(Duration::from_secs(5));
}

#[test]
fn model_cache_identity_changes_for_asset_and_backend_loading_inputs() {
    let fixture = Fixture::new();
    let resolved = fixture.resolved(fixture.gguf("model.gguf", None));
    let settings = resolved.settings.clone();
    let base = ModelCacheKey::new(
        crate::llamacpp::LLAMACPP_BACKEND_ID,
        "model",
        &resolved,
        &settings,
    )
    .unwrap();
    type Change = fn(&mut ResolvedModelPaths, &mut ModelSettings);
    let changes: &[(&str, Change)] = &[
        ("main path", |paths, _| {
            paths.model_path.set_file_name("other.gguf")
        }),
        ("projector", |paths, _| {
            paths.mmproj_path = Some(paths.model_path.with_file_name("projector.gguf"))
        }),
        ("main shards", |paths, _| {
            paths
                .shard_paths
                .push(paths.model_path.with_file_name("shard.gguf"))
        }),
        ("GPU", |_, settings| settings.n_gpu_layers = Some(12)),
        ("context", |_, settings| settings.context_size = Some(4096)),
        ("batch", |_, settings| settings.n_batch = Some(512)),
        ("threads", |_, settings| settings.n_threads = Some(4)),
        ("mlock", |_, settings| settings.use_mlock = true),
        ("flash attention", |_, settings| {
            settings.flash_attention = Some(true)
        }),
        ("template", |_, settings| {
            settings.chat_template = ChatTemplate::CustomInline {
                template: "{{ messages }}".into(),
            }
        }),
    ];
    for (name, change) in changes {
        let mut changed_paths = resolved.clone();
        let mut changed_settings = settings.clone();
        change(&mut changed_paths, &mut changed_settings);
        let key = ModelCacheKey::new(
            crate::llamacpp::LLAMACPP_BACKEND_ID,
            "model",
            &changed_paths,
            &changed_settings,
        )
        .unwrap();
        assert_ne!(key, base, "Cache key ignored {name}");
    }
    assert_ne!(
        ModelCacheKey::new(crate::mlx::MLX_BACKEND_ID, "model", &resolved, &settings).unwrap(),
        base
    );
    assert_ne!(
        ModelCacheKey::new(
            crate::llamacpp::LLAMACPP_BACKEND_ID,
            "other-model",
            &resolved,
            &settings
        )
        .unwrap(),
        base
    );
}

#[test]
fn draft_assets_change_only_the_mlx_cache_identity() {
    let fixture = Fixture::new();
    let resolved = fixture.resolved(fixture.gguf("model.gguf", None));
    type Change = fn(&mut ResolvedModelPaths);
    let changes: &[Change] = &[
        |paths| paths.draft_model_path = Some(paths.model_path.with_file_name("draft")),
        |paths| {
            paths
                .draft_shard_paths
                .push(paths.model_path.with_file_name("draft-shard.safetensors"))
        },
    ];
    for backend in [
        crate::llamacpp::LLAMACPP_BACKEND_ID,
        crate::mlx::MLX_BACKEND_ID,
    ] {
        let base = ModelCacheKey::new(backend, "model", &resolved, &resolved.settings).unwrap();
        for change in changes {
            let mut changed = resolved.clone();
            change(&mut changed);
            let key = ModelCacheKey::new(backend, "model", &changed, &changed.settings).unwrap();
            if backend == crate::mlx::MLX_BACKEND_ID {
                assert_ne!(key, base);
            } else {
                assert_eq!(key, base);
            }
        }
    }
}

#[test]
fn generation_overrides_keep_the_loaded_model_cache_identity() {
    let fixture = Fixture::new();
    let resolved = fixture.resolved(fixture.gguf("model.gguf", None));
    let base = ModelCacheKey::new(
        crate::llamacpp::LLAMACPP_BACKEND_ID,
        "model",
        &resolved,
        &resolved.settings,
    )
    .unwrap();
    let mut generation = resolved.settings.clone();
    generation.sampling = SamplingConfig::Greedy;
    generation.enable_thinking = !generation.enable_thinking;
    generation.max_output_tokens = Some(17);
    generation.repeat_penalty = 1.2;
    let request_key = ModelCacheKey::new(
        crate::llamacpp::LLAMACPP_BACKEND_ID,
        "model",
        &resolved,
        &generation,
    )
    .unwrap();
    assert_eq!(request_key, base);
}
