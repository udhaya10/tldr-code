//! Octagon Abstract Domain (Mine 2006)
//!
//! This module implements the octagon abstract domain, which tracks
//! relational constraints of the form `+-xi +- xj <= c` using a
//! Difference Bound Matrix (DBM) representation.
//!
//! # Architecture
//!
//! ```text
//! octagon/
//! +-- mod.rs        -- Public API (this file)
//! +-- bound.rs      -- Bound trait + f64/i64 implementations
//! +-- dbm.rs        -- Half-matrix DBM storage, indexing
//! +-- closure.rs    -- Strong closure, incremental closure, tight closure
//! +-- operations.rs -- Join, meet, widening, narrowing
//! +-- transfer.rs   -- Assignment, guard/test, forget
//! +-- pack.rs       -- Variable packing infrastructure
//! +-- query.rs      -- Inclusion, equality, constraint extraction
//! ```
//!
//! # Usage
//!
//! The octagon domain provides the same public API as the interval domain
//! (`AbstractState::get()`, `AbstractState::set()`, etc.) but internally
//! uses a DBM for relational precision.
//!
//! # References
//!
//! - Mine, A. (2006). "The Octagon Abstract Domain."
//!   Higher-Order and Symbolic Computation, 19(1), 31-100.
//! - Bagnara, R., Hill, P.M., Zaffanella, E. (2018).
//!   "Incrementally Closing Octagons."
//!   Formal Methods in System Design, 51(2), 342-363.

pub mod bound;
pub mod closure;
pub mod dbm;
pub mod operations;
pub mod pack;
pub mod query;
pub mod transfer;

// Property-based tests (proptest)
// Re-exports for convenience
pub use bound::Bound;
pub use closure::{strong_closure, incremental_closure, tight_closure, ClosureResult};
pub use dbm::Dbm;
pub use operations::{join, meet, widen, widen_with_thresholds, is_included};
pub use pack::{Pack, HybridState, IntervalValue, DEFAULT_PACK_LIMIT};
pub use query::{project_interval, is_bottom, is_top, extract_constraints};
pub use transfer::{assign, guard, forget, OctExpr, OctGuard};

/// Octagon state: wraps a DBM with variable name mapping.
///
/// This is the main entry point for the octagon domain. It provides
/// the same API as the existing `AbstractState` but with relational
/// precision internally.
#[derive(Debug, Clone)]
pub struct OctagonState {
    /// The hybrid state (pack + overflow intervals).
    state: HybridState<f64>,
}

impl OctagonState {
    /// Create a new empty octagon state.
    pub fn new() -> Self {
        OctagonState {
            state: HybridState::new(),
        }
    }

    /// Get the abstract value (interval projection) for a variable.
    ///
    /// For packed variables, projects from the DBM.
    /// For overflow variables, returns the stored interval.
    /// For unknown variables, returns top (no information).
    pub fn get(&self, var: &str) -> IntervalValue {
        self.state.get_interval(var)
    }

    /// Set the value of a variable.
    ///
    /// If the variable is in the pack, updates the DBM.
    /// If the variable is in overflow, updates the interval.
    /// If the variable is new and the pack is not full, adds to pack.
    /// If the variable is new and the pack is full, adds to overflow.
    pub fn set(&mut self, var: &str, value: IntervalValue) {
        // Try to add the variable to the pack (returns existing index if already present)
        if let Some(idx) = self.state.pack_mut().add_var(var) {
            // Variable is in the pack: set bounds in the DBM
            let pos = 2 * idx;
            let neg = pos + 1;
            let dbm = self.state.pack_mut().dbm_mut();

            // First forget old constraints on this variable
            forget(dbm, idx);

            // Set upper bound: x <= hi => m[2v+1, 2v] = 2*hi
            if let Some(hi) = value.high {
                let two_hi = 2.0 * hi as f64;
                dbm.set(neg, pos, two_hi);
            }
            // Set lower bound: x >= lo => m[2v, 2v+1] = -2*lo
            if let Some(lo) = value.low {
                let neg_two_lo = -2.0 * lo as f64;
                dbm.set(pos, neg, neg_two_lo);
            }
        } else {
            // Pack is full: store in overflow intervals
            self.state.overflow_mut().insert(var.to_string(), value);
        }
    }

    /// Copy the state (explicit clone).
    pub fn copy(&self) -> Self {
        self.clone()
    }

