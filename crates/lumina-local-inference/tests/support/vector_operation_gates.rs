use super::*;
use crate::lifecycle::vector_runtime::{NativeEmbeddingAdapter, NativeRerankAdapter};
use futures::{future::LocalBoxFuture, FutureExt};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread;
use tokio::sync::oneshot;

#[derive(Clone, Debug)]
struct Request {
    path: String,
    body: Value,
}

fn read_request(stream: &mut TcpStream) -> Request {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while bytes.len() < header_end + content_length {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    Request {
        path: headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .into(),
        body: serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap(),
    }
}

struct Helper {
    endpoint: Url,
    alias: String,
    digest: Sha256Digest,
    requests: Arc<Mutex<Vec<Request>>>,
    stopping: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Helper {
    fn new(mut respond: impl FnMut(&Request, &str) -> String + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let digest = Sha256Digest::of_bytes(b"unit transport fixture, not a native owner");
        let alias = format!("lumina-{}", digest.as_str());
        let server_alias = alias.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let request = read_request(&mut stream);
                captured.lock().unwrap().push(request.clone());
                let body = respond(&request, &server_alias);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                );
                if stream.write_all(header.as_bytes()).is_ok() {
                    let _ = stream.write_all(body.as_bytes());
                }
            }
        });
        Self {
            endpoint,
            alias,
            digest,
            requests,
            stopping,
            worker: Some(worker),
        }
    }

    fn adapter(&self, profile: VectorRuntimeProfile, gate: &Arc<NativeRequestGate>) -> Adapter {
        // Direct transport fixtures exercise the shared gate without inventing
        // a live NativeHelperRuntime or claiming resource ownership.
        let connection = VectorConnection {
            client: Client::builder().no_proxy().build().unwrap(),
            endpoint: self.endpoint.clone(),
            profile,
            timeout: Duration::from_secs(5),
            model_alias: self.alias.clone(),
            launch_digest: self.digest.clone(),
            request_gate: Arc::clone(gate),
        };
        match profile {
            VectorRuntimeProfile::Qwen3Embedding06bQ8 => {
                Adapter::Embedding(NativeEmbeddingAdapter {
                    _owner: None,
                    connection,
                })
            }
            VectorRuntimeProfile::BgeRerankerV2M3Q4Km => Adapter::Rerank(NativeRerankAdapter {
                _owner: None,
                connection,
            }),
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let joined = self.worker.take().unwrap().join();
        if !thread::panicking() {
            joined.unwrap();
        }
    }
}

