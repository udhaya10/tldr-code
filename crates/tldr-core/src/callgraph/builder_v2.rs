//! Builder V2 - Main entry point with parallel processing (Phase 14)
//!
//! This module provides the V2 implementation of the call graph builder with:
//! - Parallel file processing via rayon
//! - String interning for memory efficiency
//! - Explicit tree drops for memory management
//! - Integration with all 17 language handlers
//!
//! # Feature Gate
//! Canonical implementation (no feature flag required).
//!
//! # Example
//! ```rust,ignore
//! use tldr_core::callgraph::builder_v2::{build_project_call_graph_v2, BuildConfig};
//!
//! let config = BuildConfig {
//!     language: "python".to_string(),
//!     parallelism: 4,
//!     ..Default::default()
//! };
//!
//! let ir = build_project_call_graph_v2(Path::new("src"), config)?;
//! println!("Found {} files with {} functions", ir.file_count(), ir.function_count());
//! ```

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use rayon::prelude::*;

use super::cross_file_types::{CallGraphIR, CallSite, CallType, FileIR};
use super::import_resolver::{ImportResolver, ReExportTracer};
use super::module_index::ModuleIndex;
use super::type_resolver::{expand_union_type, MAX_UNION_EXPANSION};
use crate::types::Language;

// --- Re-exports for backward compatibility (sub-modules are private) ---
pub use super::imports::{
    augment_go_module_imports, build_import_map, build_import_map_with_index,
    extract_python_imports, resolve_imports_for_file, trace_reexport_with_cycle_detection,
    ImportMap, ModuleImports,
};
pub use super::module_path::path_to_module;
pub use super::resolution::{
    apply_type_resolution, resolve_call, resolve_call_with_receiver, ResolutionContext,
    ResolvedTarget,
};
pub use super::scanner::{filter_tldrignored, scan_project_files, should_skip_path, ScannedFile};
pub use super::types::{
    BuildConfig, BuildDiagnostics, BuildError, BuildResult, ClassEntry, ClassIndex, FuncEntry,
    FuncIndex, ParseDiagnostic, ResolutionWarning, SkipReason,
};

// --- Internal imports from sub-modules ---
use super::module_path::{extract_definitions, normalize_path_relative_to_root};
use super::resolution::{
    compute_via_import, enclosing_class_for_call, first_base_for_class, resolve_caller_name,
    resolve_constructor_target, resolve_method_in_bases, resolve_method_in_class,
};
use super::scanner::{is_supported_language, normalize_language_string};
use super::types::PYTHON_BUILTINS;
use super::var_types::FileParseResult;

/// Build the exact V2 per-file IR from the syntax tree already produced by
/// unified ingestion.
pub(crate) fn file_ir_from_tree(
    root: &Path,
    path: &Path,
    language: &str,
    source: &str,
    tree: &tree_sitter::Tree,
) -> FileIR {
    let canonical_root = root.canonicalize().ok();
    let relative_path = normalize_path_relative_to_root(path, root, canonical_root.as_deref());
    let parsed = super::module_path::extract_definitions_from_tree(source, path, language, tree);
    FileIR {
        path: relative_path,
        funcs: parsed.funcs,
        classes: parsed.classes,
        imports: parsed.imports,
        var_types: parsed.var_types,
        calls: parsed.calls,
    }
}

// =============================================================================
// Parallel Index Building (Spec Section 14.5)
// =============================================================================

