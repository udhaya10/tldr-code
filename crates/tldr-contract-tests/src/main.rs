use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tldr_cli::commands::daemon::artifact_manager::ArtifactManager;
use tldr_core::artifact_store::{
    schema::STORE_FILE, ArtifactKey, ArtifactKind, ArtifactStore, ArtifactSubject, FileFactsParser,
    FunctionArtifactCoordinator, GenerationManifest, GenerationSnapshot, IngestionEngine,
    IngestionScope, ProducerId, ProjectId, RedbArtifactStore,
};
use tldr_core::semantic::vector_store::{ChunkMeta, VectorStore};
use tldr_core::semantic::{ChunkId, ChunkRevision, StructuralAnchor};
use tldr_core::Language;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Profile {
    Smoke,
    Certification,
}

struct Scenario {
    name: &'static str,
    smoke: bool,
    run: fn() -> Result<(), String>,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "artifact_lifecycle",
        smoke: true,
        run: artifact_lifecycle,
    },
    Scenario {
        name: "deterministic_rank_1",
        smoke: true,
        run: deterministic_rank_1,
    },
    Scenario {
        name: "resume_after_checkpoint",
        smoke: false,
        run: resume_after_checkpoint,
    },
    Scenario {
        name: "generation_monotonicity",
        smoke: false,
        run: generation_monotonicity,
    },
    Scenario {
        name: "checkpoint_scope_isolation",
        smoke: false,
        run: checkpoint_scope_isolation,
    },
    Scenario {
        name: "cache_clear_symlink_containment",
        smoke: false,
        run: cache_clear_symlink_containment,
    },
    Scenario {
        name: "source_path_containment",
        smoke: false,
        run: source_path_containment,
    },
    Scenario {
        name: "full_project_deletion",
        smoke: false,
        run: full_project_deletion,
    },
    Scenario {
        name: "ignore_matcher_unification",
        smoke: false,
        run: ignore_matcher_unification,
    },
    Scenario {
        name: "language_matrix",
        smoke: false,
        run: language_matrix,
    },
];

fn main() {
    let profile = match std::env::args().nth(1).as_deref() {
        Some("smoke") => Profile::Smoke,
        Some("certification") => Profile::Certification,
        _ => {
            eprintln!("usage: tldr-contract-tests <smoke|certification>");
            std::process::exit(2);
        }
    };
    let selected = SCENARIOS
        .iter()
        .filter(|scenario| profile == Profile::Certification || scenario.smoke)
        .collect::<Vec<_>>();
    let mut failed = 0;
    for scenario in &selected {
        match (scenario.run)() {
            Ok(()) => println!("ok   {}", scenario.name),
            Err(error) => {
                failed += 1;
                eprintln!("FAIL {}\n     {error}", scenario.name);
            }
        }
    }
    println!(
        "profile={profile:?} cases={} passed={} failed={failed}",
        selected.len(),
        selected.len() - failed
    );
    if failed > 0 {
        std::process::exit(1);
    }
}

