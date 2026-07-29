use std::collections::BTreeSet;

use tldr_cli::commands::route_contract::{
    validate_command_capabilities, COMMAND_CAPABILITIES, PROTOCOL_CAPABILITIES,
};

fn command_variants_from_main() -> BTreeSet<String> {
    let main_rs = include_str!("../src/main.rs");
    let body = main_rs
        .split_once("pub enum Command {")
        .expect("top-level Command enum")
        .1
        .split_once("/// Daemon subcommands")
        .expect("DaemonCommand boundary")
        .0;

    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let (candidate, _) = trimmed.split_once('(')?;
            (!candidate.is_empty()
                && candidate.chars().all(|ch| ch.is_ascii_alphanumeric())
                && candidate
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_uppercase()))
            .then(|| candidate.to_string())
        })
        .collect()
}

fn daemon_variants_from_protocol() -> BTreeSet<String> {
    let types_rs = include_str!("../src/commands/daemon/types.rs");
    let body = types_rs
        .split_once("pub enum DaemonCommand {")
        .expect("DaemonCommand enum")
        .1
        .split_once("\n}")
        .expect("DaemonCommand closing brace")
        .0;

    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let candidate = trimmed
                .strip_suffix(',')
                .or_else(|| trimmed.strip_suffix(" {"))?;
            (!candidate.is_empty()
                && candidate.chars().all(|ch| ch.is_ascii_alphanumeric())
                && candidate
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_uppercase()))
            .then(|| candidate.to_string())
        })
        .collect()
}

#[test]
fn every_top_level_command_has_exactly_one_execution_owner() {
    validate_command_capabilities().expect("valid command capability registry");

    let actual = command_variants_from_main();
    let declared = COMMAND_CAPABILITIES
        .iter()
        .map(|capability| capability.variant.to_string())
        .collect::<BTreeSet<_>>();

    assert_eq!(declared, actual);
}

#[test]
fn every_daemon_variant_has_an_explicit_supported_client() {
    validate_command_capabilities().expect("valid command and protocol registries");

    let actual = daemon_variants_from_protocol();
    let declared = PROTOCOL_CAPABILITIES
        .iter()
        .map(|capability| capability.variant.to_string())
        .collect::<BTreeSet<_>>();

    assert_eq!(declared, actual);
}

#[test]
fn workspace_has_one_daemon_runtime() {
    let workspace = include_str!("../../../Cargo.toml");
    let cli_manifest = include_str!("../Cargo.toml");
    assert!(
        !workspace.contains("\"crates/tldr-daemon\""),
        "standalone tldr-daemon remains a workspace member"
    );
    assert!(
        !cli_manifest.contains("tldr-daemon = { path = \"../tldr-daemon\""),
        "tldr-cli still links the duplicate daemon crate"
    );
    assert!(
        !cli_manifest.contains("path = \"src/bin/tldr_daemon.rs\""),
        "duplicate tldr-daemon wrapper binary remains packaged"
    );
}

#[test]
fn legacy_cold_and_json_cache_routes_are_not_public() {
    let router = include_str!("../src/commands/daemon_router.rs");
    let enriched = include_str!("../../tldr-core/src/search/enriched.rs");
    let hybrid = include_str!("../../tldr-core/src/search/hybrid.rs");
    let semantic = include_str!("../../tldr-core/src/semantic/store_search.rs");

    assert!(!router.contains("pub fn try_daemon_route"));
    for obsolete in [
        "pub fn enriched_search_with_index",
        "pub fn enriched_search_with_callgraph_cache",
        "pub fn enriched_search_with_structure_cache",
        "pub fn read_structure_cache",
        "pub fn write_structure_cache",
        "pub fn read_callgraph_cache",
    ] {
        assert!(
            !enriched.contains(obsolete),
            "obsolete API remains: {obsolete}"
        );
    }
    assert!(!hybrid.contains("pub fn hybrid_search("));
    assert!(!semantic.contains("pub fn search_with_store"));
    assert!(!semantic.contains("pub fn query_store("));
}