/// Build function and class indices in parallel using rayon.
///
/// This function processes all files in parallel, extracting function and
/// class definitions, then merges the results into unified indices.
///
/// # Mitigations Implemented
/// - M1.4: Thread-local parsers via thread_local!
/// - M1.8: Flat par_iter, no nested parallelism
/// - M1.9: Phase barrier - indices built before resolution
///
/// # Arguments
/// * `files` - Scanned files to process
/// * `root` - Project root for relative path computation
/// * `language` - Language to parse
/// * `config` - Build configuration
///
/// # Returns
/// Tuple of (FuncIndex, ClassIndex, Vec<FileIR>)
pub fn build_indices_parallel(
    files: &[ScannedFile],
    root: &Path,
    language: &str,
    _config: &BuildConfig,
) -> (FuncIndex, ClassIndex, Vec<FileIR>) {
    // P1: Canonicalize root for consistent path operations (parity-fix-plan.yaml)
    let canonical_root = root.canonicalize().ok();

    // Process files in parallel using rayon
    // Per M1.8: Use flat par_iter, no nested parallelism
    let results: Vec<_> = files
        .par_iter()
        .map(|scanned| {
            // Read file content
            let content = match fs::read_to_string(&scanned.path) {
                Ok(c) => c,
                Err(_) => {
                    return (scanned.path.clone(), FileParseResult::default());
                }
            };

            // Extract definitions
            let result = extract_definitions(&content, &scanned.path, language);
            (scanned.path.clone(), result)
        })
        .collect();

    // Build indices from parallel results (single-threaded merge)
    // Per M1.9: Build indices completely before resolution phase
    let total_funcs: usize = results.iter().map(|(_, r)| r.funcs.len()).sum();
    let total_classes: usize = results.iter().map(|(_, r)| r.classes.len()).sum();

    let mut func_index = FuncIndex::with_capacity(total_funcs);
    let mut class_index = ClassIndex::with_capacity(total_classes);
    let mut file_irs = Vec::with_capacity(results.len());

    for (abs_path, parse_result) in results {
        // P1: Compute relative path with normalization (parity-fix-plan.yaml)
        let relative_path =
            normalize_path_relative_to_root(&abs_path, root, canonical_root.as_deref());

        // Compute module name from path (language-aware for ModuleIndex parity)
        let module = path_to_module(&relative_path, language);

        // Build FileIR
        let mut file_ir = FileIR::new(relative_path.clone());

        // Add functions to FileIR and index
        for func in parse_result.funcs {
            // Add to function index
            let entry = if func.is_method {
                FuncEntry::method(
                    relative_path.clone(),
                    func.line,
                    func.end_line,
                    func.class_name.clone().unwrap_or_default(),
                )
            } else {
                FuncEntry::function(relative_path.clone(), func.line, func.end_line)
            };

            func_index.insert(&module, &func.name, entry.clone());

            // BUG FIX 2: Index BOTH simple AND full module name (CROSSFILE_SPEC.md Section 2.2)
            // When resolving `from core import my_function`, we need to find it under both
            // "pkg.core" (full path) and "core" (simple name). Previously only full path was indexed.
            //
            // v031-issue-7: simple_module aliasing must NOT silently overwrite an existing
            // entry pointing at a different file. When two distinct modules share the same
            // simple_module suffix (e.g., `pkg1.foo` and `pkg2.foo`), `HashMap::insert`
            // would let the second writer clobber the first under `(simple_module, name)` —
            // closing the only path by which the losing file's definition could be looked
            // up via PYTHONPATH-style `from <simple> import <name>`. Suppress the alias
            // insert when collision detected (first-writer-wins, deterministic).
            let simple_module = module.split('.').next_back().unwrap_or(&module);
            if simple_module != module
                && func_index
                    .get(simple_module, &func.name)
                    .map(|e| e.file_path == relative_path)
                    .unwrap_or(true)
            {
                func_index.insert(simple_module, &func.name, entry);
            }

            // Also index as Class.method if it's a method
            if let Some(ref class_name) = func.class_name {
                let qualified = format!("{}.{}", class_name, func.name);
                let method_entry = FuncEntry::method(
                    relative_path.clone(),
                    func.line,
                    func.end_line,
                    class_name.clone(),
                );
                func_index.insert(&module, &qualified, method_entry.clone());
                // v031-issue-7: same first-writer-wins guard for the qualified alias.
                if simple_module != module
                    && func_index
                        .get(simple_module, &qualified)
                        .map(|e| e.file_path == relative_path)
                        .unwrap_or(true)
                {
                    func_index.insert(simple_module, &qualified, method_entry);
                }
            }

            // Add to FileIR
            file_ir.funcs.push(func);
        }

        // Add classes to FileIR and index
        for class in parse_result.classes {
            // Add to class index
            let entry = ClassEntry::new(
                relative_path.clone(),
                class.line,
                class.end_line,
                class.methods.clone(),
                class.bases.clone(),
            );
            class_index.insert(&class.name, entry);

            // Add to FileIR
            file_ir.classes.push(class);
        }

        // Add imports to FileIR (Phase 14d)
        file_ir.imports = parse_result.imports;

        // Add calls to FileIR (Phase 14d)
        file_ir.calls = parse_result.calls;

        // Add VarType information to FileIR (Phase: VarType extraction)
        file_ir.var_types = parse_result.var_types;

        file_irs.push(file_ir);
    }

    (func_index, class_index, file_irs)
}

// =============================================================================
// Call Resolution (Spec Section 14.6)
// =============================================================================

/// Result of extracting and resolving calls from a file.
#[derive(Debug, Default)]
pub struct ResolvedCalls {
    /// Resolved calls: (CallSite, ResolvedTarget)
    pub resolved: Vec<(CallSite, ResolvedTarget)>,

    /// Unresolved calls (external, stdlib, or cannot be resolved)
    pub unresolved: Vec<CallSite>,

    /// Warnings generated during resolution
    pub warnings: Vec<ResolutionWarning>,
}

/// Extract and resolve all calls from a file.
///
/// This function processes all call sites in a FileIR and attempts to resolve
/// each one to its target definition using the various indices.
///
/// # Arguments
/// * `file_ir` - The FileIR containing calls to resolve
/// * `context` - Shared resolution indexes and state for this file
///
/// # Returns
/// ResolvedCalls containing resolved and unresolved calls
pub fn extract_and_resolve_calls(
    file_ir: &FileIR,
    context: &mut ResolutionContext<'_, '_>,
) -> ResolvedCalls {
    let scoped_import_maps = HashMap::new();
    extract_and_resolve_calls_with_scoped_imports(file_ir, context, &scoped_import_maps)
}