fn artifact_lifecycle() -> Result<(), String> {
    let project = tempfile::tempdir().map_err(display)?;
    copy_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lifecycle"),
        project.path(),
    )?;
    let store_path = project.path().join(".tldr/store").join(STORE_FILE);
    let store = Arc::new(RedbArtifactStore::open(&store_path).map_err(display)?);
    let engine = IngestionEngine::new(project.path(), store.clone()).map_err(display)?;

    let cold = engine.ingest(IngestionScope::Project).map_err(display)?;
    ensure(
        cold.generation == 1 && cold.parsed_files == 2,
        "cold build counters",
    )?;
    let first = GenerationSnapshot::active(store.as_ref())
        .map_err(display)?
        .ok_or("cold build did not publish")?;
    ensure(first.file_count() == 2, "cold file count")?;
    ensure(
        first.call_edges(Some(Language::Python)).any(|edge| {
            edge.source_file == "main.py"
                && edge.caller == "entry"
                && edge.destination_file == "helper.py"
                && edge.callee == "authenticate"
        }),
        "cross-file call projection",
    )?;

    let warm = engine.ingest(IngestionScope::Project).map_err(display)?;
    ensure(
        warm.generation == 2 && warm.parsed_files == 0,
        "warm reuse counters",
    )?;

    let facts = first.file("helper.py").ok_or("helper facts missing")?;
    let project_id = ProjectId::for_root(project.path()).map_err(display)?;
    let input = ArtifactKey {
        project: project_id,
        revision: facts.revision,
        subject: ArtifactSubject::File("helper.py".into()),
        kind: ArtifactKind::FileFacts,
        producer: ProducerId::new("file-facts", 4),
    };
    let optional = ArtifactKey {
        project: project_id,
        revision: facts.revision,
        subject: ArtifactSubject::Symbol("helper.py::authenticate".into()),
        kind: ArtifactKind::Cfg,
        producer: ProducerId::new("contract-cfg", 1),
    };
    let coordinator = FunctionArtifactCoordinator::new(store.clone());
    let builds = AtomicUsize::new(0);
    for _ in 0..2 {
        let value: u64 = coordinator
            .materialize(optional.clone(), vec![input.clone()], || {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(7)
            })
            .map_err(display)?;
        ensure(value == 7, "optional artifact payload")?;
    }
    ensure(
        builds.load(Ordering::SeqCst) == 1,
        "optional artifact cache hit",
    )?;

    fs::write(
        project.path().join("helper.py"),
        "def authenticate(token):\n    return token.startswith(\"valid\")\n",
    )
    .map_err(display)?;
    let delta = engine
        .ingest(IngestionScope::Files(vec!["helper.py".into()]))
        .map_err(display)?;
    ensure(
        delta.generation == 3 && delta.parsed_files == 1,
        "delta counters",
    )?;
    let current = GenerationSnapshot::active(store.as_ref())
        .map_err(display)?
        .ok_or("delta did not publish")?;
    ensure(current.file_count() == 2, "delta carried unchanged files")?;
    ensure(
        current.file("main.py").is_some(),
        "delta dropped unchanged main.py",
    )?;
    ensure(
        !store
            .generation(3)
            .map_err(display)?
            .ok_or("delta manifest missing")?
            .artifacts
            .contains(&optional),
        "delta retained stale optional artifact",
    )?;
    store.set_vector_generation(3, 11).map_err(display)?;
    ensure(
        store
            .generation(3)
            .map_err(display)?
            .and_then(|manifest| manifest.vector_generation)
            == Some(11),
        "vector generation join",
    )?;
    drop(current);
    drop(first);
    drop(engine);
    drop(coordinator);
    drop(store);

    let reopened = RedbArtifactStore::open(&store_path).map_err(display)?;
    let restarted = GenerationSnapshot::active(&reopened)
        .map_err(display)?
        .ok_or("restart lost active generation")?;
    ensure(restarted.generation() == 3, "restart generation")?;
    ensure(
        no_json_under(&project.path().join(".tldr"))?,
        "persistent JSON found",
    )?;
    Ok(())
}

fn deterministic_rank_1() -> Result<(), String> {
    let mut store = VectorStore::new(3, 2).map_err(display)?;
    store
        .add(1, &[1.0, 0.0, 0.0], meta(1, "helper.py", "authenticate"))
        .map_err(display)?;
    store
        .add(2, &[0.0, 1.0, 0.0], meta(2, "parser.py", "parse"))
        .map_err(display)?;
    let hits = store.search(&[0.99, 0.01, 0.0], 2).map_err(display)?;
    ensure(hits.len() == 2, "rank result count")?;
    ensure(
        hits[0].key == 1 && hits[0].meta.function_name.as_deref() == Some("authenticate"),
        "expected authenticate at rank 1",
    )
}

fn resume_after_checkpoint() -> Result<(), String> {
    let project = tempfile::tempdir().map_err(display)?;
    for index in 0..40 {
        fs::write(
            project.path().join(format!("file_{index:02}.py")),
            format!("def function_{index:02}():\n    return {index}\n"),
        )
        .map_err(display)?;
    }
    let store = Arc::new(
        RedbArtifactStore::open(&project.path().join(".tldr/store").join(STORE_FILE))
            .map_err(display)?,
    );
    let engine = IngestionEngine::new(project.path(), store.clone()).map_err(display)?;
    ensure(
        engine
            .ingest_interrupted_after(IngestionScope::Project, 1)
            .is_err(),
        "interruption hook did not stop",
    )?;
    ensure(
        store.active_generation().map_err(display)?.is_none(),
        "partial generation published",
    )?;
    let resumed = engine.ingest(IngestionScope::Project).map_err(display)?;
    ensure(resumed.resumed, "job did not report resume")?;
    ensure(
        resumed.parsed_files == 8,
        "resume did not skip committed batch",
    )?;
    let snapshot = GenerationSnapshot::active(store.as_ref())
        .map_err(display)?
        .ok_or("resume did not publish")?;
    ensure(snapshot.file_count() == 40, "resumed manifest coverage")
}

