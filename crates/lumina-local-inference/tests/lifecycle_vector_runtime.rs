use lumina_local_inference::lifecycle::vector_runtime::*;
use lumina_local_inference::lifecycle::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

const TOKEN: &str = "unit-test-helper-credential";

fn binding(profile: VectorRuntimeProfile, port: u16) -> (LaunchConfig, ModelManifest) {
    // Synthetic installation metadata reuses the public content identities only;
    // this fixture does not assert an installation or an actual inference run.
    let hash = Sha256Digest::parse(profile.model_sha256()).unwrap();
    let plan = InstallPlan {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        id: "vector-plan".into(),
        model_id: "vector-model".into(),
        variant_id: "vector-variant".into(),
        target_capabilities: BTreeSet::from([profile.capability()]),
        snapshots: BTreeMap::from([(
            "source".into(),
            RepositorySnapshot {
                repository: ModelScopeRepository::parse("unit/vector-protocol").unwrap(),
                requested_revision: "a".repeat(40),
                resolved_revision: "a".repeat(40),
                files_digest: Sha256Digest::of_bytes(b"synthetic inventory"),
                fetched_at: 100,
            },
        )]),
        artifacts: BTreeMap::from([(
            "main".into(),
            Artifact {
                id: "main".into(),
                snapshot_id: "source".into(),
                path: RepositoryPath::parse("model.gguf").unwrap(),
                purpose: FilePurpose::GgufWeight,
                size_bytes: Some(profile.model_bytes()),
                source_sha256: Some(hash.clone()),
            },
        )]),
        dependencies: DependencyGraph {
            root: "main".into(),
            components: BTreeMap::from([(
                "main".into(),
                Component {
                    id: "main".into(),
                    role: ComponentRole::Main,
                    artifact_ids: BTreeSet::from(["main".into()]),
                    compatibility: ComponentCompatibility::Verified,
                    evidence_ids: BTreeSet::from(["unit-only".into()]),
                },
            )]),
            edges: vec![],
        },
        runtime: baseline_pairing_runtime().unwrap(),
        runtime_verified: true,
        hardware: HardwareCompatibility::Runnable,
        required_disk_bytes: 2 * profile.model_bytes(),
        rules_version: "unit-vector-protocol".into(),
        expires_at: 1000,
        admission: None,
    };
    let manifest = ModelManifest {
        schema_version: MODEL_LIFECYCLE_SCHEMA_VERSION,
        installation_id: "vector-install".into(),
        version_id: "vector-v1".into(),
        plan,
        installed_files: BTreeMap::from([(
            "main".into(),
            InstalledArtifact {
                artifact_id: "main".into(),
                relative_path: RepositoryPath::parse("files/main.gguf").unwrap(),
                computed_sha256: hash,
                size_bytes: profile.model_bytes(),
                verification_evidence: Sha256Digest::of_bytes(b"synthetic verification"),
            },
        )]),
        plan_accepted_at: 101,
        installed_at: 102,
    };
    let launch = LaunchConfig {
        installation_id: manifest.installation_id.clone(),
        version_id: manifest.version_id.clone(),
        manifest_digest: manifest.digest().unwrap(),
        runtime: manifest.plan.runtime.clone(),
        capabilities: BTreeSet::from([profile.capability()]),
        component_files: BTreeMap::from([(
            ComponentRole::Main,
            vec![manifest.installed_files["main"].relative_path.clone()],
        )]),
        parameters: profile.launch_parameters(),
        mode: RuntimeExecutionMode::Helper { port },
    };
    (launch, manifest)
}

#[derive(Clone)]
struct Request {
    path: String,
    headers: String,
    body: Value,
}

struct Reply {
    status: u16,
    content_type: &'static str,
    extra_headers: String,
    body: String,
    delay: Duration,
    chunked: bool,
}

impl Reply {
    fn json(body: Value) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            extra_headers: String::new(),
            body: body.to_string(),
            delay: Duration::ZERO,
            chunked: false,
        }
    }
}

