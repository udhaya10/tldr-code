//! Module resolution for import tracking
//!
//! Resolves imports to their source files, handling:
//! - Python: relative imports, packages, __init__.py
//! - TypeScript: .ts/.tsx resolution, index files
//! - Go: package paths
//!
//! # Limitations (M3: Python Module Resolution)
//! - Does not search sys.path or PYTHONPATH
//! - Only resolves files within the project
//! - Namespace packages (PEP 420) may not resolve correctly

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::{ImportInfo, Language};

/// Module resolver for import tracking
#[derive(Debug, Default)]
pub struct ModuleResolver {
    /// Map of module name to file path
    module_index: HashMap<String, PathBuf>,
    /// Map of function name to (file, module) for cross-file resolution
    function_index: HashMap<String, Vec<(PathBuf, String)>>,
    /// Language being resolved
    language: Option<Language>,
    /// Project root
    root: PathBuf,
}

impl ModuleResolver {
    /// Create a new resolver for a project
    pub fn new(root: PathBuf) -> Self {
        Self {
            module_index: HashMap::new(),
            function_index: HashMap::new(),
            language: None,
            root,
        }
    }

    /// Set the language for resolution
    pub fn with_language(mut self, language: Language) -> Self {
        self.language = Some(language);
        self
    }

    /// Index a file's module name
    ///
    /// Bug fix: Index under BOTH the full module name (e.g., "pkg.helper") AND
    /// the simple name (last component, e.g., "helper"). This ensures that imports
    /// referencing the simple name can still be resolved.
    pub fn index_file(&mut self, file_path: &Path) {
        let module_name = self.path_to_module(file_path);

        // Index under full module name
        self.module_index
            .insert(module_name.clone(), file_path.to_path_buf());

        // Also index under the simple (last component) name for fallback resolution
        let simple_name = module_name.split('.').next_back().unwrap_or(&module_name);
        if simple_name != module_name {
            // Only insert if there isn't already a direct match for the simple name
            // (avoid overwriting a direct module with a nested one)
            self.module_index
                .entry(simple_name.to_string())
                .or_insert_with(|| file_path.to_path_buf());
        }
    }

    /// Index a function from a file
    pub fn index_function(&mut self, file_path: &Path, func_name: &str) {
        let module_name = self.path_to_module(file_path);
        self.function_index
            .entry(func_name.to_string())
            .or_default()
            .push((file_path.to_path_buf(), module_name));
    }

    /// Convert a file path to a module name
    pub fn path_to_module(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        let language = self.language.unwrap_or(Language::Python);

        match language {
            Language::Python => {
                // Convert path/to/file.py to path.to.file
                let stem = relative.with_extension("");
                let parts: Vec<&str> = stem.iter().filter_map(|s| s.to_str()).collect();
                parts.join(".")
            }
            Language::TypeScript | Language::JavaScript => {
                // Convert path/to/file.ts to ./path/to/file
                let stem = relative.with_extension("");
                format!("./{}", stem.display())
            }
            Language::Go => {
                // Use path as package
                relative
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            }
            Language::Java => {
                // Convert com/example/Foo.java to com.example.Foo
                // Handle Maven/Gradle src/main/java prefix
                let stem = relative.with_extension("");
                let path_str = stem.to_string_lossy();

                // Strip common source prefixes
                let cleaned = path_str
                    .strip_prefix("src/main/java/")
                    .or_else(|| path_str.strip_prefix("src/test/java/"))
                    .or_else(|| path_str.strip_prefix("src/"))
                    .unwrap_or(&path_str);

                // Convert path separators to dots
                cleaned.replace(['/', '\\'], ".")
            }
            Language::Rust => {
                // Convert src/foo/bar.rs to crate::foo::bar
                // Convert src/foo/mod.rs to crate::foo
                // Convert src/lib.rs to crate
                // Convert src/main.rs to crate
                let stem = relative.with_extension("");
                let parts: Vec<&str> = stem.iter().filter_map(|s| s.to_str()).collect();

                // Handle special cases
                if parts.is_empty() {
                    return "crate".to_string();
                }

                // Check for mod.rs or lib.rs/main.rs
                let last = parts.last().copied().unwrap_or("");
                let is_mod_file = last == "mod" || last == "lib" || last == "main";

                let mut module_parts = Vec::new();

                // Skip 'src' prefix if present
                let start_idx = if parts.first() == Some(&"src") { 1 } else { 0 };

                for (i, part) in parts.iter().enumerate().skip(start_idx) {
                    // Skip 'mod', 'lib', 'main' at the end
                    if i == parts.len() - 1 && is_mod_file {
                        continue;
                    }
                    module_parts.push(*part);
                }

                if module_parts.is_empty() {
                    "crate".to_string()
                } else {
                    format!("crate::{}", module_parts.join("::"))
                }
            }
            _ => relative.to_string_lossy().to_string(),
        }
    }

