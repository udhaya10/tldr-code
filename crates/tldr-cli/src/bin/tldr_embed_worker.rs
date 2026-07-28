//! Disposable bulk embedding worker. The daemon communicates over bounded JSONL stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use tldr_core::semantic::{
    decode_worker_message, encode_worker_message, load_or_build_store_from_artifacts_with_progress,
    GenerationManager, JobRecord, JobState, RedbStore, WorkerBuildRequest, WorkerEvent,
    DEFAULT_REDB_CACHE_BYTES, MAX_WORKER_MESSAGE_BYTES, WORKER_PROTOCOL_VERSION,
};

fn main() -> Result<()> {
    let mut input = Vec::new();
    BufReader::new(std::io::stdin())
        .take(MAX_WORKER_MESSAGE_BYTES as u64 + 1)
        .read_until(b'\n', &mut input)
        .context("read worker request")?;
    let request: WorkerBuildRequest = decode_worker_message(&input)?;
    request.validate()?;
    let recipe = request.compatibility_fingerprint()?;
    let ledger_path = request.store_dir.join("worker-jobs.redb");
    let ledger = RedbStore::open(&ledger_path, DEFAULT_REDB_CACHE_BYTES)?;
    let mut previous = ledger.get_job(&request.job_id)?;
    if let Some(record) = previous.as_ref() {
        if record.protocol_version != WORKER_PROTOCOL_VERSION || record.recipe != recipe {
            let reason = format!(
                "discarded incompatible worker metadata: protocol={} recipe_match={}",
                record.protocol_version,
                record.recipe == recipe
            );
            ledger.remove_job(&request.job_id)?;
            emit(&WorkerEvent::Invalidated {
                reason: reason.clone(),
            })?;
            previous = None;
        }
    }
    if previous
        .as_ref()
        .is_some_and(|record| record.state == JobState::Completed)
        && !request.options.collect_metrics
    {
        let identity =
            tldr_core::semantic::store_search::manifest_id_for(&request.project, &request.options);
        let store = GenerationManager::open(&request.store_dir)?.load(&identity)?;
        return emit(&WorkerEvent::Completed {
            vectors: store.len(),
        });
    }
    let retries = previous.as_ref().map_or(0, |record| {
        record
            .retries
            .saturating_add(u32::from(record.state == JobState::Running))
    });
    if retries >= request.max_retries {
        emit(&WorkerEvent::Failed {
            message: "finite worker retry budget exhausted".into(),
            retries,
        })?;
        return Err(anyhow!("finite worker retry budget exhausted"));
    }
    if previous.is_some() {
        // The ledger is advisory. Reconcile it from the new scan while the
        // embedding cache independently preserves completed inference.
        ledger.remove_job(&request.job_id)?;
    }
    let attempt = retries.saturating_add(1);
    let mut running = JobRecord {
        id: request.job_id.clone(),
        protocol_version: WORKER_PROTOCOL_VERSION,
        recipe,
        next_batch: 0,
        total_batches: 1,
        retries,
        max_retries: request.max_retries,
        state: JobState::Running,
        updated_at: now(),
    };
    ledger.commit_job_batch(&running, &[])?;
    emit(&WorkerEvent::Started { attempt })?;

    let source_path = request
        .source_artifacts
        .as_ref()
        .ok_or_else(|| anyhow!("shared semantic source artifact export is missing"))?;
    let source_file = std::fs::File::open(source_path).context("open semantic source export")?;
    let source_chunks: Vec<tldr_core::semantic::CodeChunk> =
        ciborium::de::from_reader(source_file).context("decode semantic source export")?;
    let mut latest_progress = None;
    let result = load_or_build_store_from_artifacts_with_progress(
        &request.project,
        &request.store_dir,
        &request.options,
        request.cache_config,
        source_chunks,
        &mut |mut progress| {
            progress.retries = retries;
            if progress.windows_completed > running.next_batch {
                running.next_batch = progress.windows_completed;
                running.total_batches = running.next_batch.saturating_add(1);
                running.updated_at = now();
                ledger.commit_job_batch(&running, &[])?;
                emit(&WorkerEvent::BatchCommitted {
                    batch: running.next_batch - 1,
                })
                .map_err(|error| tldr_core::TldrError::Embedding(error.to_string()))?;
            }
            latest_progress = Some(progress.clone());
            emit(&WorkerEvent::Progress { progress })
                .map_err(|error| tldr_core::TldrError::Embedding(error.to_string()))
        },
    );
    match result {
        Ok(store) => {
            if let Some(metrics_path) = request.metrics_output.as_ref() {
                let write_result = store
                    .build_metrics()
                    .ok_or_else(|| {
                        anyhow!("worker metrics were requested but the build produced no report")
                    })
                    .and_then(|report| {
                        let file = std::fs::File::create(metrics_path).with_context(|| {
                            format!("create metrics {}", metrics_path.display())
                        })?;
                        serde_json::to_writer_pretty(file, report)
                            .context("write semantic worker metrics")
                    });
                if let Err(error) = write_result {
                    eprintln!("[tldr-warn] semantic metrics output failed: {error}");
                }
            }
            let completed_batches = latest_progress
                .as_ref()
                .map_or(running.next_batch, |progress| progress.windows_completed);
            let completed = JobRecord {
                next_batch: completed_batches,
                total_batches: completed_batches,
                state: JobState::Completed,
                updated_at: now(),
                ..running
            };
            // The generation is durable before this checkpoint and acknowledgement.
            ledger.commit_job_batch(&completed, &[])?;
            emit(&WorkerEvent::Completed {
                vectors: store.len(),
            })
        }
        Err(error) => {
            let failed = JobRecord {
                state: if attempt >= request.max_retries {
                    JobState::Failed
                } else {
                    JobState::Pending
                },
                retries: attempt,
                updated_at: now(),
                ..running
            };
            ledger.commit_job_batch(&failed, &[])?;
            emit(&WorkerEvent::Failed {
                message: error.to_string(),
                retries: attempt,
            })?;
            Err(error.into())
        }
    }
}

fn emit(event: &WorkerEvent) -> Result<()> {
    let bytes = encode_worker_message(event)?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.flush()?;
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