fn read_request_chunk(stream: &mut TcpStream, buffer: &mut [u8]) -> Option<usize> {
    match stream.read(buffer) {
        Ok(0) => None,
        Ok(count) => Some(count),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            None
        }
        Err(error) => panic!("request read failed: {error}"),
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut buffer = [0; 4096];
        let count = read_request_chunk(stream, &mut buffer)?;
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while bytes.len() < header_end + content_length {
        let mut buffer = [0; 4096];
        let count = read_request_chunk(stream, &mut buffer)?;
        bytes.extend_from_slice(&buffer[..count]);
    }
    let path = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .into();
    Some(Request {
        path,
        headers,
        body: serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap(),
    })
}

struct MockHelper {
    launch: LaunchConfig,
    manifest: ModelManifest,
    endpoint: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl MockHelper {
    fn new(
        profile: VectorRuntimeProfile,
        mut handler: impl FnMut(&Request, &str) -> Reply + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (launch, manifest) = binding(profile, port);
        let alias = format!("lumina-{}", launch.digest().unwrap().as_str());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let worker = thread::spawn(move || loop {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("accept failed: {error}"),
            };
            stream.set_nonblocking(false).unwrap();
            let Some(request) = read_request(&mut stream) else {
                continue;
            };
            captured.lock().unwrap().push(request.clone());
            let reply = handler(&request, &alias);
            thread::sleep(reply.delay);
            let length_header = if reply.chunked {
                "Transfer-Encoding: chunked".into()
            } else {
                format!("Content-Length: {}", reply.body.len())
            };
            let header = format!(
                "HTTP/1.1 {} Unit\r\nContent-Type: {}\r\n{}\r\nConnection: close\r\n{}\r\n",
                reply.status, reply.content_type, length_header, reply.extra_headers
            );
            let _ = stream.write_all(header.as_bytes());
            if reply.chunked {
                for chunk in reply.body.as_bytes().chunks(8192) {
                    if write!(stream, "{:X}\r\n", chunk.len()).is_err()
                        || stream.write_all(chunk).is_err()
                        || stream.write_all(b"\r\n").is_err()
                    {
                        break;
                    }
                }
                let _ = stream.write_all(b"0\r\n\r\n");
            } else {
                let _ = stream.write_all(reply.body.as_bytes());
            }
        });
        Self {
            launch,
            manifest,
            endpoint: format!("http://127.0.0.1:{port}/"),
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn embedding(&self) -> NativeEmbeddingAdapter<'static> {
        NativeEmbeddingAdapter::diagnostic(
            &self.launch,
            &self.manifest,
            &self.endpoint,
            TOKEN,
            Duration::from_secs(2),
        )
        .unwrap()
    }

    fn reranker(&self) -> NativeRerankAdapter<'static> {
        NativeRerankAdapter::diagnostic(
            &self.launch,
            &self.manifest,
            &self.endpoint,
            TOKEN,
            Duration::from_secs(2),
        )
        .unwrap()
    }
}

impl Drop for MockHelper {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let joined = self.worker.take().unwrap().join();
        if !thread::panicking() {
            joined.unwrap();
        }
    }
}

fn vector(axis: usize) -> Vec<f64> {
    let mut values = vec![0.0; 1024];
    values[axis] = 1.0;
    values
}

fn embedding_response(alias: &str, count: usize) -> Value {
    json!({"model":alias,"object":"list","usage":{"prompt_tokens":count,"total_tokens":count},
        "data": (0..count).rev().map(|index| json!({"index":index,"object":"embedding","embedding":vector(index)})).collect::<Vec<_>>()})
}

fn rerank_response(alias: &str, count: usize) -> Value {
    json!({"model":alias,"object":"list","usage":{"prompt_tokens":count*5,"total_tokens":count*5},
        "results":(0..count).rev().map(|index| json!({"index":index,"relevance_score":index as f64-10.0})).collect::<Vec<_>>()})
}

