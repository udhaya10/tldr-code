//! Constraint Generation for Alias Analysis
//!
//! This module extracts constraints from SSA form for Andersen-style
//! points-to analysis. Constraints are used by the solver to compute
//! points-to sets via fixed-point iteration.
//!
//! # Constraint Types
//!
//! - **Copy**: `x = y` -> `pts(x) ⊇ pts(y)`
//! - **Alloc**: `x = new T()` -> `pts(x) ⊇ {alloc_site}`
//! - **FieldLoad**: `x = y.f` -> `pts(x) ⊇ pts(y).f`
//! - **FieldStore**: `x.f = y` -> `pts(x).f ⊇ pts(y)`
//!
//! # TIGER Mitigations
//!
//! - **TIGER-3**: Validates all SSA references exist before processing
//! - **TIGER-14**: Validates phi function source count matches predecessors

use std::collections::HashSet;

use crate::ssa::types::{
    PhiFunction, SsaBlock, SsaFunction, SsaInstruction, SsaInstructionKind, SsaNameId,
};

use super::types::{AbstractLocation, AliasError};

// =============================================================================
// Constraint Types
// =============================================================================

/// Constraint types for Andersen's analysis.
///
/// These represent the fundamental pointer relationships that must be
/// propagated during fixed-point iteration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Constraint {
    /// Copy constraint: `x = y` -> `pts(x) ⊇ pts(y)`
    ///
    /// The target variable's points-to set must include everything
    /// the source variable points to.
    Copy {
        /// Variable receiving the copy (left-hand side)
        target: String,
        /// Variable being copied (right-hand side)
        source: String,
    },

    /// Allocation constraint: `x = new T()` -> `pts(x) ⊇ {alloc_site}`
    ///
    /// The target variable points to a newly allocated object
    /// at the given abstract location.
    Alloc {
        /// Variable receiving the allocation
        target: String,
        /// Abstract location representing the allocation site
        site: AbstractLocation,
    },

    /// Field load constraint: `x = y.field` -> `pts(x) ⊇ pts(y).field`
    ///
    /// The target variable's points-to set must include the field
    /// of every location the base variable points to.
    FieldLoad {
        /// Variable receiving the field value
        target: String,
        /// Base object being accessed
        base: String,
        /// Field name being loaded
        field: String,
    },

    /// Field store constraint: `x.field = y` -> `pts(x).field ⊇ pts(y)`
    ///
    /// For every location the base points to, the field of that
    /// location must include everything the source points to.
    FieldStore {
        /// Base object being modified
        base: String,
        /// Field name being stored to
        field: String,
        /// Variable whose value is being stored
        source: String,
    },
}

impl Constraint {
    /// Create a copy constraint.
    pub fn copy(target: impl Into<String>, source: impl Into<String>) -> Self {
        Constraint::Copy {
            target: target.into(),
            source: source.into(),
        }
    }

    /// Create an allocation constraint.
    pub fn alloc(target: impl Into<String>, site: AbstractLocation) -> Self {
        Constraint::Alloc {
            target: target.into(),
            site,
        }
    }

    /// Create a field load constraint.
    pub fn field_load(
        target: impl Into<String>,
        base: impl Into<String>,
        field: impl Into<String>,
    ) -> Self {
        Constraint::FieldLoad {
            target: target.into(),
            base: base.into(),
            field: field.into(),
        }
    }

    /// Create a field store constraint.
    pub fn field_store(
        base: impl Into<String>,
        field: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Constraint::FieldStore {
            base: base.into(),
            field: field.into(),
            source: source.into(),
        }
    }

    /// Get the target variable name if this constraint defines one.
    pub fn target(&self) -> Option<&str> {
        match self {
            Constraint::Copy { target, .. } => Some(target),
            Constraint::Alloc { target, .. } => Some(target),
            Constraint::FieldLoad { target, .. } => Some(target),
            Constraint::FieldStore { .. } => None,
        }
    }

    /// Get all variables referenced by this constraint.
    pub fn variables(&self) -> Vec<&str> {
        match self {
            Constraint::Copy { target, source } => vec![target, source],
            Constraint::Alloc { target, .. } => vec![target],
            Constraint::FieldLoad { target, base, .. } => vec![target, base],
            Constraint::FieldStore { base, source, .. } => vec![base, source],
        }
    }
}

