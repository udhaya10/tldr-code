//! Variable packing infrastructure for the octagon domain.
//!
//! Variable packing limits the number of variables tracked in the DBM
//! to maintain performance. Variables beyond the pack limit fall back
//! to interval-only tracking.
//!
//! # Design
//!
//! ```text
//! OctagonState {
//!     pack: Pack         // Top N variables tracked relationally in DBM
//!     overflow: HashMap  // Remaining variables tracked as intervals
//! }
//! ```
//!
//! The default pack limit is 64 variables, chosen so that the half-matrix
//! fits in L1 cache (~66KB).
//!
//! # References
//!
//! - ASTREE: Variable packing with 10-50 variable packs
//! - Prior art Section 3.1: 32-64 variable sweet spot

use std::collections::HashMap;

use super::bound::Bound;
use super::dbm::Dbm;

/// Default maximum number of variables in a pack.
///
/// Chosen so the half-matrix fits in L1 cache:
/// 64 vars -> 128*129/2 = 8256 entries * 8 bytes = ~66KB.
pub const DEFAULT_PACK_LIMIT: usize = 64;

/// A variable pack: maps program variable names to DBM indices.
///
/// Variables within the pack are tracked relationally via the DBM.
/// Variables outside the pack are tracked independently via intervals.
#[derive(Debug, Clone)]
pub struct Pack<B: Bound> {
    /// The DBM for this pack.
    dbm: Dbm<B>,
    /// Map from variable name to pack-local variable index (0-based).
    var_to_idx: HashMap<String, usize>,
    /// Reverse map: pack-local index to variable name.
    idx_to_var: Vec<String>,
    /// Maximum number of variables this pack can hold.
    limit: usize,
}

impl<B: Bound> Pack<B> {
    /// Create a new empty pack with the given variable limit.
    pub fn new(limit: usize) -> Self {
        Pack {
            dbm: Dbm::new(0),
            var_to_idx: HashMap::new(),
            idx_to_var: Vec::new(),
            limit,
        }
    }

    /// Create a new pack with default limit.
    pub fn with_default_limit() -> Self {
        Self::new(DEFAULT_PACK_LIMIT)
    }

    /// Try to add a variable to the pack.
    ///
    /// Returns `Some(idx)` if the variable was added (or already exists),
    /// `None` if the pack is full.
    pub fn add_var(&mut self, name: &str) -> Option<usize> {
        if let Some(&idx) = self.var_to_idx.get(name) {
            return Some(idx);
        }
        if self.idx_to_var.len() >= self.limit {
            return None;
        }
        let idx = self.idx_to_var.len();
        self.var_to_idx.insert(name.to_string(), idx);
        self.idx_to_var.push(name.to_string());

        // Rebuild DBM with new size, copying existing constraints
        let new_n = self.idx_to_var.len();
        let old_dbm = std::mem::replace(&mut self.dbm, Dbm::new(new_n));
        let old_dim = old_dbm.dim();
        for i in 0..old_dim {
            for j in 0..old_dim {
                let val = old_dbm.get(i, j);
                self.dbm.set(i, j, val);
            }
        }

        Some(idx)
    }

    /// Get the pack-local index for a variable name.
    pub fn var_index(&self, name: &str) -> Option<usize> {
        self.var_to_idx.get(name).copied()
    }

    /// Get the variable name for a pack-local index.
    pub fn var_name(&self, idx: usize) -> Option<&str> {
        self.idx_to_var.get(idx).map(|s| s.as_str())
    }

    /// Number of variables currently in the pack.
    pub fn len(&self) -> usize {
        self.idx_to_var.len()
    }

    /// Whether the pack is empty.
    pub fn is_empty(&self) -> bool {
        self.idx_to_var.is_empty()
    }

    /// Whether the pack is full (at capacity).
    pub fn is_full(&self) -> bool {
        self.idx_to_var.len() >= self.limit
    }

    /// Get the pack limit.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Access the underlying DBM.
    pub fn dbm(&self) -> &Dbm<B> {
        &self.dbm
    }

    /// Access the underlying DBM mutably.
    pub fn dbm_mut(&mut self) -> &mut Dbm<B> {
        &mut self.dbm
    }

    /// Get all variable names in the pack, ordered by index.
    pub fn var_names(&self) -> &[String] {
        &self.idx_to_var
    }
}

/// Interval-only value for overflow variables.
///
/// Variables that don't fit in the pack are tracked with independent bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct IntervalValue {
    /// Lower bound (None = -infinity).
    pub low: Option<i64>,
    /// Upper bound (None = +infinity).
    pub high: Option<i64>,
}

impl IntervalValue {
    /// Top (unknown): no bounds.
    pub fn top() -> Self {
        IntervalValue {
            low: None,
            high: None,
        }
    }

    /// Exact value: [v, v].
    pub fn exact(v: i64) -> Self {
        IntervalValue {
            low: Some(v),
            high: Some(v),
        }
    }
}

/// Hybrid state: pack (relational) + overflow (interval-only).
#[derive(Debug, Clone)]
pub struct HybridState<B: Bound> {
    /// Relational pack for top-N variables.
    pack: Pack<B>,
    /// Overflow variables tracked as intervals.
    overflow: HashMap<String, IntervalValue>,
}

impl<B: Bound> Default for HybridState<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: Bound> HybridState<B> {
    /// Create a new hybrid state with default pack limit.
    pub fn new() -> Self {
        HybridState {
            pack: Pack::with_default_limit(),
            overflow: HashMap::new(),
        }
    }

    /// Create a new hybrid state with custom pack limit.
    pub fn with_limit(limit: usize) -> Self {
        HybridState {
            pack: Pack::new(limit),
            overflow: HashMap::new(),
        }
    }

    /// Access the pack.
    pub fn pack(&self) -> &Pack<B> {
        &self.pack
    }

    /// Access the pack mutably.
    pub fn pack_mut(&mut self) -> &mut Pack<B> {
        &mut self.pack
    }

    /// Access the overflow map (immutable).
    pub fn overflow(&self) -> &HashMap<String, IntervalValue> {
        &self.overflow
    }

    /// Access the overflow map mutably.
    pub fn overflow_mut(&mut self) -> &mut HashMap<String, IntervalValue> {
        &mut self.overflow
    }

    /// Get the interval for a variable, whether packed or overflow.
    ///
    /// For packed variables, projects the interval from the DBM.
    /// For overflow variables, returns the stored interval.
    /// For unknown variables, returns top (no information).
    pub fn get_interval(&self, name: &str) -> IntervalValue {
        if let Some(idx) = self.pack.var_index(name) {
            // Project from DBM using query::project_interval
            let (lo_f64, hi_f64) = super::query::project_interval(self.pack.dbm(), idx);
            IntervalValue {
                low: lo_f64.map(|v| v as i64),
                high: hi_f64.map(|v| v as i64),
            }
        } else if let Some(iv) = self.overflow.get(name) {
            iv.clone()
        } else {
            IntervalValue::top()
        }
    }
}

// =============================================================================
// Tests
// =============================================================================