#[test]
fn constructor_binds_manifest_runtime_capability_and_exact_specialized_parameters() {
    let profile = VectorRuntimeProfile::Qwen3Embedding06bQ8;
    let (launch, manifest) = binding(profile, 19238);
    let create = |launch: &LaunchConfig, manifest: &ModelManifest| {
        NativeEmbeddingAdapter::diagnostic(
            launch,
            manifest,
            "http://127.0.0.1:19238/",
            TOKEN,
            Duration::from_secs(1),
        )
    };
    assert!(create(&launch, &manifest).is_ok());
    let mutations: &[fn(&mut LaunchConfig)] = &[
        |launch| launch.version_id = "other".into(),
        |launch| launch.manifest_digest = Sha256Digest::of_bytes(b"other"),
        |launch| launch.runtime.binary_sha256 = Sha256Digest::of_bytes(b"other"),
        |launch| {
            launch.capabilities.insert(ModelCapability::Text);
        },
        |launch| launch.mode = RuntimeExecutionMode::InProcess,
        |launch| launch.mode = RuntimeExecutionMode::Helper { port: 0 },
        |launch| launch.component_files.clear(),
        |launch| {
            if let RuntimeParameters::Specialized { settings, .. } = &mut launch.parameters {
                settings.insert("pooling".into(), "mean".into());
            }
        },
        |launch| {
            if let RuntimeParameters::Specialized { settings, .. } = &mut launch.parameters {
                settings.insert("context_length".into(), "1024".into());
            }
        },
    ];
    for mutate in mutations {
        let mut changed = launch.clone();
        mutate(&mut changed);
        assert!(create(&changed, &manifest).is_err());
    }
    let (rerank_launch, rerank_manifest) =
        binding(VectorRuntimeProfile::BgeRerankerV2M3Q4Km, 19238);
    assert!(create(&rerank_launch, &rerank_manifest).is_err());
    let mut changed_manifest = manifest.clone();
    let wrong = Sha256Digest::of_bytes(b"unmeasured model");
    changed_manifest
        .installed_files
        .get_mut("main")
        .unwrap()
        .computed_sha256 = wrong.clone();
    changed_manifest
        .plan
        .artifacts
        .get_mut("main")
        .unwrap()
        .source_sha256 = Some(wrong);
    let mut changed_launch = launch;
    changed_launch.manifest_digest = changed_manifest.digest().unwrap();
    assert!(create(&changed_launch, &changed_manifest).is_err());
    let (mut changed_launch, mut changed_manifest) = binding(profile, 19238);
    changed_manifest.plan.runtime.binary_sha256 = Sha256Digest::of_bytes(b"unmeasured helper");
    changed_launch.runtime = changed_manifest.plan.runtime.clone();
    changed_launch.manifest_digest = changed_manifest.digest().unwrap();
    assert!(create(&changed_launch, &changed_manifest).is_err());
}

#[test]
fn endpoint_requires_literal_loopback_matching_port_and_no_hidden_target() {
    let (launch, manifest) = binding(VectorRuntimeProfile::Qwen3Embedding06bQ8, 19238);
    for endpoint in [
        "http://192.0.2.1:19238/",
        "http://localhost:19238/",
        "https://127.0.0.1:19238/",
        "http://127.0.0.1:19239/",
        "http://user@127.0.0.1:19238/",
        "http://127.0.0.1:19238/api",
        "http://127.0.0.1:19238/?target=remote",
        "http://127.0.0.1:19238/#fragment",
    ] {
        assert!(
            NativeEmbeddingAdapter::diagnostic(
                &launch,
                &manifest,
                endpoint,
                TOKEN,
                Duration::from_secs(1)
            )
            .is_err(),
            "{endpoint}"
        );
    }
    for (token, timeout) in [
        ("", Duration::from_secs(1)),
        ("bad\nheader", Duration::from_secs(1)),
        (TOKEN, Duration::ZERO),
        (TOKEN, Duration::from_secs(181)),
    ] {
        assert!(NativeEmbeddingAdapter::diagnostic(
            &launch,
            &manifest,
            "http://127.0.0.1:19238/",
            token,
            timeout
        )
        .is_err());
    }
}