    /// Resolve an import to a file path
    pub fn resolve_import(&self, import: &ImportInfo, from_file: &Path) -> Option<PathBuf> {
        let language = self.language.unwrap_or(Language::Python);

        match language {
            Language::Python => self.resolve_python_import(import, from_file),
            Language::TypeScript | Language::JavaScript => {
                self.resolve_ts_import(import, from_file)
            }
            Language::Go => self.resolve_go_import(import),
            Language::Rust => self.resolve_rust_import(import, from_file),
            Language::Java => self.resolve_java_import(import, from_file),
            _ => None,
        }
    }

    /// Look up a module name in the index
    pub fn resolve_module(&self, module_name: &str) -> Option<PathBuf> {
        self.module_index.get(module_name).cloned()
    }

    /// Resolve a function call to its definition file
    ///
    /// Bug fixes applied:
    /// - Bug 1: Handle aliased from-imports (e.g., `from X import Y as Z` -- calling Z)
    /// - Bug 3: Try simple module name fallback when full path lookup fails
    pub fn resolve_function(&self, func_name: &str, imports: &[ImportInfo]) -> Option<PathBuf> {
        // Check if function is imported
        for import in imports {
            if import.is_from {
                // Check direct name match: from X import callee
                let is_direct_match = import.names.contains(&func_name.to_string());

                // Bug fix 1: Check alias match: from X import Y as callee
                let is_alias_match = import.alias.as_ref().is_some_and(|a| a == func_name);

                if is_direct_match || is_alias_match {
                    // Try full module name first, then simple name fallback (Bug fix 3)
                    if let Some(module_path) = self.module_index.get(&import.module) {
                        return Some(module_path.clone());
                    }
                    // Fallback: try simple module name (last component)
                    let simple = import
                        .module
                        .split('.')
                        .next_back()
                        .unwrap_or(&import.module);
                    if simple != import.module {
                        if let Some(module_path) = self.module_index.get(simple) {
                            return Some(module_path.clone());
                        }
                    }
                }
            }
            // Check for module.function pattern
            if !import.is_from {
                let qualified_name = format!("{}.{}", import.module, func_name);
                if self.function_index.contains_key(&qualified_name) {
                    if let Some(module_path) = self.module_index.get(&import.module) {
                        return Some(module_path.clone());
                    }
                }
            }
        }

        // Check if function is defined in any indexed file
        if let Some(locations) = self.function_index.get(func_name) {
            if locations.len() == 1 {
                return Some(locations[0].0.clone());
            }
        }

        None
    }

    /// Resolve a Python import
    fn resolve_python_import(&self, import: &ImportInfo, from_file: &Path) -> Option<PathBuf> {
        let module = &import.module;

        // Handle relative imports
        if module.starts_with('.') {
            let from_dir = from_file.parent()?;
            let dots = module.chars().take_while(|c| *c == '.').count();
            let mut base = from_dir.to_path_buf();

            // Go up directories for each dot after the first
            for _ in 1..dots {
                base = base.parent()?.to_path_buf();
            }

            // Get the rest of the module path
            let rest = module.trim_start_matches('.');
            if !rest.is_empty() {
                for part in rest.split('.') {
                    base = base.join(part);
                }
            }

            // Try as file
            let as_file = base.with_extension("py");
            if self.module_index.values().any(|p| p == &as_file) {
                return Some(as_file);
            }

            // Try as package (__init__.py)
            let as_package = base.join("__init__.py");
            if self.module_index.values().any(|p| p == &as_package) {
                return Some(as_package);
            }

            return None;
        }

        // Absolute import
        let module_path = module.replace('.', "/");

        // Try as file
        let as_file = self.root.join(&module_path).with_extension("py");
        if self.module_index.values().any(|p| p == &as_file) {
            return Some(as_file);
        }

        // Try as package
        let as_package = self.root.join(&module_path).join("__init__.py");
        if self.module_index.values().any(|p| p == &as_package) {
            return Some(as_package);
        }

        // Try direct lookup
        self.module_index.get(module).cloned()
    }

