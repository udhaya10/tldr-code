//! Dataflow Analysis Foundation Types & CFG Helpers
//!
//! This module provides shared types and helper functions for dataflow analyses:
//!
//! - `BlockId`: Type alias matching existing CFG types (usize)
//! - `DataflowError`: Error types for dataflow analyses
//! - `build_predecessors`: Build predecessor map from CFG
//! - `find_back_edges`: Identify loop header blocks using dominance
//! - `reverse_postorder`: Compute efficient iteration order
//!
//! # Mitigations Addressed
//!
//! - TIGER-PASS1-8: Use usize for BlockId to match existing CFG types
//! - TIGER-PASS1-9: Centralize CFG helper functions
//! - TIGER-PASS3-4: Add MAX_BLOCKS constant for pathological CFG defense
//! - TIGER-PASS1-6: Implement find_back_edges using dominance

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::TldrError;
use crate::ssa::dominators::build_dominator_tree;
use crate::types::CfgInfo;

// =============================================================================
// Type Aliases
// =============================================================================

/// Block ID type alias matching existing CFG types.
///
/// Using `usize` to match CfgBlock.id and CfgEdge.from/to.
/// TIGER-PASS1-8: Use consistent types across CFG and dataflow modules.
pub type BlockId = usize;

// =============================================================================
// Constants
// =============================================================================

/// Maximum number of CFG blocks before analysis is refused.
///
/// TIGER-PASS3-4: Defense against pathological CFGs (e.g., generated code).
/// Chosen based on typical function sizes; functions with >10k blocks
/// are likely generated or should be split.
pub const MAX_BLOCKS: usize = 10_000;

/// Maximum fixpoint iterations before giving up.
///
/// This is a base limit that will be dynamically adjusted based on:
/// - Available Expressions: blocks * expressions * 2 + 10
/// - Abstract Interpretation: blocks * 10 + 100
///
/// The base limit provides a safety bound for edge cases.
pub const MAX_ITERATIONS: usize = 100;

// =============================================================================
// Error Types
// =============================================================================

/// Errors specific to dataflow analyses.
///
/// These errors are designed to be converted to TldrError for consistency
/// with the rest of the codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataflowError {
    /// CFG is required but not provided or empty
    NoCfg,

    /// DFG is required but not provided or empty
    NoDfg,

    /// CFG exceeds MAX_BLOCKS limit (TIGER-PASS3-4)
    TooManyBlocks {
        /// Number of blocks in the CFG
        count: usize,
    },

    /// Analysis did not converge within iteration limit
    IterationLimit {
        /// Number of iterations performed before giving up
        iterations: usize,
    },

    /// CFG pattern not supported (e.g., exception edges)
    UnsupportedCfgPattern {
        /// Description of the unsupported pattern
        pattern: String,
    },
}

impl std::fmt::Display for DataflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataflowError::NoCfg => write!(f, "CFG is required but not provided or empty"),
            DataflowError::NoDfg => write!(f, "DFG is required but not provided or empty"),
            DataflowError::TooManyBlocks { count } => {
                write!(
                    f,
                    "CFG has {} blocks, exceeds maximum of {} (TIGER-PASS3-4)",
                    count, MAX_BLOCKS
                )
            }
            DataflowError::IterationLimit { iterations } => {
                write!(
                    f,
                    "Dataflow analysis did not converge after {} iterations",
                    iterations
                )
            }
            DataflowError::UnsupportedCfgPattern { pattern } => {
                write!(f, "Unsupported CFG pattern: {}", pattern)
            }
        }
    }
}

impl std::error::Error for DataflowError {}

impl From<DataflowError> for TldrError {
    fn from(err: DataflowError) -> Self {
        TldrError::InvalidArgs {
            arg: "dataflow".to_string(),
            message: err.to_string(),
            suggestion: None,
        }
    }
}

// =============================================================================
// CFG Helper Functions
// =============================================================================

/// Build a predecessor map from a CFG.
///
/// For each block, returns the list of blocks that have edges pointing to it.
/// This is the inverse of the successor relationship in CFG edges.
///
/// # Arguments
///
/// * `cfg` - The control flow graph
///
/// # Returns
///
/// HashMap where keys are block IDs and values are vectors of predecessor block IDs.
///
/// # Example
///
/// ```rust,ignore
/// let preds = build_predecessors(&cfg);
/// // For block 2, get all blocks that can jump to it
/// let block_2_preds = preds.get(&2).unwrap_or(&vec![]);
/// ```
///
/// # TIGER Mitigation
///
/// - TIGER-PASS1-9: Centralized helper function
/// - TIGER-PASS1-8: Returns HashMap<usize, Vec<usize>> matching CFG types
pub fn build_predecessors(cfg: &CfgInfo) -> HashMap<BlockId, Vec<BlockId>> {
    let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::new();

    // Initialize all blocks with empty predecessor lists
    for block in &cfg.blocks {
        predecessors.entry(block.id).or_default();
    }

    // Populate from edges
    for edge in &cfg.edges {
        predecessors.entry(edge.to).or_default().push(edge.from);
    }

    predecessors
}

/// Build a successor map from a CFG.
///
/// For each block, returns the list of blocks that it has edges to.
/// This mirrors the edge structure in the CFG.
///
/// # Arguments
///
/// * `cfg` - The control flow graph
///
/// # Returns
///
/// HashMap where keys are block IDs and values are vectors of successor block IDs.
pub fn build_successors(cfg: &CfgInfo) -> HashMap<BlockId, Vec<BlockId>> {
    let mut successors: HashMap<BlockId, Vec<BlockId>> = HashMap::new();

    // Initialize all blocks with empty successor lists
    for block in &cfg.blocks {
        successors.entry(block.id).or_default();
    }

    // Populate from edges
    for edge in &cfg.edges {
        successors.entry(edge.from).or_default().push(edge.to);
    }

    successors
}