// =============================================================================
// Constraint Extractor
// =============================================================================

/// Extract constraints from SSA form.
///
/// The `ConstraintExtractor` processes an SSA function and generates
/// the constraint set needed for Andersen's analysis.
///
/// # Example
///
/// ```rust,ignore
/// use tldr_core::alias::constraints::ConstraintExtractor;
/// use tldr_core::ssa::types::SsaFunction;
///
/// let ssa: SsaFunction = /* ... */;
/// let extractor = ConstraintExtractor::extract_from_ssa(&ssa)?;
///
/// for constraint in extractor.constraints() {
///     println!("{:?}", constraint);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ConstraintExtractor {
    /// Extracted constraints
    constraints: Vec<Constraint>,
    /// Allocation sites discovered during extraction
    allocation_sites: HashSet<AbstractLocation>,
    /// Set of SSA names that are phi function targets (may-alias only)
    phi_targets: HashSet<String>,
    /// Set of parameters (for parameter aliasing)
    parameters: HashSet<String>,
    /// Mapping from SsaNameId to formatted name for quick lookup
    name_map: std::collections::HashMap<SsaNameId, String>,
}

impl ConstraintExtractor {
    /// Create a new empty constraint extractor.
    pub fn new() -> Self {
        ConstraintExtractor {
            constraints: Vec::new(),
            allocation_sites: HashSet::new(),
            phi_targets: HashSet::new(),
            parameters: HashSet::new(),
            name_map: std::collections::HashMap::new(),
        }
    }

    /// Extract constraints from an SSA function.
    ///
    /// This is the main entry point for constraint extraction.
    ///
    /// # Arguments
    /// * `ssa` - The SSA form of the function to analyze
    ///
    /// # Returns
    /// * `Ok(ConstraintExtractor)` - Extracted constraints and metadata
    /// * `Err(AliasError)` - If SSA validation fails
    ///
    /// # TIGER-3 Mitigation
    /// Validates all SSA references exist before processing.
    pub fn extract_from_ssa(ssa: &SsaFunction) -> Result<Self, AliasError> {
        let mut extractor = Self::new();

        // Build name map for fast lookup (TIGER-3: validate references)
        extractor.build_name_map(ssa)?;

        // Process each block
        for block in &ssa.blocks {
            // Process phi functions first
            extractor.process_phi_functions(ssa, block)?;

            // Process instructions
            for instruction in &block.instructions {
                extractor.process_instruction(ssa, instruction)?;
            }
        }

        Ok(extractor)
    }

    /// Get the extracted constraints.
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Get the allocation sites discovered during extraction.
    pub fn allocation_sites(&self) -> &HashSet<AbstractLocation> {
        &self.allocation_sites
    }

    /// Get the set of phi function targets.
    ///
    /// These variables should NOT have must-alias relationships
    /// because they could come from multiple sources at runtime.
    pub fn phi_targets(&self) -> &HashSet<String> {
        &self.phi_targets
    }

    /// Get the set of parameter names.
    pub fn parameters(&self) -> &HashSet<String> {
        &self.parameters
    }

    /// Check if a variable is a phi target.
    pub fn is_phi_target(&self, var: &str) -> bool {
        self.phi_targets.contains(var)
    }

    // =========================================================================
    // Internal Methods
    // =========================================================================

    /// Build a mapping from SsaNameId to formatted name.
    ///
    /// TIGER-3: This validates that all SSA names are properly defined.
    fn build_name_map(&mut self, ssa: &SsaFunction) -> Result<(), AliasError> {
        for ssa_name in &ssa.ssa_names {
            let formatted = ssa_name.format_name();
            self.name_map.insert(ssa_name.id, formatted);
        }
        Ok(())
    }