#[test]
fn measured_resource_flags_are_frozen_in_the_binding_and_helper_arguments() {
    let frozen = [
        ("batch_cpu_threads", "4"),
        ("flash_attention", "on"),
        ("cache_type_k", "f16"),
        ("cache_type_v", "f16"),
        ("prompt_cache_mib", "0"),
        ("context_checkpoints", "0"),
        ("cache_prompt", "false"),
        ("cache_idle_slots", "false"),
        ("cache_reuse", "0"),
        ("auto_fit", "false"),
        ("speculative_type", "none"),
        ("warmup", "false"),
    ];
    for profile in [
        VectorRuntimeProfile::Qwen3Embedding06bQ8,
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
    ] {
        let (launch, manifest) = binding(profile, 19238);
        let create = |launch: &LaunchConfig| {
            if profile == VectorRuntimeProfile::Qwen3Embedding06bQ8 {
                NativeEmbeddingAdapter::diagnostic(
                    launch,
                    &manifest,
                    "http://127.0.0.1:19238/",
                    TOKEN,
                    Duration::from_secs(1),
                )
                .map(|adapter| adapter.helper_arguments())
            } else {
                NativeRerankAdapter::diagnostic(
                    launch,
                    &manifest,
                    "http://127.0.0.1:19238/",
                    TOKEN,
                    Duration::from_secs(1),
                )
                .map(|adapter| adapter.helper_arguments())
            }
        };
        let args = create(&launch).unwrap();
        for (key, expected) in frozen {
            let RuntimeParameters::Specialized { settings, .. } = &launch.parameters else {
                unreachable!()
            };
            assert_eq!(settings.get(key).map(String::as_str), Some(expected));
            for replacement in [None, Some("unmeasured")] {
                let mut changed = launch.clone();
                let RuntimeParameters::Specialized { settings, .. } = &mut changed.parameters
                else {
                    unreachable!()
                };
                if let Some(value) = replacement {
                    settings.insert(key.into(), value.into());
                } else {
                    settings.remove(key);
                }
                assert!(create(&changed).is_err(), "unbound resource flag: {key}");
            }
        }
        for pair in [
            ["--threads-batch", "4"],
            ["--cache-ram", "0"],
            ["--ctx-checkpoints", "0"],
            ["--cache-reuse", "0"],
            ["--fit", "off"],
            ["--spec-type", "none"],
            ["--cache-type-k", "f16"],
            ["--cache-type-v", "f16"],
            ["--flash-attn", "on"],
        ] {
            assert_eq!(
                args.windows(2).filter(|actual| *actual == pair).count(),
                1,
                "{pair:?}"
            );
        }
        for flag in ["--no-cache-prompt", "--no-cache-idle-slots", "--no-warmup"] {
            assert_eq!(
                args.iter().filter(|actual| actual.as_str() == flag).count(),
                1
            );
        }
        let alias = format!("lumina-{}", launch.digest().unwrap().as_str());
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--alias", alias.as_str()]));
    }
}

#[tokio::test]
async fn embedding_uses_real_batch_endpoint_and_restores_input_index_order() {
    let helper = MockHelper::new(
        VectorRuntimeProfile::Qwen3Embedding06bQ8,
        |request, alias| {
            Reply::json(if request.path == "/tokenize" {
                json!({"tokens":[10]})
            } else {
                embedding_response(alias, 2)
            })
        },
    );
    let adapter = helper.embedding();
    let batch = adapter
        .embed(&["Paris".into(), "北京".into()])
        .await
        .unwrap();
    assert_eq!(batch.vectors, vec![vector(0), vector(1)]);
    assert_eq!(batch.launch_digest, helper.launch.digest().unwrap());
    let requests = helper.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request
        .headers
        .to_lowercase()
        .contains(&format!("authorization: bearer {TOKEN}"))));
    assert_eq!(
        requests[0].body,
        json!({"content":"Paris","add_special":true,"parse_special":true,"with_pieces":false})
    );
    assert_eq!(requests[2].path, "/v1/embeddings");
    assert_eq!(
        requests[2].body,
        json!({"input":["Paris","北京"],"encoding_format":"float","embd_normalize":2})
    );
    let args = adapter.helper_arguments();
    assert!(args.windows(2).any(|pair| pair == ["--pooling", "last"]));
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--alias", adapter.model_alias()]));
    assert!(!args.iter().any(|value| value == TOKEN || value == "-m"));
}

