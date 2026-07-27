//! Strong closure, incremental closure, and tight closure for octagon DBMs.
//!
//! # Strong Closure (Mine 2006, Algorithm 1)
//!
//! Strong closure tightens all constraints in the DBM via Floyd-Warshall
//! shortest-path computation followed by a strengthening (Str) pass.
//! Complexity: O(n^3).
//!
//! # Incremental Closure (Bagnara et al. 2018)
//!
//! When adding a single constraint to an already-closed DBM, only O(n^2)
//! work is needed. This is the dominant use case during transfer functions.
//!
//! # Tight Closure
//!
//! For integer-valued variables, tight closure ensures that diagonal
//! entries `m[2i, 2i+1]` are even (i.e., `2 * floor(m[2i, 2i+1] / 2)`).
//! This provides tighter integer bounds.
//!
//! # References
//!
//! - Mine 2006, Section 4.3: Strong closure algorithm
//! - Bagnara et al. 2018: Incremental closure
//! - Prior art Section 2.1-2.2: Algorithm details

use super::bound::Bound;
use super::dbm::Dbm;

/// Result of a closure operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosureResult {
    /// Closure succeeded; DBM is now strongly closed.
    Closed,
    /// Negative cycle detected; the octagon is empty (bottom).
    Empty,
}

/// Perform strong closure on a DBM (Floyd-Warshall + strengthening pass).
///
/// After strong closure, the DBM satisfies:
/// - Triangle inequality: `m[i,j] <= m[i,k] + m[k,j]` for all i,j,k
/// - Strengthening: `m[i,j] <= (m[i,i_bar] + m[j_bar,j]) / 2`
/// - Diagonal: `m[i,i] = 0`
///
/// Returns `ClosureResult::Empty` if a negative diagonal entry is found
/// (indicating an inconsistent constraint set).
///
/// Complexity: O(n^3) where n is the number of program variables.
pub fn strong_closure<B: Bound>(dbm: &mut Dbm<B>) -> ClosureResult {
    let dim = dbm.dim();

    // Use a full 2n x 2n matrix for Floyd-Warshall to avoid half-matrix
    // aliasing issues. The half-matrix storage maps (i,j) and (j^1,i^1)
    // to the same physical cell, which can cause overwrites during
    // in-place Floyd-Warshall updates.
    let mut m = vec![B::infinity(); dim * dim];

    // Copy DBM into full matrix
    for i in 0..dim {
        for j in 0..dim {
            m[i * dim + j] = dbm.get(i, j);
        }
    }

    // Phase 1: Floyd-Warshall shortest path on full matrix
    for k in 0..dim {
        for i in 0..dim {
            let m_ik = m[i * dim + k];
            for j in 0..dim {
                let through_k = B::closure_add(m_ik, m[k * dim + j]);
                let current = m[i * dim + j];
                let tighter = B::min(current, through_k);
                if tighter != current {
                    m[i * dim + j] = tighter;
                }
            }
        }
    }

    // Phase 2: Strengthening (Str) pass (Mine 2006, Section 4.3)
    // m[i,j] = min(m[i,j], (m[i, i^1] + m[j^1, j]) / 2)
    for i in 0..dim {
        let i_bar = Dbm::<B>::complement(i);
        let m_i_ibar = m[i * dim + i_bar];
        for j in 0..dim {
            let j_bar = Dbm::<B>::complement(j);
            let coherence_sum = B::closure_add(m_i_ibar, m[j_bar * dim + j]);
            let coherence_val = B::half(coherence_sum);
            let current = m[i * dim + j];
            let tighter = B::min(current, coherence_val);
            if tighter != current {
                m[i * dim + j] = tighter;
            }
        }
    }

    // Phase 3: Consistency check -- negative diagonal means empty (bottom)
    let mut result = ClosureResult::Closed;
    for i in 0..dim {
        if m[i * dim + i] < B::zero() {
            result = ClosureResult::Empty;
            break;
        }
    }

    // Write back to DBM. For coherent pairs (i,j) and (j^1,i^1), take the
    // minimum of both values to maintain coherence invariant.
    // Always write back (even for Empty) so that has_negative_cycle() can
    // inspect the DBM diagonal after closure.
    for i in 0..dim {
        for j in 0..dim {
            dbm.set(i, j, m[i * dim + j]);
        }
    }

    result
}