    /// Resolve a TypeScript/JavaScript import
    fn resolve_ts_import(&self, import: &ImportInfo, from_file: &Path) -> Option<PathBuf> {
        let module = &import.module;
        let from_dir = from_file.parent()?;

        // Handle relative imports
        if module.starts_with('.') {
            let target = from_dir.join(module);
            let target = dunce::canonicalize(&target).ok()?;

            // Try with extensions
            for ext in &[".ts", ".tsx", ".js", ".jsx"] {
                let with_ext = target.with_extension(&ext[1..]);
                if self.module_index.values().any(|p| p == &with_ext) {
                    return Some(with_ext);
                }
            }

            // Try index file
            for ext in &[".ts", ".tsx", ".js", ".jsx"] {
                let index = target.join(format!("index{}", ext));
                if self.module_index.values().any(|p| p == &index) {
                    return Some(index);
                }
            }
        }

        None
    }

    /// Resolve a Go import
    fn resolve_go_import(&self, import: &ImportInfo) -> Option<PathBuf> {
        // Go imports are typically package paths
        // For local packages, try to find in module index
        let module = &import.module;

        // Check if it's a local package
        for (name, path) in &self.module_index {
            if name.ends_with(module) || module.ends_with(name) {
                return Some(path.clone());
            }
        }

        None
    }

    /// Resolve a Rust use statement to a file path
    ///
    /// Handles:
    /// - `use crate::foo::bar` -> look for `src/foo/bar.rs` or `src/foo/bar/mod.rs`
    /// - `use super::sibling` -> relative to current file's parent module
    /// - `use self::child` -> relative to current module
    /// - `use std::collections::HashMap` -> external crate (returns None)
    fn resolve_rust_import(&self, import: &ImportInfo, from_file: &Path) -> Option<PathBuf> {
        let module = &import.module;

        // External crates (std, external dependencies) - cannot resolve to local files
        if !module.starts_with("crate")
            && !module.starts_with("super")
            && !module.starts_with("self")
        {
            // Check if it might be a local path-based import (rare)
            // For now, treat non-crate/super/self imports as external
            return None;
        }

        let from_dir = from_file.parent()?;

        // Handle `use crate::foo::bar`
        if let Some(rest) = module.strip_prefix("crate::") {
            return self.resolve_rust_crate_path(rest);
        }

        // Handle `use crate` (root module)
        if module == "crate" {
            // Try src/lib.rs or src/main.rs
            let lib_rs = self.root.join("src").join("lib.rs");
            if self.module_index.values().any(|p| p == &lib_rs) {
                return Some(lib_rs);
            }
            let main_rs = self.root.join("src").join("main.rs");
            if self.module_index.values().any(|p| p == &main_rs) {
                return Some(main_rs);
            }
            return None;
        }

        // Handle `use super::sibling`
        if let Some(rest) = module.strip_prefix("super::") {
            return self.resolve_rust_super_path(rest, from_dir);
        }

        // Handle `use super` (parent module)
        if module == "super" {
            let parent_dir = from_dir.parent()?;
            // Try parent/mod.rs
            let mod_rs = parent_dir.join("mod.rs");
            if self.module_index.values().any(|p| p == &mod_rs) {
                return Some(mod_rs);
            }
            return None;
        }

        // Handle `use self::child`
        if let Some(rest) = module.strip_prefix("self::") {
            return self.resolve_rust_self_path(rest, from_file);
        }

        // Handle `use self` (current module)
        if module == "self" {
            return Some(from_file.to_path_buf());
        }

        None
    }

    /// Resolve a crate-relative path like `foo::bar` to a file
    fn resolve_rust_crate_path(&self, path: &str) -> Option<PathBuf> {
        let parts: Vec<&str> = path.split("::").collect();
        if parts.is_empty() {
            return None;
        }

        // Build potential file paths
        let mut dir_path = self.root.join("src");
        for part in &parts {
            dir_path = dir_path.join(part);
        }

        // Try as file: src/foo/bar.rs
        let as_file = dir_path.with_extension("rs");
        if self.module_index.values().any(|p| p == &as_file) {
            return Some(as_file);
        }

        // Try as directory module: src/foo/bar/mod.rs
        let as_mod = dir_path.join("mod.rs");
        if self.module_index.values().any(|p| p == &as_mod) {
            return Some(as_mod);
        }

        // Try direct module index lookup
        let module_name = format!("crate::{}", path);
        self.module_index.get(&module_name).cloned()
    }