enum Adapter {
    Embedding(NativeEmbeddingAdapter<'static>),
    Rerank(NativeRerankAdapter<'static>),
}

impl Adapter {
    fn invoke<'a>(&'a self, inputs: &'a [String]) -> LocalBoxFuture<'a, Result<()>> {
        async move {
            match self {
                Self::Embedding(adapter) => adapter.embed(inputs).await.map(|_| ()),
                Self::Rerank(adapter) => adapter.rerank("query", inputs).await.map(|_| ()),
            }
        }
        .boxed_local()
    }
}

fn successful_response(request: &Request, alias: &str) -> Value {
    match request.path.as_str() {
        "/tokenize" => json!({"tokens":[10]}),
        "/v1/embeddings" => {
            let count = request.body["input"].as_array().unwrap().len();
            let mut vector = vec![0.0; 1024];
            vector[0] = 1.0;
            json!({"model":alias,"object":"list",
                "usage":{"prompt_tokens":count,"total_tokens":count},
                "data":(0..count).map(|index| json!({"index":index,"object":"embedding","embedding":vector})).collect::<Vec<_>>()})
        }
        "/v1/rerank" => {
            let count = request.body["documents"].as_array().unwrap().len();
            json!({"model":alias,"object":"list",
                "usage":{"prompt_tokens":count*5,"total_tokens":count*5},
                "results":(0..count).map(|index| json!({"index":index,"relevance_score":-5.0-index as f64})).collect::<Vec<_>>()})
        }
        path => panic!("unexpected request {path}"),
    }
}

const PROFILES: [VectorRuntimeProfile; 2] = [
    VectorRuntimeProfile::Qwen3Embedding06bQ8,
    VectorRuntimeProfile::BgeRerankerV2M3Q4Km,
];

#[tokio::test]
async fn shared_gate_serializes_every_preflight_and_inference_in_a_complete_batch() {
    for profile in PROFILES {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut entered = Some(entered_tx);
        let helper = Helper::new(move |request, alias| {
            if let Some(entered) = entered.take() {
                entered.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            successful_response(request, alias).to_string()
        });
        let gate = Arc::new(NativeRequestGate::default());
        let first = helper.adapter(profile, &gate);
        let second = helper.adapter(profile, &gate);
        let first_inputs = ["first-a".into(), "first-b".into()];
        let second_inputs = ["second-a".into(), "second-b".into()];
        let mut first_operation = first.invoke(&first_inputs);
        tokio::select! {
            result = &mut first_operation => panic!("first batch finished before release: {result:?}"),
            entered = entered_rx => entered.unwrap(),
        }
        let mut second_operation = second.invoke(&second_inputs);
        assert!(futures::poll!(&mut second_operation).is_pending());
        assert_eq!(helper.requests.lock().unwrap().len(), 1);
        release_tx.send(()).unwrap();
        let (first_result, second_result) = tokio::join!(first_operation, second_operation);
        first_result.unwrap();
        second_result.unwrap();
        {
            let requests = helper.requests.lock().unwrap();
            let batch_width = if profile == VectorRuntimeProfile::Qwen3Embedding06bQ8 {
                3
            } else {
                4
            };
            assert_eq!(requests.len(), batch_width * 2);
            for (batch, inputs) in requests
                .chunks_exact(batch_width)
                .zip([first_inputs, second_inputs])
            {
                let expected: Vec<&str> = if profile == VectorRuntimeProfile::Qwen3Embedding06bQ8 {
                    inputs.iter().map(String::as_str).collect()
                } else {
                    std::iter::once("query")
                        .chain(inputs.iter().map(String::as_str))
                        .collect()
                };
                assert_eq!(
                    batch[..batch.len() - 1]
                        .iter()
                        .map(|request| request.body["content"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    expected,
                );
                assert!(batch.last().unwrap().path.starts_with("/v1/"));
            }
        }
        assert!(gate.acquire().await.is_ok());
    }
}

#[tokio::test]
async fn cancelled_preflight_or_inference_rejects_reuse_by_another_adapter() {
    for profile in PROFILES {
        for cancel_inference in [false, true] {
            let (entered_tx, entered_rx) = oneshot::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let mut entered = Some(entered_tx);
            let helper = Helper::new(move |request, alias| {
                let selected = if cancel_inference {
                    request.path != "/tokenize"
                } else {
                    true
                };
                if let Some(entered) = entered.take_if(|_| selected) {
                    entered.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                successful_response(request, alias).to_string()
            });
            let gate = Arc::new(NativeRequestGate::default());
            let first = helper.adapter(profile, &gate);
            let second = helper.adapter(profile, &gate);
            let inputs = ["one".into(), "two".into()];
            let mut operation = first.invoke(&inputs);
            tokio::select! {
                result = &mut operation => panic!("operation finished before cancellation: {result:?}"),
                entered = entered_rx => entered.unwrap(),
            }
            let sent = helper.requests.lock().unwrap().len();
            drop(operation);
            let error = second.invoke(&inputs).await.unwrap_err();
            assert!(error.to_string().contains("stop and restart this helper"));
            assert_eq!(helper.requests.lock().unwrap().len(), sent);
            release_tx.send(()).unwrap();
        }
    }
}

#[tokio::test]
async fn cancelling_or_timing_out_a_queued_operation_preserves_the_active_helper() {
    for profile in PROFILES {
        let helper = Helper::new(|request, alias| successful_response(request, alias).to_string());
        let gate = Arc::new(NativeRequestGate::default());
        let active = gate.acquire().await.unwrap();
        let adapter = helper.adapter(profile, &gate);
        let inputs = ["one".into()];
        let mut waiting = adapter.invoke(&inputs);
        assert!(futures::poll!(&mut waiting).is_pending());
        drop(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(5), adapter.invoke(&inputs))
                .await
                .is_err()
        );
        assert!(helper.requests.lock().unwrap().is_empty());
        drop(active);
        adapter.invoke(&inputs).await.unwrap();
        assert!(gate.acquire().await.is_ok());
    }
}

#[tokio::test]
async fn malformed_http_completion_invalidates_the_shared_helper_gate() {
    for profile in PROFILES {
        let helper = Helper::new(|_, _| "{\"tokens\":[10]".into());
        let gate = Arc::new(NativeRequestGate::default());
        let first = helper.adapter(profile, &gate);
        let second = helper.adapter(profile, &gate);
        let inputs = ["one".into()];
        assert!(first.invoke(&inputs).await.is_err());
        let error = second.invoke(&inputs).await.unwrap_err();
        assert!(error.to_string().contains("stop and restart this helper"));
        assert_eq!(helper.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn completed_token_budget_rejection_releases_the_gate_without_poisoning_it() {
    for profile in PROFILES {
        let mut first_request = true;
        let helper = Helper::new(move |request, alias| {
            if std::mem::take(&mut first_request) {
                return json!({"tokens":vec![10; 513]}).to_string();
            }
            successful_response(request, alias).to_string()
        });
        let gate = Arc::new(NativeRequestGate::default());
        let first = helper.adapter(profile, &gate);
        let second = helper.adapter(profile, &gate);
        let inputs = ["one".into()];
        let error = first.invoke(&inputs).await.unwrap_err();
        assert!(error.to_string().contains("exceeds capacity"));
        second.invoke(&inputs).await.unwrap();
        assert!(gate.acquire().await.is_ok());
    }
}

#[tokio::test]
async fn completed_rerank_sentinel_rejection_allows_a_later_valid_negative_logit() {
    let mut first_inference = true;
    let helper = Helper::new(move |request, alias| {
        let mut response = successful_response(request, alias);
        if request.path == "/v1/rerank" && std::mem::take(&mut first_inference) {
            response["results"][0]["relevance_score"] = json!(-1_000_000.0);
        }
        response.to_string()
    });
    let gate = Arc::new(NativeRequestGate::default());
    let adapter = helper.adapter(VectorRuntimeProfile::BgeRerankerV2M3Q4Km, &gate);
    let inputs = ["one".into()];
    let error = adapter.invoke(&inputs).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("failed to produce a rank embedding"));
    adapter.invoke(&inputs).await.unwrap();
    assert_eq!(helper.requests.lock().unwrap().len(), 6);
    assert!(gate.acquire().await.is_ok());
}
