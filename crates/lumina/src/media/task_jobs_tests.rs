use super::*;
use crate::media::{
    BeginMediaPublish, MediaRuntimeBuild, MediaSource, NativeTaskTranscript, PublishMediaRequest,
};
use lumina_providers::local_inference::lifecycle::*;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::io::{Cursor, Write};
use std::path::PathBuf;

const SESSION: &str = "test-session";

fn temp() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lifecycle-validation/native-task-jobs-tests");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base.canonicalize().unwrap()).unwrap()
}

async fn open(path: &Path) -> NativeTaskJobs {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path.join("sessions.db"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT OR IGNORE INTO sessions VALUES (?)")
        .bind(SESSION)
        .execute(&pool)
        .await
        .unwrap();
    MediaResultStore::new(pool.clone())
        .initialize()
        .await
        .unwrap();
    TaskConfigurationStore::new(pool.clone())
        .initialize()
        .await
        .unwrap();
    let jobs = NativeTaskJobs::new(pool);
    jobs.initialize().await.unwrap();
    jobs
}

fn image_adapter() -> GenerationParameterService {
    let profile = GenerationProfile::Sd15CpuTextToImage;
    GenerationParameterService::new(
        profile,
        GenerationModelBinding {
            installation_id: "synthetic-installation".into(),
            version_id: "v1".into(),
            manifest_digest: Sha256Digest::of_bytes(b"synthetic generation manifest"),
            runtime: profile.runtime_build(),
            target_capabilities: profile.required_capabilities(),
            assets: profile
                .recorded_assets()
                .into_iter()
                .map(|(role, (sha256, size_bytes))| {
                    (
                        role,
                        GenerationAssetBinding {
                            artifact_id: format!("{role:?}"),
                            sha256,
                            size_bytes,
                        },
                    )
                })
                .collect(),
        },
    )
    .unwrap()
}

async fn take(
    jobs: &NativeTaskJobs,
    root: &Path,
    call: &str,
    args: &Value,
    identity: &TaskParameterIdentity,
) -> NativeTaskAttempt {
    match jobs
        .claim(root, SESSION, call, "run", args, identity)
        .await
        .unwrap()
    {
        NativeTaskClaim::Execute(owner) => owner,
        _ => panic!("expected initial execution claim"),
    }
}

#[tokio::test]
async fn busy_failed_cancelled_interrupted_and_deferred_remain_distinct_on_reopen() {
    let tmp = temp();
    let root = tmp.path().join("outputs");
    let a = open(tmp.path()).await;
    let b = open(tmp.path()).await;
    let identity = image_adapter().identity().unwrap();
    let args = json!({"identity":identity,"parameters":{"prompt":"apple"}});
    let same_args = json!({"parameters":{"prompt":"apple"},"identity":identity});
    let owner = take(&a, &root, "failure", &args, &identity).await;
    assert!(matches!(
        b.claim(&root, SESSION, "failure", "run", &same_args, &identity)
            .await
            .unwrap(),
        NativeTaskClaim::Busy
    ));
    owner.finish(NativeTaskTerminal::Failed).await.unwrap();
    assert!(matches!(
        b.claim(&root, SESSION, "failure", "run", &same_args, &identity)
            .await
            .unwrap(),
        NativeTaskClaim::Stopped(NativeTaskTerminal::Failed)
    ));
    assert!(b
        .claim(
            &root,
            SESSION,
            "failure",
            "run",
            &json!({"parameters":{"prompt":"other"}}),
            &identity
        )
        .await
        .is_err());
    assert!(b
        .claim(&root, SESSION, "failure", "reuse", &args, &identity)
        .await
        .is_err());

    let owner = take(&a, &root, "cancelled", &args, &identity).await;
    owner.finish(NativeTaskTerminal::Cancelled).await.unwrap();
    assert!(matches!(
        b.claim(&root, SESSION, "cancelled", "run", &args, &identity)
            .await
            .unwrap(),
        NativeTaskClaim::Stopped(NativeTaskTerminal::Cancelled)
    ));
    drop(take(&a, &root, "abandoned", &args, &identity).await);
    assert!(matches!(
        b.claim(&root, SESSION, "abandoned", "run", &args, &identity)
            .await
            .unwrap(),
        NativeTaskClaim::Stopped(NativeTaskTerminal::Interrupted)
    ));
    take(&a, &root, "profiling", &args, &identity)
        .await
        .finish(NativeTaskTerminal::Deferred)
        .await
        .unwrap();
    take(&b, &root, "profiling", &args, &identity)
        .await
        .finish(NativeTaskTerminal::Deferred)
        .await
        .unwrap();
}

#[tokio::test]
async fn restart_replays_only_a_complete_published_batch_and_preserves_the_generated_seed() {
    let tmp = temp();
    let root = tmp.path().join("outputs");
    let jobs = open(tmp.path()).await;
    let call = "fresh-seed-call";
    let job = NativeTaskJobs::job_id(SESSION, call).unwrap();
    let adapter = image_adapter();
    let identity = adapter.identity().unwrap();
    let args = json!({"parameters":{"prompt":"an apple","batchCount":2,"seed":GenerationSeed::New},"identity":identity});
    let owner = take(&jobs, &root, call, &args, &identity).await;
    let plan = adapter
        .compile(GenerationTaskRequest {
            session_id: SESSION.into(),
            job_id: job.clone(),
            parameters: GenerationParameters::Image(Sd15ImageParameters {
                prompt: Some("an apple".into()),
                batch_count: Some(2),
                seed: Some(GenerationSeed::New),
                ..Default::default()
            }),
        })
        .unwrap();
    let configs = TaskConfigurationStore::new(jobs.pool.clone());
    let saved = configs.save_generation(&plan).await.unwrap();
    let EffectiveGenerationParameters::Image(parameters) = &plan.snapshot().parameters else {
        panic!("expected image")
    };
    let media = MediaResultStore::new(jobs.pool.clone());
    let output = media.register_output_root(&root).await.unwrap();
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::new(parameters.width, parameters.height))
        .write_to(&mut bytes, image::ImageOutputFormat::Png)
        .unwrap();
    let runtime = adapter.descriptor().binding.runtime.clone();
    let hash = Sha256Digest::parse(saved.generation_config_sha256()).unwrap();
    let ids: Vec<_> = (0..2)
        .map(|index| native_job_result_id(SESSION, &job, &hash, index))
        .collect();
    let publish = |id: String| {
        let request = PublishMediaRequest {
            session_id: SESSION.into(),
            job_id: job.clone(),
            result_id: id,
            root_id: output.root_id.clone(),
            kind: MediaKind::Image,
            association: MediaAssociation::ToolCall {
                tool_call_id: call.into(),
            },
            source: MediaSource::LocalModel {
                installation_id: identity.installation_id.clone(),
                version_id: identity.version_id.clone(),
                runtime: MediaRuntimeBuild {
                    adapter_id: runtime.adapter_id.clone(),
                    engine_revision: runtime.engine_revision.clone(),
                    binary_sha256: runtime.binary_sha256.as_str().into(),
                },
                generation_config_sha256: saved.generation_config_sha256().into(),
                input_refs: vec![],
            },
        };
        let media = &media;
        let bytes = bytes.get_ref();
        async move {
            let BeginMediaPublish::Staging(mut staged) =
                media.begin_publish(request).await.unwrap()
            else {
                panic!("new unique result expected")
            };
            staged.write_all(bytes).unwrap();
            media.publish(staged).await.unwrap();
        }
    };
    publish(ids[0].clone()).await;
    drop(owner);
    // A durable snapshot and the first of two results cannot certify a finished batch.
    assert!(matches!(
        jobs.claim(&root, SESSION, call, "run", &args, &identity)
            .await
            .unwrap(),
        NativeTaskClaim::Stopped(NativeTaskTerminal::Interrupted)
    ));
    publish(ids[1].clone()).await;
    jobs.pool.close().await;
    let reopened = open(tmp.path()).await;
    let NativeTaskClaim::Replay(ImportedTaskOutput::Media(results)) = reopened
        .claim(&root, SESSION, call, "run", &args, &identity)
        .await
        .unwrap()
    else {
        panic!("expected complete persisted replay")
    };
    assert_eq!(
        results.into_iter().map(|r| r.result_id).collect::<Vec<_>>(),
        ids
    );
    let frozen = TaskConfigurationStore::new(reopened.pool.clone())
        .read(SESSION, &job)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frozen, saved);
    let mut modified = args.clone();
    modified["parameters"]["seed"] = json!(44);
    assert!(reopened
        .claim(&root, SESSION, call, "run", &modified, &identity)
        .await
        .is_err());
}