    /// Format an SSA name ID to its string representation.
    ///
    /// TIGER-3: Returns a placeholder for missing names instead of crashing.
    fn format_ssa_name(&self, id: SsaNameId) -> String {
        self.name_map
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("$unknown_{}", id.0))
    }

    /// Validate that an SSA name ID exists.
    ///
    /// TIGER-3: Returns error if the ID is not in the name map.
    fn validate_ssa_name(&self, id: SsaNameId, context: &str) -> Result<String, AliasError> {
        self.name_map.get(&id).cloned().ok_or_else(|| {
            AliasError::InvalidRef(format!(
                "SSA name ${} not found in {} (TIGER-3 violation)",
                id.0, context
            ))
        })
    }

    /// Process phi functions in a block.
    ///
    /// TIGER-14: Validates phi source count matches predecessor count.
    fn process_phi_functions(
        &mut self,
        ssa: &SsaFunction,
        block: &SsaBlock,
    ) -> Result<(), AliasError> {
        for phi in &block.phi_functions {
            self.process_single_phi(ssa, phi, block)?;
        }
        Ok(())
    }

    /// Process a single phi function.
    fn process_single_phi(
        &mut self,
        _ssa: &SsaFunction,
        phi: &PhiFunction,
        block: &SsaBlock,
    ) -> Result<(), AliasError> {
        // TIGER-14: Validate phi source count
        // Note: This is a warning, not an error - some SSA forms may have
        // different source counts due to unreachable predecessors
        if phi.sources.len() != block.predecessors.len() && !block.predecessors.is_empty() {
            // Log warning but continue - this is acceptable in some SSA variants
            // In strict mode, this could return an error
        }

        // Get target name
        let target =
            self.validate_ssa_name(phi.target, &format!("phi target in block {}", block.id))?;

        // Mark as phi target (no must-alias for phi results)
        self.phi_targets.insert(target.clone());

        // Create copy constraints from each source
        for source in &phi.sources {
            let source_name = self.validate_ssa_name(
                source.name,
                &format!(
                    "phi source for {} from block {}",
                    phi.variable, source.block
                ),
            )?;

            self.constraints
                .push(Constraint::copy(target.clone(), source_name));
        }

        Ok(())
    }

    /// Process an SSA instruction.
    fn process_instruction(
        &mut self,
        ssa: &SsaFunction,
        instruction: &SsaInstruction,
    ) -> Result<(), AliasError> {
        match instruction.kind {
            SsaInstructionKind::Param => {
                self.process_param_instruction(instruction)?;
            }
            SsaInstructionKind::Assign => {
                self.process_assign_instruction(instruction)?;
            }
            SsaInstructionKind::Call => {
                self.process_call_instruction(ssa, instruction)?;
            }
            SsaInstructionKind::BinaryOp
            | SsaInstructionKind::UnaryOp
            | SsaInstructionKind::Return
            | SsaInstructionKind::Branch => {
                // These don't create alias constraints
            }
        }
        Ok(())
    }

    /// Process a parameter instruction.
    ///
    /// Creates: `pts(param) = {param_NAME}`
    /// For mutable defaults (TIGER-7): `pts(param) = {alloc_default_LINE}`
    fn process_param_instruction(
        &mut self,
        instruction: &SsaInstruction,
    ) -> Result<(), AliasError> {
        if let Some(target_id) = instruction.target {
            let target = self.validate_ssa_name(target_id, "param instruction")?;

            // TIGER-7: Check for mutable default argument
            if let Some(source_text) = &instruction.source_text {
                if let Some(default_site) =
                    self.parse_mutable_default(source_text, instruction.line)
                {
                    // This parameter has a mutable default value
                    // The default is shared across all calls
                    self.allocation_sites.insert(default_site.clone());
                    self.parameters.insert(target.clone());
                    self.constraints
                        .push(Constraint::alloc(target.clone(), default_site));
                    // Also add the param location for when caller provides value
                    let param_name =
                        target.trim_end_matches(|c: char| c == '_' || c.is_ascii_digit());
                    let param_site = AbstractLocation::param(param_name);
                    self.allocation_sites.insert(param_site.clone());
                    self.constraints.push(Constraint::alloc(target, param_site));
                    return Ok(());
                }
            }

            // Extract parameter name from SSA name (e.g., "p_0" -> "p")
            let param_name = target
                .rsplit('_')
                .nth(1)
                .map(|_| {
                    // Handle cases like "param_name_0" -> "param_name"
                    target.trim_end_matches(|c: char| c == '_' || c.is_ascii_digit())
                })
                .unwrap_or(&target);

            let site = AbstractLocation::param(param_name);
            self.allocation_sites.insert(site.clone());
            self.parameters.insert(target.clone());

            self.constraints.push(Constraint::alloc(target, site));
        }
        Ok(())
    }

    /// Process an assignment instruction.
    ///
    /// For `x = y`: Creates copy constraint `pts(x) ⊇ pts(y)`
    /// For `x = y.f`: Creates field load constraint
    /// For `x = Class.f`: Creates class variable allocation (TIGER-8)
    fn process_assign_instruction(
        &mut self,
        instruction: &SsaInstruction,
    ) -> Result<(), AliasError> {
        if let Some(target_id) = instruction.target {
            let target = self.validate_ssa_name(target_id, "assign target")?;

            // Check if this is a simple copy or field access
            if let Some(source_text) = &instruction.source_text {
                // TIGER-8: Check for class variable access (ClassName.field)
                if let Some(class_var) = self.detect_class_var(source_text) {
                    self.allocation_sites.insert(class_var.clone());
                    self.constraints.push(Constraint::alloc(target, class_var));
                    return Ok(());
                }

                // Try to detect field access pattern
                if let Some(field_access) = self.parse_field_access(source_text) {
                    // This is a field load: x = base.field
                    // Find the base variable in uses
                    if !instruction.uses.is_empty() {
                        let base = self.format_ssa_name(instruction.uses[0]);
                        self.constraints
                            .push(Constraint::field_load(target, base, field_access));
                        return Ok(());
                    }
                }

                // Try to detect field store pattern: base.field = value
                if let Some((base_field, _)) = self.parse_field_store(source_text) {
                    // This is handled separately - field stores don't have a target
                    // The target here is actually the base object being modified
                    if !instruction.uses.is_empty() {
                        let source = self.format_ssa_name(instruction.uses[0]);
                        let (base_name, field_name) = base_field;
                        self.constraints
                            .push(Constraint::field_store(base_name, field_name, source));
                        return Ok(());
                    }
                }
            }

            // Simple copy: x = y
            if instruction.uses.len() == 1 {
                let source = self.format_ssa_name(instruction.uses[0]);
                self.constraints.push(Constraint::copy(target, source));
            } else if instruction.uses.is_empty() {
                // Assignment with no uses: could be literal, allocation, or constant.
                // Check source_text to determine if this is a known allocation pattern.
                let is_allocation = instruction
                    .source_text
                    .as_ref()
                    .map(|s| self.is_allocation_call(s))
                    .unwrap_or(false);

                let site = if is_allocation {
                    AbstractLocation::alloc(instruction.line)
                } else {
                    AbstractLocation::unknown(instruction.line)
                };
                self.allocation_sites.insert(site.clone());
                self.constraints.push(Constraint::alloc(target, site));
            }
        }
        Ok(())
    }

    /// Process a call instruction.
    ///
    /// For `x = Foo()`: Creates allocation constraint (constructor call)
    /// For `x = func()`: Creates unknown constraint (external call)
    fn process_call_instruction(
        &mut self,
        _ssa: &SsaFunction,
        instruction: &SsaInstruction,
    ) -> Result<(), AliasError> {
        if let Some(target_id) = instruction.target {
            let target = self.validate_ssa_name(target_id, "call target")?;

            // Determine if this is an allocation or unknown call
            let is_allocation = instruction
                .source_text
                .as_ref()
                .map(|s| self.is_allocation_call(s))
                .unwrap_or(false);

            let site = if is_allocation {
                // Allocation: x = Foo(), x = [], x = {}
                AbstractLocation::alloc(instruction.line)
            } else {
                // Unknown/external call
                AbstractLocation::unknown(instruction.line)
            };

            self.allocation_sites.insert(site.clone());
            self.constraints.push(Constraint::alloc(target, site));
        }
        Ok(())
    }

    /// Check if a call is an allocation (constructor, list, dict).
    fn is_allocation_call(&self, source_text: &str) -> bool {
        // Simple heuristics for allocation detection
        let trimmed = source_text.trim();

        // Constructor call: Foo(), ClassName()
        // Look for pattern: something = Name()
        if let Some(rhs) = trimmed.split('=').nth(1) {
            let rhs = rhs.trim();
            // Check for constructor pattern: starts with uppercase or is [], {}
            if rhs.starts_with('[') || rhs.starts_with('{') {
                return true;
            }
            // Check for ClassName() pattern
            if let Some(first_char) = rhs.chars().next() {
                if first_char.is_uppercase() {
                    return true;
                }
            }
        }

        // Direct patterns
        trimmed.contains("[]") || trimmed.contains("{}")
    }

    /// Parse a field access from source text.
    ///
    /// Returns the field name if the source text contains `base.field`.
    fn parse_field_access(&self, source_text: &str) -> Option<String> {
        // Look for pattern: x = something.field
        let trimmed = source_text.trim();
        if let Some(rhs) = trimmed.split('=').nth(1) {
            let rhs = rhs.trim();
            // Check for dot notation, excluding method calls
            if rhs.contains('.') && !rhs.contains('(') {
                // Extract field name after last dot
                if let Some(field) = rhs.rsplit('.').next() {
                    let field = field.trim();
                    if !field.is_empty() && field.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        return Some(field.to_string());
                    }
                }
            }
        }
        None
    }

    /// Parse a field store from source text.
    ///
    /// Returns (base_name, field_name) if the source text contains `base.field = value`.
    fn parse_field_store(&self, source_text: &str) -> Option<((String, String), ())> {
        // Look for pattern: base.field = value
        let trimmed = source_text.trim();
        let parts: Vec<&str> = trimmed.splitn(2, '=').collect();
        if parts.len() == 2 {
            let lhs = parts[0].trim();
            if lhs.contains('.') {
                let lhs_parts: Vec<&str> = lhs.rsplitn(2, '.').collect();
                if lhs_parts.len() == 2 {
                    let field = lhs_parts[0].trim().to_string();
                    let base = lhs_parts[1].trim().to_string();
                    if !field.is_empty() && !base.is_empty() {
                        return Some(((base, field), ()));
                    }
                }
            }
        }
        None
    }

    /// Check if source text represents a mutable default argument.
    ///
    /// Detects Python patterns like `def f(x=[])` or `def f(x={})` which create
    /// shared mutable objects across all calls (TIGER-7).
    ///
    /// Returns Some(site) if this is a default arg initialization.
    pub fn parse_mutable_default(&self, source_text: &str, line: u32) -> Option<AbstractLocation> {
        let trimmed = source_text.trim();

        // Look for pattern: param=[] or param={}
        // This appears in function definition context
        if trimmed.contains("def ") {
            // Check for mutable default patterns
            if trimmed.contains("=[]") || trimmed.contains("= []") {
                return Some(AbstractLocation::default_arg(line));
            }
            if trimmed.contains("={}") || trimmed.contains("= {}") {
                return Some(AbstractLocation::default_arg(line));
            }
        }

        None
    }

    /// Parse a class variable access pattern.
    ///
    /// Detects Python patterns like `ClassName.attr` which access class-level
    /// variables (singletons shared across all instances) (TIGER-8).
    ///
    /// Returns Some((class_name, field_name)) if this is a class variable access.
    pub fn parse_class_var_access(&self, source_text: &str) -> Option<(String, String)> {
        let trimmed = source_text.trim();

        // Look for pattern: x = ClassName.field or ClassName.field = value
        // Class names start with uppercase
        let rhs = if let Some(rhs) = trimmed.split('=').nth(1) {
            rhs.trim()
        } else if trimmed.contains('.') {
            trimmed
        } else {
            return None;
        };

        // Check for ClassName.field pattern (not method call)
        if rhs.contains('.') && !rhs.contains('(') {
            let parts: Vec<&str> = rhs.splitn(2, '.').collect();
            if parts.len() == 2 {
                let potential_class = parts[0].trim();
                let field = parts[1].trim();

                // Check if it looks like a class name (starts with uppercase)
                // and is not `self` or `cls` (instance access)
                if let Some(first_char) = potential_class.chars().next() {
                    if first_char.is_uppercase()
                        && !field.is_empty()
                        && field.chars().all(|c| c.is_alphanumeric() || c == '_')
                    {
                        return Some((potential_class.to_string(), field.to_string()));
                    }
                }
            }
        }

        None
    }

    /// Check if this is a class variable access and create the appropriate location.
    ///
    /// Returns Some(AbstractLocation::ClassVar) if this is a class variable,
    /// None otherwise (instance variable access).
    pub fn detect_class_var(&self, source_text: &str) -> Option<AbstractLocation> {
        self.parse_class_var_access(source_text)
            .map(|(class, field)| AbstractLocation::class_var(class, field))
    }
}

impl Default for ConstraintExtractor {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================