/// Find back edges and return the set of loop header block IDs.
///
/// A back edge is an edge from a block to one of its dominators in the CFG.
/// The target of a back edge is a loop header.
///
/// # Algorithm
///
/// 1. Build dominator tree using Lengauer-Tarjan algorithm
/// 2. For each edge (u -> v), check if v dominates u
/// 3. If so, (u -> v) is a back edge and v is a loop header
///
/// # Arguments
///
/// * `cfg` - The control flow graph
///
/// # Returns
///
/// HashSet of block IDs that are loop headers (targets of back edges).
///
/// # Errors
///
/// Returns empty set if dominator tree cannot be built (e.g., empty CFG).
///
/// # TIGER Mitigation
///
/// - TIGER-PASS1-5: Identifies loop headers for widening application
/// - TIGER-PASS1-6: Uses dominance-based back edge detection
///
/// # Example
///
/// ```rust,ignore
/// let loop_headers = find_back_edges(&cfg);
/// if loop_headers.contains(&block_id) {
///     // Apply widening at this block
///     state = widen_state(&old_state, &new_state);
/// }
/// ```
pub fn find_back_edges(cfg: &CfgInfo) -> HashSet<BlockId> {
    let mut loop_headers = HashSet::new();

    // Handle empty or trivial CFG
    if cfg.blocks.is_empty() {
        return loop_headers;
    }

    // Build dominator tree
    let dom_tree = match build_dominator_tree(cfg) {
        Ok(tree) => tree,
        Err(_) => return loop_headers, // Return empty set on error
    };

    // Check each edge for back edge property
    for edge in &cfg.edges {
        // An edge (u -> v) is a back edge if v dominates u
        if dom_tree.dominates(edge.to, edge.from) {
            loop_headers.insert(edge.to);
        }
    }

    loop_headers
}

/// Compute reverse postorder traversal of the CFG.
///
/// Reverse postorder is an efficient iteration order for forward dataflow
/// analysis because it ensures we process predecessors before successors
/// (except for back edges).
///
/// # Algorithm
///
/// 1. Perform DFS from entry, recording postorder (visit order when leaving)
/// 2. Reverse the postorder to get reverse postorder
///
/// # Arguments
///
/// * `cfg` - The control flow graph
///
/// # Returns
///
/// Vector of block IDs in reverse postorder.
///
/// # Properties
///
/// - Entry block is first (unless unreachable from entry)
/// - For acyclic CFGs, all predecessors appear before successors
/// - Improves convergence speed for dataflow analysis
///
/// # Example
///
/// ```rust,ignore
/// let order = reverse_postorder(&cfg);
/// for block_id in &order {
///     // Process blocks in efficient order
///     process_block(block_id, &state);
/// }
/// ```
pub fn reverse_postorder(cfg: &CfgInfo) -> Vec<BlockId> {
    if cfg.blocks.is_empty() {
        return Vec::new();
    }

    let successors = build_successors(cfg);
    let entry = cfg.entry_block;

    // DFS to compute postorder
    let mut postorder = Vec::new();
    let mut visited = HashSet::new();
    let mut stack = vec![(entry, false)];

    while let Some((block, finished)) = stack.pop() {
        if finished {
            postorder.push(block);
            continue;
        }

        if visited.contains(&block) {
            continue;
        }
        visited.insert(block);

        // Push marker for when we finish this node
        stack.push((block, true));

        // Push successors (in reverse for consistent ordering)
        if let Some(succs) = successors.get(&block) {
            for &succ in succs.iter().rev() {
                if !visited.contains(&succ) {
                    stack.push((succ, false));
                }
            }
        }
    }

    // Reverse to get reverse postorder
    postorder.reverse();
    postorder
}

/// Validate that a CFG is suitable for dataflow analysis.
///
/// Checks:
/// - CFG is not empty
/// - Number of blocks does not exceed MAX_BLOCKS
///
/// # Arguments
///
/// * `cfg` - The control flow graph to validate
///
/// # Returns
///
/// Ok(()) if valid, Err(DataflowError) otherwise.
///
/// # TIGER Mitigation
///
/// - TIGER-PASS3-4: Reject pathological CFGs early
pub fn validate_cfg(cfg: &CfgInfo) -> Result<(), DataflowError> {
    if cfg.blocks.is_empty() {
        return Err(DataflowError::NoCfg);
    }

    if cfg.blocks.len() > MAX_BLOCKS {
        return Err(DataflowError::TooManyBlocks {
            count: cfg.blocks.len(),
        });
    }

    Ok(())
}

/// Compute the reachable blocks from entry.
///
/// Returns the set of block IDs that are reachable from the entry block.
/// Useful for filtering out unreachable code in analysis results.
///
/// # Arguments
///
/// * `cfg` - The control flow graph
///
/// # Returns
///
/// HashSet of block IDs reachable from entry.
pub fn reachable_blocks(cfg: &CfgInfo) -> HashSet<BlockId> {
    if cfg.blocks.is_empty() {
        return HashSet::new();
    }

    let successors = build_successors(cfg);
    let entry = cfg.entry_block;

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(entry);

    while let Some(block) = queue.pop_front() {
        if visited.contains(&block) {
            continue;
        }
        visited.insert(block);

        if let Some(succs) = successors.get(&block) {
            for &succ in succs {
                if !visited.contains(&succ) {
                    queue.push_back(succ);
                }
            }
        }
    }

    visited
}

// =============================================================================
// Tests
// =============================================================================