fn generation_monotonicity() -> Result<(), String> {
    let project = tempfile::tempdir().map_err(display)?;
    copy_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lifecycle"),
        project.path(),
    )?;
    let store = Arc::new(
        RedbArtifactStore::open(&project.path().join(".tldr/store").join(STORE_FILE))
            .map_err(display)?,
    );
    let engine = IngestionEngine::new(project.path(), store.clone()).map_err(display)?;
    engine.ingest(IngestionScope::Project).map_err(display)?;
    let first = store
        .generation(1)
        .map_err(display)?
        .ok_or("first generation missing")?;
    store
        .publish(&GenerationManifest {
            generation: 3,
            ..first.clone()
        })
        .map_err(display)?;
    ensure(
        store
            .publish(&GenerationManifest {
                generation: 2,
                ..first
            })
            .is_err(),
        "store accepted a generation older than active",
    )?;
    ensure(
        store.active_generation().map_err(display)? == Some(3),
        "failed publication changed the active generation",
    )
}

fn checkpoint_scope_isolation() -> Result<(), String> {
    let project = tempfile::tempdir().map_err(display)?;
    fs::write(project.path().join("a.py"), "def a():\n    return 1\n").map_err(display)?;
    fs::write(project.path().join("b.py"), "def b():\n    return 1\n").map_err(display)?;
    let store = Arc::new(
        RedbArtifactStore::open(&project.path().join(".tldr/store").join(STORE_FILE))
            .map_err(display)?,
    );
    let engine = IngestionEngine::new(project.path(), store.clone()).map_err(display)?;
    engine.ingest(IngestionScope::Project).map_err(display)?;
    let baseline = GenerationSnapshot::active(store.as_ref())
        .map_err(display)?
        .ok_or("baseline generation missing")?;
    let old_a = baseline
        .file("a.py")
        .ok_or("baseline a.py missing")?
        .revision;
    let old_b = baseline
        .file("b.py")
        .ok_or("baseline b.py missing")?
        .revision;

    fs::write(project.path().join("a.py"), "def a():\n    return 2\n").map_err(display)?;
    fs::write(project.path().join("b.py"), "def b():\n    return 2\n").map_err(display)?;
    ensure(
        engine
            .ingest_interrupted_after(IngestionScope::Files(vec!["a.py".into()]), 1)
            .is_err(),
        "file-a interruption hook did not stop",
    )?;
    let b_report = engine
        .ingest(IngestionScope::Files(vec!["b.py".into()]))
        .map_err(display)?;
    ensure(!b_report.resumed, "file-b delta resumed file-a checkpoint")?;
    let current = GenerationSnapshot::active(store.as_ref())
        .map_err(display)?
        .ok_or("file-b delta did not publish")?;
    ensure(
        current.file("a.py").ok_or("a.py missing")?.revision == old_a,
        "file-b delta leaked the interrupted file-a artifact",
    )?;
    ensure(
        current.file("b.py").ok_or("b.py missing")?.revision != old_b,
        "file-b delta did not publish its own revision",
    )
}

#[cfg(unix)]
fn cache_clear_symlink_containment() -> Result<(), String> {
    use std::os::unix::fs::symlink;

    use tldr_cli::commands::daemon::CacheClearArgs;
    use tldr_cli::output::OutputFormat;

    let project = tempfile::tempdir().map_err(display)?;
    let external = tempfile::tempdir().map_err(display)?;
    let sentinel = external.path().join("must-survive.txt");
    fs::write(&sentinel, "outside cache root").map_err(display)?;
    let cache = project.path().join(".tldr/cache");
    fs::create_dir_all(&cache).map_err(display)?;
    symlink(external.path(), cache.join("external-link")).map_err(display)?;

    CacheClearArgs {
        project: project.path().to_path_buf(),
    }
    .run(OutputFormat::Text, true)
    .map_err(display)?;
    ensure(
        sentinel.exists(),
        "cache clear followed a directory symlink outside the cache root",
    )
}

