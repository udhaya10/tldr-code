//! Agent lifecycle context packing and durable code-context continuity.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::time::Instant;

use tldr_core::artifact_store::GenerationSnapshot;

use super::types::{ContextPack, SessionStats};

const MAX_PERSISTED_SESSIONS: usize = 32;
const MAX_HOT_ITEMS: usize = 64;
const OUTPUT_CHAR_LIMIT: usize = 9_500;

#[derive(Debug)]
struct Candidate {
    score: u64,
    file: String,
    symbol: String,
    line: u32,
    kind: String,
    signature: String,
}

/// Build a prompt/session-aware context pack from one pinned generation.
pub fn build_context_pack(
    snapshot: &GenerationSnapshot,
    session: &SessionStats,
    project_hot_files: &BTreeMap<String, u64>,
    prompt: &str,
    event: &str,
    lifecycle_source: Option<&str>,
    max_tokens: usize,
) -> ContextPack {
    let started = Instant::now();
    let query = words(prompt);
    let lifecycle_context = event.eq_ignore_ascii_case("SessionStart")
        || event.eq_ignore_ascii_case("PostCompact")
        || event.eq_ignore_ascii_case("PreCompact");
    let mut candidates = snapshot
        .definitions()
        .filter_map(|(file, definition)| {
            let symbol_words = words(&definition.name);
            let file_words = words(file);
            let lexical = query
                .iter()
                .map(|word| {
                    u64::from(symbol_words.contains(word)) * 12
                        + u64::from(file_words.contains(word)) * 6
                        + u64::from(definition.name.to_ascii_lowercase().contains(word.as_str()))
                            * 3
                })
                .sum::<u64>();
            let session_score = session.hot_files.get(file).copied().unwrap_or(0) * 8
                + session
                    .hot_symbols
                    .get(&definition.name)
                    .copied()
                    .unwrap_or(0)
                    * 10;
            let persisted_score = project_hot_files.get(file).copied().unwrap_or(0) * 2;
            let orientation_score =
                u64::from(lifecycle_context && query.is_empty() && definition.line_start > 0);
            let score = lexical + session_score + persisted_score + orientation_score;
            (score > 0).then(|| Candidate {
                score,
                file: file.to_string(),
                symbol: definition.name.clone(),
                line: definition.line_start,
                kind: definition.kind.clone(),
                signature: definition.signature.clone(),
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.symbol.cmp(&right.symbol))
    });

    let max_chars = max_tokens.max(64).saturating_mul(4).min(OUTPUT_CHAR_LIMIT);
    let source = if event.eq_ignore_ascii_case("UserPromptSubmit") {
        if session.requests > 0 {
            "session"
        } else {
            "prompt"
        }
    } else if lifecycle_context
        && lifecycle_source.is_some_and(|source| source.eq_ignore_ascii_case("compact"))
    {
        "compaction"
    } else {
        "project"
    };
    let mut content = format!(
        "<tldr-context source={source} generation={}>\n",
        snapshot.generation()
    );
    content.push_str(
        "Relevant project facts from the current indexed generation; paths are project-relative.\n",
    );
    let mut files = Vec::new();
    let mut symbols = Vec::new();
    let mut selected = HashSet::<(String, String)>::new();
    let mut truncated = false;
    for candidate in candidates {
        let row = format!(
            "{}:{}\t{}\t{}\n",
            candidate.file, candidate.line, candidate.kind, candidate.signature
        );
        if content.len() + row.len() + 16 > max_chars {
            truncated = true;
            break;
        }
        if selected.insert((candidate.file.clone(), candidate.symbol.clone())) {
            content.push_str(&row);
            files.push(candidate.file);
            symbols.push(candidate.symbol);
        }
    }

    if !selected.is_empty() {
        content.push_str("relations:\n");
        let selected_symbols = symbols.iter().cloned().collect::<HashSet<_>>();
        for edge in snapshot.call_edges(None).filter(|edge| {
            selected_symbols.contains(&edge.caller) || selected_symbols.contains(&edge.callee)
        }) {
            let row = format!(
                "{}:{} -> {}:{}\n",
                edge.source_file, edge.caller, edge.destination_file, edge.callee
            );
            if content.len() + row.len() + 16 > max_chars {
                truncated = true;
                break;
            }
            content.push_str(&row);
        }
    }
    content.push_str("</tldr-context>");
    if selected.is_empty() {
        content.clear();
    }
    files.sort();
    files.dedup();
    symbols.sort();
    symbols.dedup();
    ContextPack {
        tokens: content.len().div_ceil(4),
        content,
        files,
        symbols,
        generation: snapshot.generation(),
        elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
        truncated,
        source: source.to_string(),
    }
}

/// Aggregate a bounded project hot set from prior and active sessions.
pub fn aggregate_hot_files<'a>(
    sessions: impl IntoIterator<Item = &'a SessionStats>,
) -> BTreeMap<String, u64> {
    let mut scores = BTreeMap::new();
    for session in sessions {
        for (file, weight) in &session.hot_files {
            *scores.entry(file.clone()).or_default() += weight;
        }
    }
    trim_scores(scores)
}