#[tokio::test]
async fn transcript_replay_authenticates_original_bytes_and_the_tool_call() {
    let tmp = temp();
    let root = tmp.path().join("outputs");
    let jobs = open(tmp.path()).await;
    let profile = AudioTaskProfile::Qwen3Asr06bCpu;
    let asset = |name: &str, (sha256, size_bytes)| AudioTaskAssetBinding {
        artifact_id: name.into(),
        sha256,
        size_bytes,
    };
    let adapter = Qwen3AsrParameterService::new(AudioTaskModelBinding {
        installation_id: "synthetic-asr".into(),
        version_id: "v1".into(),
        manifest_digest: Sha256Digest::of_bytes(b"fixture"),
        runtime: profile.runtime_build(),
        target_capabilities: [profile.capability()].into(),
        model: asset("main", profile.recorded_model()),
        model_spec: asset("spec", profile.recorded_model_spec()),
    })
    .unwrap();
    let identity = adapter.identity().unwrap();
    let input = uuid::Uuid::new_v4().to_string();
    let args = json!({"identity":identity,"parameters":{"audio":input}});
    let config = TaskConfigurationStore::new(jobs.pool.clone());
    let text = "Exact transcript.\n";
    for (call, source_call) in [("good", "good"), ("substituted", "other")] {
        let job = NativeTaskJobs::job_id(SESSION, call).unwrap();
        let owner = take(&jobs, &root, call, &args, &identity).await;
        let task = adapter
            .compile(Qwen3AsrRequest {
                session_id: SESSION.into(),
                job_id: job.clone(),
                audio: AudioTaskMediaReference {
                    session_id: SESSION.into(),
                    result_id: input.clone(),
                    sha256: Sha256Digest::of_bytes(b"source"),
                    mime_type: "audio/wav".into(),
                    size_bytes: 32_044,
                    sample_rate: 16_000,
                    channels: 1,
                    sample_frames: 16_000,
                },
                context: None,
                language: None,
                max_tokens: None,
                chunking: None,
            })
            .unwrap();
        let saved = config.save_qwen3_asr(&task).await.unwrap();
        let transcript = NativeTaskTranscript {
            session_id: SESSION.into(),
            job_id: job,
            association: MediaAssociation::ToolCall {
                tool_call_id: source_call.into(),
            },
            generation_config_sha256: saved.generation_config_sha256().into(),
            bytes: text.len() as u64,
            sha256: Sha256Digest::of_bytes(text.as_bytes()).as_str().into(),
            text: text.into(),
        };
        config.save_transcript(&transcript).await.unwrap();
        owner.finish(NativeTaskTerminal::Completed).await.unwrap();
        let retry = jobs
            .claim(&root, SESSION, call, "run", &args, &identity)
            .await;
        if call == source_call {
            assert!(
                matches!(retry,Ok(NativeTaskClaim::Replay(ImportedTaskOutput::Transcript(value))) if value==transcript)
            );
            sqlx::query("UPDATE media_task_transcripts_v1 SET text='tampered' WHERE job_id=?")
                .bind(&transcript.job_id)
                .execute(&jobs.pool)
                .await
                .unwrap();
            assert!(jobs
                .claim(&root, SESSION, call, "run", &args, &identity)
                .await
                .is_err());
        } else {
            assert!(retry.is_err());
        }
    }
}

#[test]
fn independent_process_observes_the_same_job_lock() {
    const VARIABLE: &str = "LUMINA_TEST_NATIVE_JOB_LOCK_ROOT";
    if let Some(root) = std::env::var_os(VARIABLE) {
        assert!(lock_job(Path::new(&root), SESSION, "child-process")
            .unwrap()
            .is_none());
        return;
    }
    let tmp = temp();
    let _owner = lock_job(tmp.path(), SESSION, "child-process")
        .unwrap()
        .unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "media::task_jobs::tests::independent_process_observes_the_same_job_lock",
        ])
        .env(VARIABLE, tmp.path())
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success() && log.contains("1 passed"),
        "independent lock check: {log} {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