#[cfg(not(unix))]
fn cache_clear_symlink_containment() -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn source_path_containment() -> Result<(), String> {
    use std::os::unix::fs::symlink;

    let project = tempfile::tempdir().map_err(display)?;
    let external = tempfile::tempdir().map_err(display)?;
    let inside = project.path().join("inside.py");
    let outside = external.path().join("outside.py");
    let link = project.path().join("escape.py");
    let directory_link = project.path().join("external-directory");
    fs::write(&inside, "def inside():\n    return 1\n").map_err(display)?;
    fs::write(&outside, "def outside():\n    return 2\n").map_err(display)?;
    symlink(&outside, &link).map_err(display)?;
    symlink(external.path(), &directory_link).map_err(display)?;

    let parser = FileFactsParser::default();
    ensure(
        parser.parse(project.path(), &link).is_err(),
        "parser followed a source symlink outside the project",
    )?;
    ensure(
        parser.invocations() == 0,
        "parser read the source before containment validation",
    )?;

    let manager = ArtifactManager::open(project.path()).map_err(display)?;
    manager.warm().map_err(display)?;
    ensure(
        manager.apply_delta(&link).is_err(),
        "delta ingestion followed a source symlink outside the project",
    )?;
    ensure(
        manager
            .apply_delta(&directory_link.join("deleted.py"))
            .is_err(),
        "deleted delta escaped through a symlinked parent directory",
    )?;

    fs::remove_file(&inside).map_err(display)?;
    manager.apply_delta(&inside).map_err(display)?;
    ensure(
        manager
            .snapshot()
            .map_err(|state| format!("artifact manager not ready: {state:?}"))?
            .file("inside.py")
            .is_none(),
        "legitimate deleted-file delta remained queryable",
    )
}

#[cfg(not(unix))]
fn source_path_containment() -> Result<(), String> {
    Ok(())
}

fn full_project_deletion() -> Result<(), String> {
    let project = tempfile::tempdir().map_err(display)?;
    copy_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lifecycle"),
        project.path(),
    )?;
    let store = Arc::new(
        RedbArtifactStore::open(&project.path().join(".tldr/store").join(STORE_FILE))
            .map_err(display)?,
    );
    let engine = IngestionEngine::new(project.path(), store.clone()).map_err(display)?;
    engine.ingest(IngestionScope::Project).map_err(display)?;

    fs::remove_file(project.path().join("helper.py")).map_err(display)?;
    engine.ingest(IngestionScope::Project).map_err(display)?;
    let snapshot = GenerationSnapshot::active(store.as_ref())
        .map_err(display)?
        .ok_or("deletion generation missing")?;
    ensure(
        snapshot.file("helper.py").is_none(),
        "deleted file remained queryable after a full project ingestion",
    )?;
    ensure(
        !snapshot
            .call_edges(Some(Language::Python))
            .any(|edge| edge.source_file == "helper.py" || edge.destination_file == "helper.py"),
        "deleted file remained in the project call graph",
    )?;
    let manifest = store
        .generation(snapshot.generation())
        .map_err(display)?
        .ok_or("deletion manifest missing")?;
    ensure(
        !manifest.artifacts.iter().any(|key| match &key.subject {
            ArtifactSubject::File(path) => path == "helper.py",
            ArtifactSubject::Symbol(anchor) => anchor.starts_with("helper.py::"),
            ArtifactSubject::Project => false,
        }),
        "deleted file or symbol subject remained in the manifest",
    )
}