#[tokio::test]
async fn malformed_embedding_protocol_indices_dimensions_and_values_are_rejected() {
    for case in 0..10 {
        let helper = MockHelper::new(
            VectorRuntimeProfile::Qwen3Embedding06bQ8,
            move |request, alias| {
                if request.path == "/tokenize" {
                    return Reply::json(json!({"tokens":[10]}));
                }
                let mut body = embedding_response(alias, 2);
                match case {
                    0 => body["data"][1]["index"] = json!(1),
                    1 => body["data"][1]["index"] = json!(2),
                    2 => {
                        body["data"].as_array_mut().unwrap().pop();
                    }
                    3 => body["data"][0]["embedding"] = json!([1.0, 0.0]),
                    4 => body["data"][0]["embedding"] = json!(vec![0.0; 1024]),
                    5 => body["object"] = json!("chat.completion"),
                    6 => body["model"] = json!("different-helper"),
                    7 => body["usage"]["total_tokens"] = json!(1),
                    8 => body["data"][0]["embedding"][0] = Value::Null,
                    9 => body["data"][0]["object"] = json!("chat.message"),
                    _ => unreachable!(),
                }
                Reply::json(body)
            },
        );
        assert!(
            helper
                .embedding()
                .embed(&["one".into(), "two".into()])
                .await
                .is_err(),
            "case {case}"
        );
    }
    for value in ["NaN", "Infinity", "1e400"] {
        let helper = MockHelper::new(
            VectorRuntimeProfile::Qwen3Embedding06bQ8,
            move |request, alias| {
                if request.path == "/tokenize" {
                    return Reply::json(json!({"tokens":[10]}));
                }
                let mut reply = Reply::json(embedding_response(alias, 1));
                reply.body = reply.body.replacen("1.0", value, 1);
                reply
            },
        );
        assert!(helper.embedding().embed(&["one".into()]).await.is_err());
    }
}