fn extract_and_resolve_calls_with_scoped_imports(
    file_ir: &FileIR,
    context: &mut ResolutionContext<'_, '_>,
    scoped_import_maps: &HashMap<String, (ImportMap, ModuleImports)>,
) -> ResolvedCalls {
    let mut result = ResolvedCalls::default();
    let current_file = &file_ir.path;
    let mut builder_context = BuilderResolutionContext {
        resolution_context: context,
        scoped_import_maps,
    };

    for call_sites in file_ir.calls.values() {
        for call_site in call_sites {
            if let Some(super_target) = resolve_super_constructor_call(
                file_ir,
                call_site,
                builder_context.resolution_context.class_index,
                builder_context.resolution_context.func_index,
                builder_context.resolution_context.language,
            ) {
                result.resolved.push((call_site.clone(), super_target));
                continue;
            }
            match resolve_call_site_for_builder(
                file_ir,
                call_site,
                &mut builder_context,
                &mut result,
            ) {
                CallSiteResolution::Handled => {}
                CallSiteResolution::Resolved(target) => {
                    if PYTHON_BUILTINS.contains(&target.name.as_str()) {
                        continue;
                    }
                    result.resolved.push((call_site.clone(), target));
                }
                CallSiteResolution::Unresolved => {
                    if call_site.target.contains("__import__")
                        || call_site.target.contains("importlib")
                    {
                        result.warnings.push(ResolutionWarning {
                            file: current_file.clone(),
                            line: call_site.line.unwrap_or(0),
                            target: call_site.target.clone(),
                            reason: "Dynamic import pattern cannot be resolved statically"
                                .to_string(),
                        });
                    }
                    result.unresolved.push(call_site.clone());
                }
            }
        }
    }

    result
}

enum CallSiteResolution {
    Handled,
    Resolved(ResolvedTarget),
    Unresolved,
}

struct BuilderResolutionContext<'ctx, 'a, 'b> {
    resolution_context: &'ctx mut ResolutionContext<'a, 'b>,
    scoped_import_maps: &'ctx HashMap<String, (ImportMap, ModuleImports)>,
}

impl BuilderResolutionContext<'_, '_, '_> {
    fn resolve_call(&mut self, target: &str, call_type: &CallType) -> Option<ResolvedTarget> {
        resolve_call(target, call_type, self.resolution_context)
    }

    fn resolve_call_with_receiver(
        &mut self,
        target: &str,
        receiver: &str,
        receiver_type: Option<&str>,
        call_type: &CallType,
    ) -> Option<ResolvedTarget> {
        resolve_call_with_receiver(
            target,
            receiver,
            receiver_type,
            call_type,
            self.resolution_context,
        )
    }

    fn resolve_local_import_call(&mut self, call_site: &CallSite) -> Option<ResolvedTarget> {
        let maps = self.scoped_import_maps.get(&call_site.caller)?;
        let context = &mut self.resolution_context;
        let mut local_context = ResolutionContext {
            import_map: &maps.0,
            module_imports: &maps.1,
            func_index: context.func_index,
            class_index: context.class_index,
            reexport_tracer: context.reexport_tracer,
            current_file: context.current_file,
            root: context.root,
            language: context.language,
        };
        match call_site.receiver.as_deref() {
            Some(receiver) => resolve_call_with_receiver(
                &call_site.target,
                receiver,
                call_site.receiver_type.as_deref(),
                &CallType::LocalImport,
                &mut local_context,
            ),
            None => resolve_call(
                &call_site.target,
                &CallType::LocalImport,
                &mut local_context,
            ),
        }
    }
}

fn resolve_super_constructor_call(
    file_ir: &FileIR,
    call_site: &CallSite,
    class_index: &ClassIndex,
    func_index: &FuncIndex,
    language: &str,
) -> Option<ResolvedTarget> {
    let supports_super_ctor = matches!(
        language,
        "java"
            | "kotlin"
            | "scala"
            | "swift"
            | "typescript"
            | "tsx"
            | "javascript"
            | "js"
            | "csharp"
    );
    if !supports_super_ctor
        || !matches!(
            call_site.call_type,
            CallType::Direct | CallType::LocalImport | CallType::Intra
        )
        || call_site.target != "super"
    {
        return None;
    }
    let class_name = enclosing_class_for_call(&file_ir.funcs, call_site)?;
    let base = first_base_for_class(&file_ir.classes, &class_name)?;
    let class_entry = class_index.get(&base)?;
    if let Some(ctor_target) = resolve_constructor_target(&base, class_entry, func_index, language)
    {
        return Some(ctor_target);
    }
    Some(ResolvedTarget {
        file: class_entry.file_path.clone(),
        name: base,
        line: Some(class_entry.line),
        is_method: false,
        class_name: None,
    })
}

fn resolve_call_site_for_builder(
    file_ir: &FileIR,
    call_site: &CallSite,
    context: &mut BuilderResolutionContext<'_, '_, '_>,
    result: &mut ResolvedCalls,
) -> CallSiteResolution {
    let resolved = match call_site.call_type {
        CallType::Intra => resolve_intra_call(file_ir, call_site, context),
        CallType::LocalImport => context.resolve_local_import_call(call_site),
        CallType::Static => resolve_static_call(file_ir, call_site, context),
        CallType::Method | CallType::Attr => {
            return resolve_method_or_attr_call(call_site, context, result);
        }
        _ => context.resolve_call(&call_site.target, &call_site.call_type),
    };

    match resolved {
        Some(target) => CallSiteResolution::Resolved(target),
        None => CallSiteResolution::Unresolved,
    }
}