/// Restore persisted code-context continuity. Corrupt/old files are ignored;
/// hooks must always fail open.
pub fn load_sessions(project: &Path) -> Vec<SessionStats> {
    std::fs::read(session_path(project))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<SessionStats>>(&bytes).ok())
        .unwrap_or_default()
}

/// Atomically persist only the bounded code-context/session ledger.
pub fn persist_sessions(project: &Path, sessions: &[SessionStats]) -> std::io::Result<()> {
    let path = session_path(project);
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;
    let mut sessions = sessions.to_vec();
    sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    sessions.truncate(MAX_PERSISTED_SESSIONS);
    let payload = serde_json::to_vec_pretty(&sessions)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, payload)?;
    std::fs::rename(temporary, path)
}

fn session_path(project: &Path) -> std::path::PathBuf {
    project.join(".tldr").join("session-context.json")
}

fn trim_scores(scores: BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    let mut ordered = scores.into_iter().collect::<Vec<_>>();
    ordered.sort_unstable_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ordered.truncate(MAX_HOT_ITEMS);
    ordered.into_iter().collect()
}

fn words(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| word.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tldr_core::artifact_store::{
        GenerationSnapshot, IngestionEngine, IngestionScope, RedbArtifactStore,
    };

    use super::{build_context_pack, load_sessions, persist_sessions};
    use crate::commands::daemon::types::SessionStats;

    #[test]
    fn prompt_pack_is_bounded_relevant_and_persistent() {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(
            project.path().join("billing.py"),
            "def calculate_invoice(total):\n    return total\n\ndef unrelated():\n    return 0\n",
        )
        .expect("fixture");
        let store =
            Arc::new(RedbArtifactStore::open(&project.path().join("store.redb")).expect("store"));
        IngestionEngine::new(project.path(), store.clone())
            .expect("engine")
            .ingest(IngestionScope::Project)
            .expect("ingest");
        let snapshot = GenerationSnapshot::active(store.as_ref())
            .expect("snapshot read")
            .expect("snapshot");
        let mut session = SessionStats::new("session-1".into());
        let pack = build_context_pack(
            &snapshot,
            &session,
            &Default::default(),
            "change invoice calculation",
            "UserPromptSubmit",
            None,
            128,
        );
        assert!(pack.content.contains("calculate_invoice"));
        assert!(!pack.content.contains("unrelated"));
        assert!(pack.tokens <= 128);

        let startup = build_context_pack(
            &snapshot,
            &session,
            &Default::default(),
            "",
            "SessionStart",
            Some("startup"),
            128,
        );
        let compact = build_context_pack(
            &snapshot,
            &session,
            &Default::default(),
            "",
            "SessionStart",
            Some("compact"),
            128,
        );
        assert_eq!(startup.source, "project");
        assert_eq!(compact.source, "compaction");

        session.touch_context(pack.files.iter().map(String::as_str), std::iter::empty());
        persist_sessions(project.path(), &[session]).expect("persist");
        let loaded = load_sessions(project.path());
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].hot_files.contains_key("billing.py"));
    }
}