/// Perform incremental closure after adding a single constraint.
///
/// Given an already-closed DBM and a new constraint `m[i,j] <= c`,
/// updates only the affected entries in O(n^2) time.
///
/// # Arguments
///
/// - `dbm`: A strongly-closed DBM (must be closed before calling).
/// - `i`, `j`: Indices of the new constraint.
/// - `value`: The new constraint bound.
///
/// Returns `ClosureResult::Empty` if the new constraint creates
/// an inconsistency.
pub fn incremental_closure<B: Bound>(
    dbm: &mut Dbm<B>,
    a: usize,
    b: usize,
    value: B,
) -> ClosureResult {
    let dim = dbm.dim();

    // Work on a full matrix to avoid half-matrix aliasing
    let mut m = vec![B::infinity(); dim * dim];
    for i in 0..dim {
        for j in 0..dim {
            m[i * dim + j] = dbm.get(i, j);
        }
    }

    // Apply the new constraint if tighter than existing
    let current = m[a * dim + b];
    let c = B::min(current, value);
    m[a * dim + b] = c;

    // Also set the coherence counterpart: m[b^1, a^1] = min(m[b^1, a^1], c)
    let a_bar = Dbm::<B>::complement(a);
    let b_bar = Dbm::<B>::complement(b);
    let current_bar = m[b_bar * dim + a_bar];
    let c_bar = B::min(current_bar, c);
    m[b_bar * dim + a_bar] = c_bar;

    // Pre-compute the "bridge" term for paths that traverse BOTH new edges:
    //   (a,b) then (b^1,a^1): c + m[b, b^1] + c_bar
    //   (b^1,a^1) then (a,b): c_bar + m[a^1, a] + c
    let bridge_ab_bar = B::closure_add(
        B::closure_add(c, m[b * dim + b_bar]),
        c_bar,
    );
    let bridge_bar_ab = B::closure_add(
        B::closure_add(c_bar, m[a_bar * dim + a]),
        c,
    );

    // Incremental shortest path (Mine 2006 / Bagnara et al. 2008):
    // For all (i,j), update via the four possible path types through the
    // new edge (a,b) and its coherence counterpart (b^1, a^1):
    //   1. i -> a -> b -> j                           (via (a,b))
    //   2. i -> b^1 -> a^1 -> j                       (via (b^1,a^1))
    //   3. i -> a -> b -> b^1 -> a^1 -> j             (via (a,b) then (b^1,a^1))
    //   4. i -> b^1 -> a^1 -> a -> b -> j             (via (b^1,a^1) then (a,b))
    for i in 0..dim {
        for j in 0..dim {
            let cur = m[i * dim + j];

            // Path 1: through (a,b) edge
            let via_ab = B::closure_add(
                B::closure_add(m[i * dim + a], c),
                m[b * dim + j],
            );

            // Path 2: through coherence counterpart (b^1, a^1) edge
            let via_bar = B::closure_add(
                B::closure_add(m[i * dim + b_bar], c_bar),
                m[a_bar * dim + j],
            );

            // Path 3: (a,b) then bridge to (b^1,a^1)
            let via_ab_bar = B::closure_add(
                B::closure_add(m[i * dim + a], bridge_ab_bar),
                m[a_bar * dim + j],
            );

            // Path 4: (b^1,a^1) then bridge to (a,b)
            let via_bar_ab = B::closure_add(
                B::closure_add(m[i * dim + b_bar], bridge_bar_ab),
                m[b * dim + j],
            );

            let best = B::min(
                B::min(cur, B::min(via_ab, via_bar)),
                B::min(via_ab_bar, via_bar_ab),
            );
            if best != cur {
                m[i * dim + j] = best;
            }
        }
    }

    // Strengthening pass
    for i in 0..dim {
        let i_comp = Dbm::<B>::complement(i);
        let m_i_icomp = m[i * dim + i_comp];
        for j in 0..dim {
            let j_comp = Dbm::<B>::complement(j);
            let coherence_sum = B::closure_add(m_i_icomp, m[j_comp * dim + j]);
            let coherence_val = B::half(coherence_sum);
            let cur = m[i * dim + j];
            let tighter = B::min(cur, coherence_val);
            if tighter != cur {
                m[i * dim + j] = tighter;
            }
        }
    }

    // Consistency check
    for i in 0..dim {
        if m[i * dim + i] < B::zero() {
            return ClosureResult::Empty;
        }
    }

    // Write back with coherence: take min of (i,j) and (j^1, i^1).
    // Lower triangle (j <= i) covers most entries via coherence-based indexing.
    for i in 0..dim {
        for j in 0..=i {
            let i_b = Dbm::<B>::complement(i);
            let j_b = Dbm::<B>::complement(j);
            let val_ij = m[i * dim + j];
            let val_coherent = m[j_b * dim + i_b];
            let best = B::min(val_ij, val_coherent);
            dbm.set(i, j, best);
        }
    }

    // Write back same-variable upper-diagonal entries m[2k, 2k+1].
    // These are stored separately in Dbm::upper_diag and are NOT reached
    // by the lower-triangle loop above (since 2k < 2k+1 means j > i).
    // Their coherence counterpart is themselves: complement(2k)=2k+1,
    // complement(2k+1)=2k, so (j^1, i^1) = (2k, 2k+1) again.
    let n_vars = dim / 2;
    for var in 0..n_vars {
        let pos = 2 * var;
        let neg = 2 * var + 1;
        let val = m[pos * dim + neg];
        dbm.set(pos, neg, val);
    }

    ClosureResult::Closed
}

/// Perform tight closure for integer-valued variables.
///
/// After strong closure, tightens diagonal entries:
/// `m[2i, 2i+1] = tighten(m[2i, 2i+1])` where `tighten(x) = 2 * floor(x/2)`.
///
/// This ensures integer bounds are as tight as possible.
pub fn tight_closure<B: Bound>(dbm: &mut Dbm<B>) -> ClosureResult {
    let n = dbm.n_vars();

    // Tighten unary constraint entries for integer variables.
    // For each variable i, the entries m[2i, 2i+1] and m[2i+1, 2i]
    // represent 2*upper and 2*lower bounds respectively.
    // Tighten them to even values: 2 * floor(x / 2).
    for var in 0..n {
        let pos = 2 * var;
        let neg = 2 * var + 1;

        let upper = dbm.get(neg, pos);
        dbm.set(neg, pos, B::tighten(upper));

        let lower = dbm.get(pos, neg);
        dbm.set(pos, neg, B::tighten(lower));
    }

    ClosureResult::Closed
}

/// Check if a DBM has a negative cycle (is empty / bottom).
///
/// A negative cycle exists if any diagonal entry `m[i,i] < 0`.
pub fn has_negative_cycle<B: Bound>(dbm: &Dbm<B>) -> bool {
    let dim = dbm.dim();
    for i in 0..dim {
        if dbm.get(i, i) < B::zero() {
            return true;
        }
    }
    false
}

// =============================================================================
// Tests
// =============================================================================