fn resolve_intra_call(
    file_ir: &FileIR,
    call_site: &CallSite,
    context: &mut BuilderResolutionContext<'_, '_, '_>,
) -> Option<ResolvedTarget> {
    let class_index = context.resolution_context.class_index;
    let func_index = context.resolution_context.func_index;
    let language = context.resolution_context.language;

    if let Some(func) = file_ir
        .funcs
        .iter()
        .find(|func| func.name == call_site.target && !func.is_method)
    {
        return Some(ResolvedTarget {
            file: file_ir.path.clone(),
            name: func.name.clone(),
            line: Some(func.line),
            is_method: false,
            class_name: None,
        });
    }
    if let Some(class_name) = enclosing_class_for_call(&file_ir.funcs, call_site) {
        if let Some(target) = resolve_method_in_class(
            &class_name,
            &call_site.target,
            class_index,
            func_index,
            language,
        ) {
            return Some(target);
        }
        if let Some(target) = resolve_method_in_bases(
            &class_name,
            &call_site.target,
            class_index,
            func_index,
            language,
        ) {
            return Some(target);
        }
        return context.resolve_call(&call_site.target, &call_site.call_type);
    }
    context.resolve_call(&call_site.target, &call_site.call_type)
}

fn resolve_static_call(
    file_ir: &FileIR,
    call_site: &CallSite,
    context: &mut BuilderResolutionContext<'_, '_, '_>,
) -> Option<ResolvedTarget> {
    let class_index = context.resolution_context.class_index;
    let func_index = context.resolution_context.func_index;
    let language = context.resolution_context.language;

    let Some((receiver, method)) = call_site.target.split_once("::") else {
        return context.resolve_call(&call_site.target, &call_site.call_type);
    };

    let receiver_key = receiver.trim();
    if receiver_key == "self" || receiver_key == "static" {
        if let Some(class_name) = enclosing_class_for_call(&file_ir.funcs, call_site) {
            if let Some(target) =
                resolve_method_in_class(&class_name, method, class_index, func_index, language)
            {
                return Some(target);
            }
            if let Some(target) =
                resolve_method_in_bases(&class_name, method, class_index, func_index, language)
            {
                return Some(target);
            }
        }
        return context.resolve_call(&call_site.target, &call_site.call_type);
    }

    if receiver_key == "parent" || receiver_key == "base" || receiver_key == "super" {
        if let Some(class_name) = enclosing_class_for_call(&file_ir.funcs, call_site) {
            if let Some(base) = first_base_for_class(&file_ir.classes, &class_name) {
                if let Some(target) =
                    resolve_method_in_class(&base, method, class_index, func_index, language)
                {
                    return Some(target);
                }
                if let Some(target) =
                    resolve_method_in_bases(&base, method, class_index, func_index, language)
                {
                    return Some(target);
                }
            }
        }
    }

    context.resolve_call(&call_site.target, &call_site.call_type)
}

fn resolve_method_or_attr_call(
    call_site: &CallSite,
    context: &mut BuilderResolutionContext<'_, '_, '_>,
    result: &mut ResolvedCalls,
) -> CallSiteResolution {
    let Some(receiver) = call_site.receiver.as_ref() else {
        return match context.resolve_call(&call_site.target, &call_site.call_type) {
            Some(target) => CallSiteResolution::Resolved(target),
            None => CallSiteResolution::Unresolved,
        };
    };

    let mut receiver_type_for_resolution = call_site.receiver_type.as_deref().map(Cow::Borrowed);

    if let Some(raw_receiver_type) = call_site.receiver_type.as_deref() {
        match expand_union_type(raw_receiver_type, Some(MAX_UNION_EXPANSION)) {
            Some(members) => {
                if members.len() > 1 {
                    let mut seen: HashSet<(PathBuf, String)> = HashSet::new();
                    let mut resolved_any = false;
                    for member in members {
                        if let Some(target) = context.resolve_call_with_receiver(
                            &call_site.target,
                            receiver,
                            Some(member.as_str()),
                            &call_site.call_type,
                        ) {
                            let key = (target.file.clone(), target.qualified_name());
                            if seen.insert(key) {
                                result.resolved.push((call_site.clone(), target));
                            }
                            resolved_any = true;
                        }
                    }
                    if resolved_any {
                        return CallSiteResolution::Handled;
                    }
                    receiver_type_for_resolution = None;
                } else if let Some(single) = members.first() {
                    receiver_type_for_resolution = Some(Cow::Owned(single.clone()));
                }
            }
            None => {
                result.warnings.push(ResolutionWarning {
                    file: context.resolution_context.current_file.to_path_buf(),
                    line: call_site.line.unwrap_or(0),
                    target: call_site.target.clone(),
                    reason: "Union type too large to expand; skipping type-aware resolution"
                        .to_string(),
                });
                receiver_type_for_resolution = None;
            }
        }
    }

    match context.resolve_call_with_receiver(
        &call_site.target,
        receiver,
        receiver_type_for_resolution.as_deref(),
        &call_site.call_type,
    ) {
        Some(target) => CallSiteResolution::Resolved(target),
        None => CallSiteResolution::Unresolved,
    }
}

// =============================================================================
// Main Entry Point (Spec Section 14.2)
// =============================================================================