fn ignore_matcher_unification() -> Result<(), String> {
    use tldr_core::callgraph::filter_tldrignored;

    let project = tempfile::tempdir().map_err(display)?;
    let root_ignored = project.path().join("corpus/root.py");
    let nested_kept = project.path().join("nested/corpus/kept.py");
    fs::create_dir_all(root_ignored.parent().ok_or("root parent missing")?).map_err(display)?;
    fs::create_dir_all(nested_kept.parent().ok_or("nested parent missing")?).map_err(display)?;
    fs::write(&root_ignored, "def root(): pass\n").map_err(display)?;
    fs::write(&nested_kept, "def nested(): pass\n").map_err(display)?;
    fs::write(project.path().join(".tldrignore"), "/corpus/\n").map_err(display)?;

    let filtered = filter_tldrignored(
        project.path(),
        vec![root_ignored.clone(), nested_kept.clone()],
    );
    ensure(
        !filtered.contains(&root_ignored),
        "root-anchored ignore was not honored",
    )?;
    ensure(
        filtered.contains(&nested_kept),
        "root-anchored ignore matched a nested directory",
    )
}

fn language_matrix() -> Result<(), String> {
    let cases = [
        ("python", "case.py", "def answer():\n    return 42\n"),
        ("rust", "case.rs", "fn answer() -> i32 { 42 }\n"),
        (
            "typescript",
            "case.ts",
            "function answer(): number { return 42; }\n",
        ),
        (
            "javascript",
            "case.js",
            "function answer() { return 42; }\n",
        ),
        (
            "go",
            "case.go",
            "package sample\nfunc answer() int { return 42 }\n",
        ),
        (
            "java",
            "Case.java",
            "class Case { int answer() { return 42; } }\n",
        ),
        ("c", "case.c", "int answer(void) { return 42; }\n"),
        ("cpp", "case.cpp", "int answer() { return 42; }\n"),
        ("ruby", "case.rb", "def answer\n  42\nend\n"),
        (
            "swift",
            "case.swift",
            "func answer() -> Int { return 42 }\n",
        ),
        ("kotlin", "case.kt", "fun answer(): Int = 42\n"),
        (
            "csharp",
            "Case.cs",
            "class Case { int Answer() { return 42; } }\n",
        ),
        (
            "scala",
            "Case.scala",
            "object Case { def answer: Int = 42 }\n",
        ),
        (
            "php",
            "case.php",
            "<?php function answer() { return 42; }\n",
        ),
        ("lua", "case.lua", "function answer() return 42 end\n"),
        (
            "luau",
            "case.luau",
            "local function answer() return 42 end\n",
        ),
        (
            "elixir",
            "case.ex",
            "defmodule Case do\n  def answer, do: 42\nend\n",
        ),
        ("ocaml", "case.ml", "let answer () = 42\n"),
    ];
    let mut observed = BTreeSet::new();
    for (label, name, source) in cases {
        let project = tempfile::tempdir().map_err(display)?;
        let file = project.path().join(name);
        fs::write(&file, source).map_err(display)?;
        let facts = tldr_core::artifact_store::FileFactsParser::default()
            .parse(project.path(), &file)
            .map_err(|error| format!("{label}/{name}: {error}"))?;
        ensure(
            !facts.semantic_chunks.is_empty(),
            &format!("{label}: no semantic source"),
        )?;
        ensure(
            !facts.callgraph_ir.is_empty(),
            &format!("{label}: no callgraph IR"),
        )?;
        observed.insert(facts.language);
    }
    ensure(
        observed.len() == cases.len(),
        "language identities collapsed",
    )
}

fn meta(id: u128, file: &str, function: &str) -> ChunkMeta {
    ChunkMeta {
        identity: format!("{id:032x}"),
        chunk_id: ChunkId(id),
        revision: ChunkRevision::from_document(function),
        anchor: StructuralAnchor::default(),
        file_rel_path: file.into(),
        function_name: Some(function.into()),
        class_name: None,
        line_start: 1,
        line_end: 1,
        content_hash: format!("hash-{id}"),
        structure: Default::default(),
    }
}

fn copy_fixture(source: PathBuf, destination: &Path) -> Result<(), String> {
    for entry in fs::read_dir(source).map_err(display)? {
        let entry = entry.map_err(display)?;
        fs::copy(entry.path(), destination.join(entry.file_name())).map_err(display)?;
    }
    Ok(())
}

fn no_json_under(root: &Path) -> Result<bool, String> {
    if !root.exists() {
        return Ok(true);
    }
    for entry in fs::read_dir(root).map_err(display)? {
        let path = entry.map_err(display)?.path();
        if path.is_dir() {
            if !no_json_under(&path)? {
                return Ok(false);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_string())
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}
