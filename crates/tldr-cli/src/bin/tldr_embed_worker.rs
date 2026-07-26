//! Disposable bulk embedding worker. The daemon communicates over bounded JSONL stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use tldr_core::semantic::{
    decode_worker_message, encode_worker_message, load_or_build_store, GenerationManager,
    JobRecord, JobState, RedbStore, WorkerBuildRequest, WorkerEvent, DEFAULT_REDB_CACHE_BYTES,
    MAX_WORKER_MESSAGE_BYTES, WORKER_PROTOCOL_VERSION,
};

fn main() -> Result<()> {
    let mut input = Vec::new();
    BufReader::new(std::io::stdin())
        .take(MAX_WORKER_MESSAGE_BYTES as u64 + 1)
        .read_until(b'\n', &mut input)
        .context("read worker request")?;
    let request: WorkerBuildRequest = decode_worker_message(&input)?;
    request.validate()?;
    let recipe = request_recipe(&request)?;
    let ledger_path = request.store_dir.join("worker-jobs.redb");
    let ledger = RedbStore::open(&ledger_path, DEFAULT_REDB_CACHE_BYTES)?;
    let previous = ledger.get_job(&request.job_id)?;
    if previous
        .as_ref()
        .is_some_and(|record| record.state == JobState::Completed)
    {
        let identity =
            tldr_core::semantic::store_search::manifest_id_for(&request.project, &request.options);
        let store = GenerationManager::open(&request.store_dir)?.load(&identity)?;
        return emit(&WorkerEvent::Completed {
            vectors: store.len(),
        });
    }
    let attempt = previous
        .as_ref()
        .map_or(1, |record| record.retries.saturating_add(1));
    if attempt > request.max_retries {
        emit(&WorkerEvent::Failed {
            message: "finite worker retry budget exhausted".into(),
            retries: previous.map_or(0, |record| record.retries),
        })?;
        return Err(anyhow!("finite worker retry budget exhausted"));
    }
    let running = JobRecord {
        id: request.job_id.clone(),
        protocol_version: WORKER_PROTOCOL_VERSION,
        recipe,
        next_batch: 0,
        total_batches: 1,
        retries: attempt,
        max_retries: request.max_retries,
        state: JobState::Running,
        updated_at: now(),
    };
    ledger.commit_job_batch(&running, &[])?;
    emit(&WorkerEvent::Started { attempt })?;

    match load_or_build_store(
        &request.project,
        &request.store_dir,
        &request.options,
        request.cache_config,
    ) {
        Ok(store) => {
            let completed = JobRecord {
                next_batch: 1,
                state: JobState::Completed,
                updated_at: now(),
                ..running
            };
            // The generation is durable before this checkpoint and acknowledgement.
            ledger.commit_job_batch(&completed, &[])?;
            emit(&WorkerEvent::BatchCommitted { batch: 0 })?;
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

fn request_recipe(request: &WorkerBuildRequest) -> Result<[u8; 32]> {
    let bytes = serde_json::to_vec(request)?;
    let digest = md5::compute(bytes);
    let mut recipe = [0_u8; 32];
    recipe[..16].copy_from_slice(&digest.0);
    recipe[16..].copy_from_slice(&digest.0);
    Ok(recipe)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