/// Build a complete project-wide call graph.
///
/// This is the V2 implementation with:
/// - Parallel file processing via rayon
/// - String interning for memory efficiency
/// - Explicit tree drops for memory management
/// - Integration with all 17 language handlers
///
/// # Arguments
/// * `root` - Project root directory
/// * `config` - Builder configuration
///
/// # Returns
/// * `Result<CallGraphIR, BuildError>` - Complete call graph or error
///
/// # Errors
/// * `BuildError::RootNotFound` - if root directory doesn't exist
/// * `BuildError::UnsupportedLanguage` - if language not in registry
/// * `BuildError::Io` - for file system errors
///
/// # Example
/// ```rust,ignore
/// let config = BuildConfig {
///     language: "python".to_string(),
///     parallelism: 0, // auto-detect
///     ..Default::default()
/// };
/// let ir = build_project_call_graph_v2(Path::new("src"), config)?;
/// ```
pub fn build_project_call_graph_v2(
    root: &Path,
    mut config: BuildConfig,
) -> Result<CallGraphIR, BuildError> {
    // Step 1: Validate inputs
    if !root.exists() {
        return Err(BuildError::RootNotFound(root.to_path_buf()));
    }

    if !root.is_dir() {
        return Err(BuildError::RootNotFound(root.to_path_buf()));
    }

    config.language = normalize_language_string(&config.language);

    if !is_supported_language(&config.language) {
        return Err(BuildError::UnsupportedLanguage(config.language.clone()));
    }

    // Gate-1 phase timers (TLDR-zde): env-gated, zero cost when unset. Splits
    // the build into SCAN / PARSE (per-file, memoizable) / COMPOSE (global
    // resolution) so the chunk-store design's compose budget can be measured
    // on the real pipeline before any storage work. Enable: TLDR_PHASE_TIMING=1.
    let phase_timing = std::env::var_os("TLDR_PHASE_TIMING").is_some();
    let t_start = std::time::Instant::now();

    // Step 2: Scan project files (Phase 14b)
    let scanned_files = scan_project_files(root, &config.language, &config)?;
    let t_scan = t_start.elapsed();

    // Step 4: Build function and class indices in parallel (Phase 14c)
    // Per M1.9: Build indices completely before resolution phase
    let (_func_index, _class_index, file_irs) =
        build_indices_parallel(&scanned_files, root, &config.language, &config);
    let t_parse = t_start.elapsed() - t_scan;

    // Steps 3 + 5-11 (TLDR-iqr seam): compose the graph from the per-file IRs.
    let ir = compose_call_graph_v2(root, &config, file_irs)?;

    if phase_timing {
        let t_compose = t_start.elapsed() - t_scan - t_parse;
        eprintln!(
            "[phase-timing] lang={} files={} funcs={} edges={} | scan={}ms parse={}ms compose={}ms total={}ms",
            config.language,
            ir.files.len(),
            ir.function_count(),
            ir.edges.len(),
            t_scan.as_millis(),
            t_parse.as_millis(),
            t_compose.as_millis(),
            t_start.elapsed().as_millis(),
        );
    }

    Ok(ir)
}

/// TLDR-iqr seam: scan + parse phase only — produces the per-file IRs that
/// [`compose_call_graph_v2`] consumes. Behavior-identical to the front half
/// of [`build_project_call_graph_v2`]; the daemon's FileIR memo re-parses
/// single changed files by calling [`build_indices_parallel`] on a one-file
/// slice and composing with memoized IRs for the rest.
pub fn parse_project_file_irs(
    root: &Path,
    config: &BuildConfig,
) -> Result<Vec<FileIR>, BuildError> {
    if !root.exists() || !root.is_dir() {
        return Err(BuildError::RootNotFound(root.to_path_buf()));
    }
    let language = normalize_language_string(&config.language);
    if !is_supported_language(&language) {
        return Err(BuildError::UnsupportedLanguage(language));
    }
    let scanned_files = scan_project_files(root, &language, config)?;
    let (_func_index, _class_index, file_irs) =
        build_indices_parallel(&scanned_files, root, &language, config);
    Ok(file_irs)
}