    /// Resolve a super-relative path
    ///
    /// In Rust, `super` refers to the parent MODULE, not the parent directory.
    /// - From `src/foo/bar.rs`, `super` is `crate::foo` (directory: `src/foo/`)
    /// - From `src/foo/bar/mod.rs`, `super` is `crate::foo` (directory: `src/foo/`)
    /// - From `src/foo/mod.rs`, `super` is `crate` (directory: `src/`)
    fn resolve_rust_super_path(&self, path: &str, from_dir: &Path) -> Option<PathBuf> {
        // from_dir is the directory containing the file
        // For super::, we stay in the same directory but look for siblings
        // (because the file's module is a child of that directory's module)

        let parts: Vec<&str> = path.split("::").collect();
        if parts.is_empty() {
            return None;
        }

        // Start from the same directory (parent module)
        let mut target = from_dir.to_path_buf();
        for part in &parts {
            target = target.join(part);
        }

        // Try as file
        let as_file = target.with_extension("rs");
        if self.module_index.values().any(|p| p == &as_file) {
            return Some(as_file);
        }

        // Try as directory module
        let as_mod = target.join("mod.rs");
        if self.module_index.values().any(|p| p == &as_mod) {
            return Some(as_mod);
        }

        None
    }

    /// Resolve a self-relative path (within same module directory)
    fn resolve_rust_self_path(&self, path: &str, from_file: &Path) -> Option<PathBuf> {
        let from_dir = from_file.parent()?;

        // Determine the module directory
        // If from_file is mod.rs or lib.rs, the module dir is the parent
        // If from_file is foo.rs, the module might have a sibling foo/ directory
        let file_stem = from_file.file_stem()?.to_str()?;
        let is_mod_file = file_stem == "mod" || file_stem == "lib" || file_stem == "main";

        let module_dir = if is_mod_file {
            from_dir.to_path_buf()
        } else {
            // foo.rs might have a sibling foo/ directory for submodules
            from_dir.join(file_stem)
        };

        let parts: Vec<&str> = path.split("::").collect();
        if parts.is_empty() {
            return None;
        }

        let mut target = module_dir;
        for part in &parts {
            target = target.join(part);
        }

        // Try as file
        let as_file = target.with_extension("rs");
        if self.module_index.values().any(|p| p == &as_file) {
            return Some(as_file);
        }

        // Try as directory module
        let as_mod = target.join("mod.rs");
        if self.module_index.values().any(|p| p == &as_mod) {
            return Some(as_mod);
        }

        // Also try sibling files in the same directory
        let sibling = from_dir.join(format!("{}.rs", parts[0]));
        if self.module_index.values().any(|p| p == &sibling) {
            return Some(sibling);
        }

        None
    }

    /// Resolve a Java import to a file path
    ///
    /// Handles:
    /// - `import com.example.Foo` -> look for `com/example/Foo.java`
    /// - `import com.example.*` -> wildcard (returns None, cannot resolve to single file)
    /// - `import static com.example.Foo.method` -> resolve to class file
    ///
    /// # Limitations
    /// - Does not search classpath or external JARs
    /// - Only resolves files within the project root
    fn resolve_java_import(&self, import: &ImportInfo, _from_file: &Path) -> Option<PathBuf> {
        let module = &import.module;

        // Wildcard imports cannot resolve to a single file
        if module.ends_with(".*") {
            return None;
        }

        // Check for JDK/external packages (common prefixes that won't be in project)
        // These are typically not in the source tree
        if module.starts_with("java.")
            || module.starts_with("javax.")
            || module.starts_with("sun.")
            || module.starts_with("com.sun.")
            || module.starts_with("org.w3c.")
            || module.starts_with("org.xml.")
        {
            return None;
        }

        // First, try direct module index lookup (fastest path)
        if let Some(path) = self.module_index.get(module) {
            return Some(path.clone());
        }

        // Convert package path to file path: com.example.Foo -> com/example/Foo.java
        let file_path = module.replace('.', "/") + ".java";

        // Try common source root patterns
        let source_roots = [
            "",               // Direct path
            "src/main/java/", // Maven/Gradle standard
            "src/test/java/", // Maven/Gradle test
            "src/",           // Simple project
        ];

        for prefix in &source_roots {
            let full_path = self.root.join(prefix).join(&file_path);
            if self.module_index.values().any(|p| p == &full_path) {
                return Some(full_path);
            }
        }

        None
    }

    /// Get all functions in a module
    pub fn get_module_functions(&self, module: &str) -> Vec<String> {
        self.function_index
            .iter()
            .filter_map(|(func, locations)| {
                if locations.iter().any(|(_, m)| m == module) {
                    Some(func.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get the module index
    pub fn modules(&self) -> &HashMap<String, PathBuf> {
        &self.module_index
    }

    /// Get the function index
    pub fn functions(&self) -> &HashMap<String, Vec<(PathBuf, String)>> {
        &self.function_index
    }
}