    /// Get all variable names tracked in this octagon state.
    ///
    /// Returns packed variable names followed by overflow variable names.
    /// The order within each group is deterministic: packed variables are
    /// ordered by pack index; overflow variables are in arbitrary order.
    pub fn var_names(&self) -> Vec<String> {
        let pack_names = self.state.pack().var_names();
        let overflow_names = self.state.overflow().keys();
        let mut names: Vec<String> = pack_names.to_vec();
        for name in overflow_names {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// Check if a variable may be zero.
    ///
    /// Conservative: returns `true` if unsure.
    pub fn may_be_zero(&self, var: &str) -> bool {
        let iv = self.get(var);
        // If the lower bound is known and strictly positive, zero is excluded.
        if let Some(lo) = iv.low {
            if lo > 0 {
                return false;
            }
        }
        // If the upper bound is known and strictly negative, zero is excluded.
        if let Some(hi) = iv.high {
            if hi < 0 {
                return false;
            }
        }
        // Both bounds known and zero is outside the range.
        if let (Some(lo), Some(hi)) = (iv.low, iv.high) {
            return lo <= 0 && hi >= 0;
        }
        // At least one bound is unknown and the known bound (if any) doesn't
        // exclude zero -- conservatively assume zero is possible.
        true
    }

    /// Check if a variable may be null.
    ///
    /// Conservative: returns `true` if unsure.
    /// (Nullability is tracked separately, not in the DBM.)
    pub fn may_be_null(&self, _var: &str) -> bool {
        // Conservative: always true until we have nullability tracking
        true
    }

    /// Access the inner `HybridState` (immutable).
    ///
    /// Used for direct DBM operations such as join/widen on raw DBMs.
    pub fn hybrid_state(&self) -> &HybridState<f64> {
        &self.state
    }

    /// Access the inner `HybridState` (mutable).
    ///
    /// Used for in-place DBM modifications during transfer functions.
    pub fn hybrid_state_mut(&mut self) -> &mut HybridState<f64> {
        &mut self.state
    }

    /// Join two octagon states (least upper bound).
    ///
    /// If both states have packs with the same variables (same size and names),
    /// uses `operations::join()` on the DBMs directly for relational precision.
    /// Otherwise, falls back to interval-based join: projects both states to
    /// intervals, takes the wider range for each variable, and builds a new
    /// `OctagonState` from the result.
    ///
    /// Overflow intervals are joined independently (wider range wins).
    pub fn join(&self, other: &OctagonState) -> OctagonState {
        let self_pack = self.state.pack();
        let other_pack = other.state.pack();

        // Check if packs are compatible (same variables in same order)
        let packs_compatible = self_pack.len() == other_pack.len()
            && self_pack.var_names() == other_pack.var_names();

        if packs_compatible && !self_pack.is_empty() {
            // Use relational join on DBMs
            if let Some(joined_dbm) = operations::join(self_pack.dbm(), other_pack.dbm()) {
                let mut result = OctagonState::new();
                // Rebuild pack with same variables
                for name in self_pack.var_names() {
                    result.state.pack_mut().add_var(name);
                }
                // Copy joined DBM entries
                let dim = joined_dbm.dim();
                for i in 0..dim {
                    for j in 0..dim {
                        result.state.pack_mut().dbm_mut().set(i, j, joined_dbm.get(i, j));
                    }
                }
                // Join overflow intervals
                let all_overflow_keys: std::collections::HashSet<&String> = self
                    .state
                    .overflow()
                    .keys()
                    .chain(other.state.overflow().keys())
                    .collect();
                for key in all_overflow_keys {
                    let iv_self = self.state.overflow().get(key).cloned().unwrap_or_else(IntervalValue::top);
                    let iv_other = other.state.overflow().get(key).cloned().unwrap_or_else(IntervalValue::top);
                    let joined_iv = join_intervals(&iv_self, &iv_other);
                    result.state.overflow_mut().insert(key.clone(), joined_iv);
                }
                return result;
            }
        }

        // Fallback: interval-based join
        self.join_by_intervals(other)
    }

    /// Fallback join via interval projection.
    ///
    /// Projects both states to intervals, takes the wider range per variable,
    /// and constructs a fresh `OctagonState`.  Loses relational information
    /// but is always correct (sound over-approximation).
    fn join_by_intervals(&self, other: &OctagonState) -> OctagonState {
        let mut result = OctagonState::new();
        let mut all_vars: std::collections::HashSet<String> = std::collections::HashSet::new();
        for name in self.var_names() {
            all_vars.insert(name);
        }
        for name in other.var_names() {
            all_vars.insert(name);
        }
        for name in &all_vars {
            let iv_self = self.get(name);
            let iv_other = other.get(name);
            let joined = join_intervals(&iv_self, &iv_other);
            result.set(name, joined);
        }
        result
    }

    /// Widen two octagon states (old, new) for fixpoint termination.
    ///
    /// If both states have compatible packs, uses `operations::widen()` on the
    /// DBMs directly.  Otherwise, falls back to interval-based widening.
    ///
    /// Constraints that grew between `old` and `new` are set to +infinity,
    /// guaranteeing termination of the fixpoint iteration in finite steps.
    pub fn widen(old: &OctagonState, new: &OctagonState) -> OctagonState {
        let old_pack = old.state.pack();
        let new_pack = new.state.pack();

        let packs_compatible = old_pack.len() == new_pack.len()
            && old_pack.var_names() == new_pack.var_names();

        if packs_compatible && !old_pack.is_empty() {
            let widened_dbm = operations::widen(old_pack.dbm(), new_pack.dbm());
            let mut result = OctagonState::new();
            // Rebuild pack with same variables
            for name in old_pack.var_names() {
                result.state.pack_mut().add_var(name);
            }
            // Copy widened DBM entries
            let dim = widened_dbm.dim();
            for i in 0..dim {
                for j in 0..dim {
                    result.state.pack_mut().dbm_mut().set(i, j, widened_dbm.get(i, j));
                }
            }
            // Widen overflow intervals
            let all_overflow_keys: std::collections::HashSet<&String> = old
                .state
                .overflow()
                .keys()
                .chain(new.state.overflow().keys())
                .collect();
            for key in all_overflow_keys {
                let iv_old = old.state.overflow().get(key).cloned().unwrap_or_else(IntervalValue::top);
                let iv_new = new.state.overflow().get(key).cloned().unwrap_or_else(IntervalValue::top);
                let widened_iv = widen_intervals(&iv_old, &iv_new);
                result.state.overflow_mut().insert(key.clone(), widened_iv);
            }
            return result;
        }

        // Fallback: interval-based widening
        let mut result = OctagonState::new();
        let mut all_vars: std::collections::HashSet<String> = std::collections::HashSet::new();
        for name in old.var_names() {
            all_vars.insert(name);
        }
        for name in new.var_names() {
            all_vars.insert(name);
        }
        for name in &all_vars {
            let iv_old = old.get(name);
            let iv_new = new.get(name);
            let widened = widen_intervals(&iv_old, &iv_new);
            result.set(name, widened);
        }
        result
    }
}

/// Join two interval values (element-wise wider range).
///
/// For each bound, takes the less restrictive option:
/// - Lower bound: min of the two (or None if either is unbounded below)
/// - Upper bound: max of the two (or None if either is unbounded above)
fn join_intervals(a: &IntervalValue, b: &IntervalValue) -> IntervalValue {
    let low = match (a.low, b.low) {
        (Some(la), Some(lb)) => Some(Ord::min(la, lb)),
        _ => None, // Either unbounded -> result unbounded
    };
    let high = match (a.high, b.high) {
        (Some(ha), Some(hb)) => Some(Ord::max(ha, hb)),
        _ => None,
    };
    IntervalValue { low, high }
}

/// Widen two interval values for fixpoint termination.
///
/// If a bound grew (became less restrictive), set it to unbounded (+/-infinity).
/// If a bound is stable or tightened, keep the new value.
fn widen_intervals(old: &IntervalValue, new: &IntervalValue) -> IntervalValue {
    let low = match (old.low, new.low) {
        (Some(lo), Some(ln)) => {
            if ln < lo {
                None // Grew downward -> -infinity
            } else {
                Some(ln)
            }
        }
        (Some(_), None) => None, // Already grew to -infinity
        (None, new_lo) => new_lo, // Old was -inf, keep new
    };
    let high = match (old.high, new.high) {
        (Some(ho), Some(hn)) => {
            if hn > ho {
                None // Grew upward -> +infinity
            } else {
                Some(hn)
            }
        }
        (Some(_), None) => None, // Already grew to +infinity
        (None, new_hi) => new_hi, // Old was +inf, keep new
    };
    IntervalValue { low, high }
}

impl Default for OctagonState {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Integration Tests
// =============================================================================