/// TLDR-iqr seam: compose phase — everything after parse (IR assembly,
/// module/function/class indexes, import resolution, call resolution, edge
/// dedup + canonical sort). Pure function of (root manifests/disk, config,
/// file_irs): chaining [`parse_project_file_irs`] into this function is
/// byte-identical to [`build_project_call_graph_v2`] (7-corpus sha256 gate,
/// TLDR-iqr). NOTE: when `config.use_type_resolution` is set, the resolution
/// loop still reads source files for receiver-type enrichment — folding that
/// into parse-time FileIR artifacts is TLDR-726 (S3) scope, deliberately NOT
/// done here (it would change observable FileIR serialization; Codex round-3
/// Q6 finding on TLDR-iqr).
pub fn compose_call_graph_v2(
    root: &Path,
    config: &BuildConfig,
    file_irs: Vec<FileIR>,
) -> Result<CallGraphIR, BuildError> {
    let mut config = config.clone();
    config.language = normalize_language_string(&config.language);

    // Step 3: Create IR with capacity hint
    let mut ir = CallGraphIR::with_capacity(root.to_path_buf(), &config.language, file_irs.len());

    // Step 5: Add FileIRs to the CallGraphIR
    for file_ir in file_irs {
        ir.add_file(file_ir);
    }

    // Step 6: Build the indices within CallGraphIR
    // This populates func_index and class_index within the IR itself
    ir.build_indices();

    // Phase 14d-14f: Import Resolution and Cross-File Edge Creation
    // Per M1.9: Build indices completely before resolution phase (done above)
    //
    // Step 7: Build ModuleIndex for import resolution.
    //
    // VAL-007: when a workspace config is active, read manifests
    // (tsconfig.json, package.json, ...) from every workspace root so
    // per-package path aliases such as `@/*` in `apps/web/tsconfig.json`
    // resolve correctly. Without this, monorepo imports collapse to the
    // AST-fallback path and emit misleading "no callers" reports.
    let extra_roots: Vec<PathBuf> = if config.use_workspace_config {
        config
            .workspace_roots
            .iter()
            .filter(|p| p.as_path() != root)
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let module_index = ModuleIndex::build_with_workspace_roots(
        root,
        &config.language,
        config.respect_ignore,
        &extra_roots,
    )
    .map_err(|e| BuildError::Io(std::io::Error::other(e.to_string())))?;

    // Step 8: ImportResolver/ReExportTracer are created PER RAYON WORKER in
    // the resolution par_iter below (TLDR-zro) — see the map_init note there.

    // Step 9: Build FuncIndex and ClassIndex for call resolution
    // We need our own copies because the IR's indices use a different format
    let mut func_index = FuncIndex::with_capacity(ir.function_count());
    let mut class_index = ClassIndex::with_capacity(ir.class_count());

    // high-bundle-progress-determinism-coverage-v1 (N2): iterate `ir.files`
    // in a stable, sorted order so that index-population collisions (same
    // simple_module alias from multiple files) resolve to the same winner
    // on every run. Without this, the call graph's `total_edges` count
    // jitters across runs because different first-writers shape which
    // calls are resolvable through the simple_module alias.
    let sorted_files: Vec<(&PathBuf, &super::cross_file_types::FileIR)> = {
        let mut v: Vec<_> = ir.files.iter().collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v
    };

    // Populate indices from IR
    for (file_path, file_ir) in &sorted_files {
        let file_path: &PathBuf = file_path;
        let file_ir: &super::cross_file_types::FileIR = file_ir;
        let module = path_to_module(file_path, &config.language);

        for func in &file_ir.funcs {
            let entry = if func.is_method {
                FuncEntry::method(
                    file_path.clone(),
                    func.line,
                    func.end_line,
                    func.class_name.clone().unwrap_or_default(),
                )
            } else {
                FuncEntry::function(file_path.clone(), func.line, func.end_line)
            };
            func_index.insert(&module, &func.name, entry.clone());

            // BUG FIX 2: Index BOTH simple AND full module name (CROSSFILE_SPEC.md Section 2.2)
            // Only for Python-style dot-separated modules (e.g., "pkg.helper" -> also index as "helper")
            // TS/JS use ./ prefix (not dot-separated), Go uses /, Rust uses ::
            //
            // v031-issue-7: see build_indices_parallel above — first-writer-wins on the
            // simple_module alias slot prevents silent overwrite when two distinct
            // modules share the same suffix (`pkg1.foo` vs `pkg2.foo`).
            let is_python_style = !module.starts_with("./")
                && !module.starts_with("crate::")
                && !module.contains('/');
            let simple_module = if is_python_style {
                module.split('.').next_back().unwrap_or(&module)
            } else {
                &module // No simple alias for non-Python languages
            };
            if is_python_style
                && simple_module != module.as_str()
                && func_index
                    .get(simple_module, &func.name)
                    .map(|e| e.file_path == *file_path)
                    .unwrap_or(true)
            {
                func_index.insert(simple_module, &func.name, entry);
            }

            // Also index as Class.method if it's a method
            if let Some(ref class_name) = func.class_name {
                let qualified = format!("{}.{}", class_name, func.name);
                let method_entry = FuncEntry::method(
                    file_path.clone(),
                    func.line,
                    func.end_line,
                    class_name.clone(),
                );
                func_index.insert(&module, &qualified, method_entry.clone());
                // v031-issue-7: same first-writer-wins guard for the qualified alias.
                if is_python_style
                    && simple_module != module.as_str()
                    && func_index
                        .get(simple_module, &qualified)
                        .map(|e| e.file_path == *file_path)
                        .unwrap_or(true)
                {
                    func_index.insert(simple_module, &qualified, method_entry);
                }
            }
        }

        for class in &file_ir.classes {
            let entry = ClassEntry::new(
                file_path.clone(),
                class.line,
                class.end_line,
                class.methods.clone(),
                class.bases.clone(),
            );
            class_index.insert(&class.name, entry);
        }
    }

    // Go interface dispatch: identify interface types before the method merge pass.
    // Interfaces have methods extracted from their type declaration AST (non-empty),
    // while structs have empty methods at this point (methods come from FuncDef merge below).
    let go_interface_names: HashSet<String> = if config.language == "go" {
        class_index
            .iter()
            .filter(|(_, entry)| !entry.methods.is_empty())
            .map(|(name, _)| name.to_string())
            .collect()
    } else {
        HashSet::new()
    };

    // Merge method definitions into class index (extensions/partials).
    // high-bundle-progress-determinism-coverage-v1 (N2): same sorted order
    // as the populate-indices loop above, for the same reason — first
    // writer wins on the class_index, and HashMap iteration is random.
    for (file_path, file_ir) in &sorted_files {
        let file_path: &PathBuf = file_path;
        let file_ir: &super::cross_file_types::FileIR = file_ir;
        for func in &file_ir.funcs {
            if !func.is_method {
                continue;
            }
            let class_name = match func.class_name.as_deref() {
                Some(name) => name,
                None => continue,
            };

            if let Some(entry) = class_index.get_mut(class_name) {
                if !entry.methods.contains(&func.name) {
                    entry.methods.push(func.name.clone());
                }
            } else {
                class_index.insert(
                    class_name,
                    ClassEntry::new(
                        file_path.clone(),
                        func.line,
                        func.end_line,
                        vec![func.name.clone()],
                        Vec::new(),
                    ),
                );
            }
        }
    }

    // Go interface dispatch: wire interface→implementor relationships.
    // For each Go interface, find all structs whose method sets are a superset
    // of the interface's method set. Add those struct names as "bases" of the
    // interface so that resolve_method_in_bases() can resolve interface method
    // calls to concrete implementations.
    if config.language == "go" && !go_interface_names.is_empty() {
        // Collect interface method sets
        let interface_methods: Vec<(String, Vec<String>)> = go_interface_names
            .iter()
            .filter_map(|name| {
                class_index
                    .get(name)
                    .map(|entry| (name.clone(), entry.methods.clone()))
            })
            .collect();

        // For each interface, find concrete implementors
        for (iface_name, iface_methods) in &interface_methods {
            if iface_methods.is_empty() {
                continue;
            }
            let mut implementors = Vec::new();
            for (class_name, class_entry) in class_index.iter() {
                // Skip interfaces themselves
                if go_interface_names.contains(class_name) {
                    continue;
                }
                // Check if this struct has all methods of the interface
                let has_all = iface_methods
                    .iter()
                    .all(|m| class_entry.methods.contains(m));
                if has_all {
                    implementors.push(class_name.to_string());
                }
            }
            // Add implementors as "bases" of the interface
            if !implementors.is_empty() {
                if let Some(iface_entry) = class_index.get_mut(iface_name) {
                    for imp in implementors {
                        if !iface_entry.bases.contains(&imp) {
                            iface_entry.bases.push(imp);
                        }
                    }
                }
            }
        }
    }

    // Step 9b (REMOVED, TLDR-zro / Codex round-1 Q3 VERIFIED): a
    // TypeAwareCallResolver was constructed here and fed a full clone of
    // every FileIR, then never read — pure dead work in the compose hot
    // path (django: 2,917 FileIR clones per build). add_file_ir only
    // populates resolver-local maps (type_aware_resolver.rs), so removal
    // is behavior-preserving; verified by the 7-corpus sha256 gate.

    // Step 10: For each file, resolve imports and then resolve calls.
    //
    // TLDR-zro: this loop is parallelized with a flat rayon par_iter (M1.8:
    // no nested parallelism — same global pool the parse phase uses). Every
    // shared input (ModuleIndex, FuncIndex, ClassIndex, root) is read-only;
    // the only mutable state, the two memo caches, memoizes PURE lookups:
    //   - ImportResolver's LRU key includes `current_file`, so there is no
    //     cross-file entry sharing to lose by going per-worker;
    //   - ReExportTracer is keyed (module, name, max_depth) over immutable
    //     ModuleIndex + on-disk content.
    // `map_init` gives each rayon WORKER its own cache pair (duplicate pure
    // lookups across workers are accepted; zero contention, no locks).
    //
    // Determinism: per-file output is a pure function of (FileIR, immutable
    // indexes, frozen disk), `par_iter().map().collect()` preserves input
    // order, and the dedup+insert below consumes the per-file edge vecs in
    // the SAME sorted file order as the previous sequential loop — so the
    // edge set, insertion sequence, and final canonical sort are all
    // byte-identical to the sequential build (verified by the 7-corpus
    // sha256 gate on TLDR-zro).
    //
    // high-bundle-progress-determinism-coverage-v1 (N2): the underlying
    // `ir.files` is a `HashMap<PathBuf, FileIR>` whose iteration order is
    // randomized per process; sort the paths so every run resolves files
    // in the same sequence.
    let mut file_paths: Vec<PathBuf> = ir.files.keys().cloned().collect();
    file_paths.sort();

    use super::cross_file_types::CrossFileCallEdge;
    let per_file_edges: Vec<Vec<CrossFileCallEdge>> = file_paths
        .par_iter()
        .map_init(
            || {
                (
                    ImportResolver::with_default_cache(&module_index),
                    ReExportTracer::new(&module_index),
                )
            },
            |(import_resolver, reexport_tracer), file_path| {
                // Get the FileIR (need to clone to avoid borrow issues)
                let mut file_ir = match ir.files.get(file_path) {
                    Some(f) => f.clone(),
                    None => return Vec::new(),
                };

                if config.use_type_resolution {
                    if let Ok(lang) = Language::from_str(&config.language) {
                        if let Ok(source) = fs::read_to_string(root.join(&file_ir.path)) {
                            apply_type_resolution(&mut file_ir, &source, lang);
                        }
                    }
                }

                // Step 10a: Resolve imports for this file
                let resolved_imports = resolve_imports_for_file(&file_ir, import_resolver, root);

                // Step 10b: Build import map from resolved imports.
                //
                // VAL-007: pass the ModuleIndex so aliased imports (e.g. `@/util`)
                // get rewritten to the canonical func_index key (e.g.
                // `./apps/web/src/util`). Without this, cross-file edges through
                // tsconfig path aliases are silently dropped.
                let global_imports: Vec<_> = resolved_imports
                    .iter()
                    .filter(|resolved| resolved.original.scope.is_none())
                    .cloned()
                    .collect();
                let (import_map, mut module_imports) =
                    build_import_map_with_index(&global_imports, Some(&module_index));

                let mut local_groups: HashMap<String, Vec<_>> = HashMap::new();
                for resolved in resolved_imports
                    .iter()
                    .filter(|resolved| resolved.original.scope.is_some())
                {
                    if let Some(scope) = &resolved.original.scope {
                        local_groups
                            .entry(scope.clone())
                            .or_default()
                            .push(resolved.clone());
                    }
                }
                let scoped_import_maps: HashMap<String, (ImportMap, ModuleImports)> = local_groups
                    .into_iter()
                    .map(|(scope, imports)| {
                        let (local_imports, local_modules) =
                            build_import_map_with_index(&imports, Some(&module_index));
                        let mut scoped_imports = import_map.clone();
                        scoped_imports.extend(local_imports);
                        let mut scoped_modules = module_imports.clone();
                        scoped_modules.extend(local_modules);
                        (scope, (scoped_imports, scoped_modules))
                    })
                    .collect();

                // Step 10b.1: Augment module_imports for Go cross-package function calls.
                // Go imports use full module paths that don't match func_index keys directly.
                // This bridges the gap by mapping Go package aliases to func_index module keys.
                if config.language == "go" {
                    augment_go_module_imports(&file_ir.imports, &mut module_imports, &func_index);
                }

                // Step 10c: Resolve calls using the import map and indices
                let mut resolution_context = ResolutionContext {
                    import_map: &import_map,
                    module_imports: &module_imports,
                    func_index: &func_index,
                    class_index: &class_index,
                    reexport_tracer,
                    current_file: &file_ir.path,
                    root,
                    language: &config.language,
                };
                let resolved_calls = extract_and_resolve_calls_with_scoped_imports(
                    &file_ir,
                    &mut resolution_context,
                    &scoped_import_maps,
                );

                // Step 10d: Map resolved calls to edges (no shared writes here;
                // dedup + IR insertion happen serially below, in file order).
                resolved_calls
                    .resolved
                    .into_iter()
                    .map(|(call_site, target)| {
                        let src_func = resolve_caller_name(&file_ir, &call_site);
                        let via_import =
                            compute_via_import(&call_site, &import_map, &module_imports);
                        CrossFileCallEdge {
                            src_file: file_path.clone(),
                            src_func,
                            dst_file: target.file.clone(),
                            dst_func: target.qualified_name(),
                            call_type: call_site.call_type,
                            via_import,
                        }
                    })
                    .collect()
            },
        )
        .collect();

    // Serial dedup + insertion, in the same sorted file order as before.
    let mut edge_set: HashSet<CrossFileCallEdge> = HashSet::with_capacity(ir.function_count() * 4);
    for edges in per_file_edges {
        for edge in edges {
            if edge_set.insert(edge.clone()) {
                ir.add_edge(edge);
            }
        }
    }

    // high-bundle-progress-determinism-coverage-v1 (N2): even with sorted
    // file iteration above, downstream consumers and tests benefit from a
    // canonical edge order. Sort by (src_file, src_func, dst_file,
    // dst_func, call_type) so JSON output is byte-stable across runs.
    ir.edges.sort_by(|a, b| {
        a.src_file
            .cmp(&b.src_file)
            .then_with(|| a.src_func.cmp(&b.src_func))
            .then_with(|| a.dst_file.cmp(&b.dst_file))
            .then_with(|| a.dst_func.cmp(&b.dst_func))
            .then_with(|| format!("{:?}", a.call_type).cmp(&format!("{:?}", b.call_type)))
    });

    Ok(ir)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod scoped_import_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn python_local_imports_resolve_per_function_without_leaking() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tldr-local-import-scope-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create fixture");
        fs::write(root.join("alpha.py"), "def work():\n    pass\n").expect("write alpha");
        fs::write(root.join("beta.py"), "def work():\n    pass\n").expect("write beta");
        fs::write(
            root.join("main.py"),
            r#"
def one():
    from alpha import work
    work()

def two():
    from beta import work
    work()

def before_binding():
    work()
    from alpha import work

def sibling():
    work()
"#,
        )
        .expect("write main");

        let graph = build_project_call_graph_v2(
            &root,
            BuildConfig {
                language: "python".to_string(),
                respect_ignore: false,
                ..BuildConfig::default()
            },
        )
        .expect("build graph");

        let one = graph
            .edges
            .iter()
            .find(|edge| edge.src_func == "one" && edge.dst_func == "work")
            .expect("one edge");
        assert_eq!(one.dst_file, PathBuf::from("alpha.py"));
        assert_eq!(one.call_type, CallType::LocalImport);

        let two = graph
            .edges
            .iter()
            .find(|edge| edge.src_func == "two" && edge.dst_func == "work")
            .expect("two edge");
        assert_eq!(two.dst_file, PathBuf::from("beta.py"));
        assert_eq!(two.call_type, CallType::LocalImport);

        assert!(!graph.edges.iter().any(|edge| {
            matches!(edge.src_func.as_str(), "before_binding" | "sibling")
                && edge.dst_func == "work"
        }));

        fs::remove_dir_all(&root).expect("remove fixture");
    }
}