#[tokio::test]
async fn invalid_inputs_are_rejected_before_any_http_request() {
    let helper = MockHelper::new(VectorRuntimeProfile::Qwen3Embedding06bQ8, |_, _| {
        panic!("unexpected HTTP request")
    });
    for inputs in [
        vec![],
        vec!["  ".into()],
        vec!["界".repeat(2731)],
        vec!["x".into(); 17],
        vec!["x".repeat(8192); 9],
    ] {
        assert!(helper.embedding().embed(&inputs).await.is_err());
    }
    assert!(helper.requests.lock().unwrap().is_empty());
    let helper = MockHelper::new(VectorRuntimeProfile::BgeRerankerV2M3Q4Km, |_, _| {
        panic!("unexpected HTTP request")
    });
    assert!(helper
        .reranker()
        .rerank(" ", &["doc".into()])
        .await
        .is_err());
    assert!(helper.reranker().rerank("query", &[]).await.is_err());
    assert!(helper.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn maximum_item_count_and_exact_utf8_byte_budget_are_accepted() {
    let helper = MockHelper::new(
        VectorRuntimeProfile::Qwen3Embedding06bQ8,
        |request, alias| {
            Reply::json(if request.path == "/tokenize" {
                json!({"tokens":[10]})
            } else {
                embedding_response(alias, request.body["input"].as_array().unwrap().len())
            })
        },
    );
    let adapter = helper.embedding();
    for inputs in [vec!["item".into(); 16], vec!["x".repeat(8192); 8]] {
        assert_eq!(
            adapter.embed(&inputs).await.unwrap().vectors.len(),
            inputs.len()
        );
    }
    let helper = MockHelper::new(
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
        |request, alias| {
            Reply::json(if request.path == "/tokenize" {
                json!({"tokens":[10]})
            } else {
                rerank_response(alias, request.body["documents"].as_array().unwrap().len())
            })
        },
    );
    let adapter = helper.reranker();
    let mut byte_boundary = vec!["x".repeat(8192); 8];
    byte_boundary[0].pop();
    for documents in [vec!["item".into(); 16], byte_boundary.clone()] {
        assert_eq!(
            adapter.rerank("q", &documents).await.unwrap().scores.len(),
            documents.len()
        );
    }
    let sent = helper.requests.lock().unwrap().len();
    assert!(adapter.rerank("qq", &byte_boundary).await.is_err());
    assert_eq!(helper.requests.lock().unwrap().len(), sent);
}

#[tokio::test]
async fn invalid_or_excessive_token_counts_never_reach_inference() {
    for tokens in [
        json!([]),
        json!([-1]),
        json!([null]),
        json!(["token"]),
        json!(vec![10; 512]),
    ] {
        let helper = MockHelper::new(
            VectorRuntimeProfile::Qwen3Embedding06bQ8,
            move |request, _| {
                assert_eq!(request.path, "/tokenize");
                Reply::json(json!({"tokens":tokens}))
            },
        );
        assert!(helper.embedding().embed(&["one".into()]).await.is_err());
        assert_eq!(helper.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn causal_last_pooling_accepts_511_tokens_below_the_native_context_boundary() {
    let helper = MockHelper::new(
        VectorRuntimeProfile::Qwen3Embedding06bQ8,
        |request, alias| {
            if request.path == "/tokenize" {
                return Reply::json(json!({"tokens":vec![10; 511]}));
            }
            let mut body = embedding_response(alias, 1);
            body["usage"] = json!({"prompt_tokens":511,"total_tokens":511});
            Reply::json(body)
        },
    );
    assert!(helper.embedding().embed(&["one".into()]).await.is_ok());
}

#[tokio::test]
async fn reranker_preserves_raw_negative_logits_and_complete_indices() {
    let helper = MockHelper::new(
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
        |request, alias| {
            Reply::json(if request.path == "/tokenize" {
                json!({"tokens":[10]})
            } else {
                rerank_response(alias, 3)
            })
        },
    );
    let adapter = helper.reranker();
    let batch = adapter
        .rerank("query", &["first".into(), "second".into(), "third".into()])
        .await
        .unwrap();
    assert_eq!(batch.semantics, RerankScoreSemantics::RawClassifierLogit);
    assert_eq!(
        batch
            .scores
            .iter()
            .map(|item| item.index)
            .collect::<Vec<_>>(),
        vec![2, 1, 0]
    );
    assert_eq!(batch.scores[0].relevance_score, -8.0);
    let requests = helper.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests[0].body,
        json!({"content":"query","add_special":false,"parse_special":false,"with_pieces":false})
    );
    assert_eq!(requests[4].path, "/v1/rerank");
    assert_eq!(requests[4].body["top_n"], 3);
    assert!(adapter.helper_arguments().contains(&"--reranking".into()));
}

#[tokio::test]
async fn rerank_pair_budget_includes_all_three_native_special_tokens() {
    for document_tokens in [254, 255] {
        let helper = MockHelper::new(
            VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
            move |request, alias| {
                if request.path == "/tokenize" {
                    let count = if request.body["content"] == "query" {
                        255
                    } else {
                        document_tokens
                    };
                    return Reply::json(json!({"tokens":vec![10; count]}));
                }
                let mut body = rerank_response(alias, 1);
                body["usage"] = json!({"prompt_tokens":512,"total_tokens":512});
                Reply::json(body)
            },
        );
        let result = helper
            .reranker()
            .rerank("query", &["document".into()])
            .await;
        assert_eq!(result.is_ok(), document_tokens == 254);
        assert_eq!(
            helper.requests.lock().unwrap().len(),
            if document_tokens == 254 { 3 } else { 2 }
        );
    }
}

#[tokio::test]
async fn rerank_rejects_missing_duplicate_out_of_range_unsorted_and_nonfinite_scores() {
    for case in 0..7 {
        let helper = MockHelper::new(
            VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
            move |request, alias| {
                if request.path == "/tokenize" {
                    return Reply::json(json!({"tokens":[10]}));
                }
                let mut body = rerank_response(alias, 2);
                match case {
                    0 => body["results"].as_array_mut().unwrap().clear(),
                    1 => body["results"][1]["index"] = json!(1),
                    2 => body["results"][1]["index"] = json!(2),
                    3 => body["results"][1]["relevance_score"] = json!(100.0),
                    4 => body["results"][1]["relevance_score"] = Value::Null,
                    5 => body["usage"]["prompt_tokens"] = json!(9),
                    6 => body["object"] = json!("chat.completion"),
                    _ => unreachable!(),
                }
                Reply::json(body)
            },
        );
        assert!(
            helper
                .reranker()
                .rerank("query", &["one".into(), "two".into()])
                .await
                .is_err(),
            "case {case}"
        );
    }
    for value in ["NaN", "Infinity", "1e400"] {
        let helper = MockHelper::new(
            VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
            move |request, alias| {
                if request.path == "/tokenize" {
                    return Reply::json(json!({"tokens":[10]}));
                }
                let mut reply = Reply::json(rerank_response(alias, 1));
                reply.body = reply.body.replace("-10.0", value);
                reply
            },
        );
        assert!(helper
            .reranker()
            .rerank("query", &["one".into()])
            .await
            .is_err());
    }
}

#[tokio::test]
async fn rerank_rejects_the_native_missing_embedding_sentinel() {
    let helper = MockHelper::new(
        VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
        |request, alias| {
            if request.path == "/tokenize" {
                return Reply::json(json!({"tokens":[10]}));
            }
            let mut body = rerank_response(alias, 2);
            body["results"][1]["relevance_score"] = json!(-1_000_000.0);
            Reply::json(body)
        },
    );
    let error = helper
        .reranker()
        .rerank("query", &["one".into(), "two".into()])
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("failed to produce a rank embedding"));
}

#[tokio::test]
async fn transport_rejects_redirect_http_failure_non_json_and_oversized_bodies() {
    for case in 0..5 {
        let helper = MockHelper::new(VectorRuntimeProfile::Qwen3Embedding06bQ8, move |_, _| {
            let mut reply = Reply::json(json!({"tokens":[10]}));
            match case {
                0 => {
                    reply.status = 302;
                    reply.extra_headers = "Location: http://192.0.2.1/forbidden\r\n".into();
                }
                1 => reply.status = 503,
                2 => reply.content_type = "text/html",
                3 => reply.body = " ".repeat(1024 * 1024 + 1),
                4 => {
                    reply.body = " ".repeat(1024 * 1024 + 1);
                    reply.chunked = true;
                }
                _ => unreachable!(),
            }
            reply
        });
        assert!(helper.embedding().embed(&["one".into()]).await.is_err());
        assert_eq!(helper.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn request_deadline_and_dropped_future_stop_further_batch_requests() {
    for external_cancel in [false, true] {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut entered = Some(entered_tx);
        let helper = MockHelper::new(VectorRuntimeProfile::Qwen3Embedding06bQ8, move |_, _| {
            if let Some(entered) = entered.take() {
                let _ = entered.send(());
                release_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            }
            Reply::json(json!({"tokens":[10]}))
        });
        let adapter = NativeEmbeddingAdapter::diagnostic(
            &helper.launch,
            &helper.manifest,
            &helper.endpoint,
            TOKEN,
            if external_cancel {
                Duration::from_secs(30)
            } else {
                Duration::from_millis(150)
            },
        )
        .unwrap();
        let inputs = vec!["one".into(), "two".into()];
        if external_cancel {
            let mut operation = Box::pin(adapter.embed(&inputs));
            tokio::select! {
                result = &mut operation => panic!("operation finished before cancellation: {result:?}"),
                entered = entered_rx => entered.unwrap(),
            }
            let requests = helper.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].path, "/tokenize");
            assert_eq!(requests[0].body["content"], "one");
            drop(requests);
            drop(operation);
        } else {
            let error = adapter.embed(&inputs).await.unwrap_err();
            assert!(
                error
                    .downcast_ref::<tokio::time::error::Elapsed>()
                    .is_some()
                    || error
                        .downcast_ref::<reqwest::Error>()
                        .is_some_and(|error| error.is_timeout()),
                "expected a request deadline error, got {error:#}"
            );
        }
        let error = adapter.embed(&inputs).await.unwrap_err();
        assert!(error.to_string().contains("stop and restart this helper"));
        release_tx.send(()).unwrap();
        let requests = Arc::clone(&helper.requests);
        drop(adapter);
        drop(helper);
        let requests = requests.lock().unwrap();
        assert!(requests.len() <= 1);
        if external_cancel {
            assert_eq!(requests.len(), 1);
        }
        assert!(requests
            .iter()
            .all(|request| request.path == "/tokenize" && request.body["content"] == "one"));
    }
}
