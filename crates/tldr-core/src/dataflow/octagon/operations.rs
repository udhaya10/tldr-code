//! Lattice operations for octagon DBMs: join, meet, widening, narrowing.
//!
//! # Join (Least Upper Bound)
//!
//! Join of two octagons is the element-wise maximum of their closed DBMs.
//! Both inputs must be closed for correctness.
//!
//! # Meet (Greatest Lower Bound)
//!
//! Meet is the element-wise minimum followed by re-closure.
//! The result may be empty (bottom).
//!
//! # Widening
//!
//! Standard widening: constraints that grow between iterations are
//! set to +infinity. Guarantees termination of fixpoint computation.
//!
//! Threshold widening: instead of jumping to +infinity, jump to the
//! next threshold value from a predefined set.
//!
//! # References
//!
//! - Mine 2006, Sections 4.4-4.6: Join, widening, narrowing
//! - Prior art Section 2.1, Q3: Widening thresholds

use super::bound::Bound;
use super::closure::{strong_closure, ClosureResult};
use super::dbm::Dbm;

/// Compute the join (least upper bound) of two DBMs.
///
/// The join is the element-wise maximum of entries from two closed DBMs.
/// The result is closed (since element-wise max of closed DBMs is closed).
///
/// # Preconditions
///
/// Both `a` and `b` must be strongly closed.
/// Both must have the same number of variables.
///
/// # Returns
///
/// A new DBM representing the join. Returns `None` if sizes differ.
pub fn join<B: Bound>(a: &Dbm<B>, b: &Dbm<B>) -> Option<Dbm<B>> {
    if a.n_vars() != b.n_vars() {
        return None;
    }

    let mut result = Dbm::new(a.n_vars());
    let dim = a.dim();

    for i in 0..dim {
        for j in 0..dim {
            let val = B::max(a.get(i, j), b.get(i, j));
            result.set(i, j, val);
        }
    }

    Some(result)
}

/// Compute the meet (greatest lower bound) of two DBMs.
///
/// The meet is the element-wise minimum followed by re-closure.
/// The result may be empty (bottom) if the intersection is inconsistent.
///
/// # Returns
///
/// `Some(dbm)` if the meet is non-empty, `None` if empty (bottom).
pub fn meet<B: Bound>(a: &Dbm<B>, b: &Dbm<B>) -> Option<Dbm<B>> {
    if a.n_vars() != b.n_vars() {
        return None;
    }

    let mut result = Dbm::new(a.n_vars());
    let dim = a.dim();

    for i in 0..dim {
        for j in 0..dim {
            let val = B::min(a.get(i, j), b.get(i, j));
            result.set(i, j, val);
        }
    }

    // Re-close to derive transitive constraints
    let closure_result = strong_closure(&mut result);
    if closure_result == ClosureResult::Empty {
        return None;
    }

    Some(result)
}

/// Standard widening of two DBMs.
///
/// For each entry:
/// - If `new[i,j] > old[i,j]` (constraint weakened), set to +infinity.
/// - Otherwise, keep `new[i,j]`.
///
/// This guarantees fixpoint convergence in at most O(n^2) iterations.
pub fn widen<B: Bound>(old: &Dbm<B>, new: &Dbm<B>) -> Dbm<B> {
    let mut result = Dbm::new(old.n_vars());
    let dim = old.dim();

    for i in 0..dim {
        for j in 0..dim {
            let old_val = old.get(i, j);
            let new_val = new.get(i, j);

            // If new constraint is weaker (grew), set to +infinity
            // If stable or tightened, keep the new value
            let widened_val = if new_val > old_val {
                B::infinity()
            } else {
                new_val
            };
            result.set(i, j, widened_val);
        }
    }

    result
}

/// Default widening thresholds for octagon analysis.
///
/// These values are commonly used as jump targets instead of +infinity.
/// Based on ASTREE conventions and common program constants.
pub const DEFAULT_THRESHOLDS: &[i64] = &[
    -128, -64, -32, -16, -8, -4, -2, -1, 0, 1, 2, 4, 8, 16, 32, 64, 128, 255, 256, 1024,
];

/// Widening with thresholds.
///
/// Instead of jumping to +infinity, jumps to the next threshold value
/// that is >= the new bound.
///
/// # Arguments
///
/// - `old`: DBM from previous iteration.
/// - `new`: DBM from current iteration.
/// - `thresholds`: Sorted list of threshold values (ascending).
///
/// # Returns
///
/// Widened DBM.
pub fn widen_with_thresholds<B: Bound>(
    old: &Dbm<B>,
    new: &Dbm<B>,
    thresholds: &[i64],
) -> Dbm<B> {
    let mut result = Dbm::new(old.n_vars());
    let dim = old.dim();

    for i in 0..dim {
        for j in 0..dim {
            let old_val = old.get(i, j);
            let new_val = new.get(i, j);

            let widened_val = if new_val > old_val {
                // Growing constraint: jump to next threshold >= new_val
                // instead of jumping straight to +infinity
                let mut jumped = B::infinity();
                for &t in thresholds {
                    let threshold_bound = B::from_i64(t);
                    if threshold_bound >= new_val {
                        jumped = threshold_bound;
                        break;
                    }
                }
                jumped
            } else {
                // Stable or tightened: keep the new value
                new_val
            };
            result.set(i, j, widened_val);
        }
    }

    result
}

/// Check if `a` is included in `b` (a <= b in the lattice).
///
/// For closed DBMs: `a <= b` iff `a[i,j] <= b[i,j]` for all i,j.
///
/// Only the right argument (b) needs to be closed.
pub fn is_included<B: Bound>(a: &Dbm<B>, b: &Dbm<B>) -> bool {
    if a.n_vars() != b.n_vars() {
        return false;
    }

    let dim = a.dim();
    for i in 0..dim {
        for j in 0..dim {
            if a.get(i, j) > b.get(i, j) {
                return false;
            }
        }
    }

    true
}

// =============================================================================
// Tests
// =============================================================================
