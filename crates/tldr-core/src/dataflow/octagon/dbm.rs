//! Difference Bound Matrix (DBM) storage for the octagon domain.
//!
//! The DBM represents octagonal constraints `+-xi +- xj <= c` using a
//! half-matrix representation (Mine 2006). For `n` program variables,
//! the full DBM is `2n x 2n`, but the half-matrix exploits coherence
//! to store only the lower-left triangle.
//!
//! # Variable Indexing
//!
//! Each program variable `xi` maps to two DBM indices:
//! - `2i` (positive literal)
//! - `2i+1` (negative literal / negated)
//!
//! The complement of index `k` is `k XOR 1`.
//!
//! # Storage Layout
//!
//! The half-matrix is stored as a contiguous `Vec<B>` with
//! `2n * (2n + 1) / 2` entries, indexed by `(i, j)` where `i >= j`.
//! This ensures cache-friendly access patterns.
//!
//! # References
//!
//! - Mine 2006, Section 3.2: DBM representation
//! - APRON: `oct_hmat.c` half-matrix implementation
//! - Prior art Section 1.1: Half-matrix layout

use super::bound::Bound;

/// Half-matrix Difference Bound Matrix for `n` variables.
///
/// Stores octagonal constraints in a flat `Vec<B>` with
/// `2n * (2n + 1) / 2` entries for the lower triangle, plus `n`
/// entries for same-variable upper-triangle pairs `m[2k, 2k+1]`.
///
/// Each entry `m[i, j]` represents the constraint
/// `x_j - x_i <= m[i, j]` where `x_k` for even `k` is the
/// positive literal and for odd `k` is the negated literal.
///
/// The coherence property `m[i,j] = m[j^1, i^1]` relates off-block
/// entries, but same-variable pairs `(2k, 2k+1)` are self-coherent
/// and need independent storage from `(2k+1, 2k)`.
#[derive(Debug, Clone)]
pub struct Dbm<B: Bound> {
    /// Number of program variables (n). The DBM dimension is 2n.
    n_vars: usize,
    /// Flat storage for the half-matrix (lower triangle). Length = 2n * (2n + 1) / 2.
    data: Vec<B>,
    /// Separate storage for same-variable upper-triangle entries `m[2k, 2k+1]`.
    /// Length = n. These are self-coherent and cannot alias with lower-triangle
    /// entries because the coherence remap `(j^1, i^1)` for `(2k, 2k+1)` yields
    /// `(2k, 2k+1)` again (still upper triangle).
    upper_diag: Vec<B>,
}

impl<B: Bound> Dbm<B> {
    /// Create a new DBM for `n` program variables.
    ///
    /// All entries are initialized to +infinity (unconstrained),
    /// except diagonal entries `m[i, i]` which are 0.
    pub fn new(n_vars: usize) -> Self {
        let dim = 2 * n_vars;
        let size = dim * (dim + 1) / 2;
        let mut data = vec![B::infinity(); size];

        // Set diagonal to zero: m[i, i] = 0
        for i in 0..dim {
            let idx = Self::lower_index(i, i);
            if idx < data.len() {
                data[idx] = B::zero();
            }
        }

        // Upper-diagonal entries m[2k, 2k+1] initialized to +infinity
        let upper_diag = vec![B::infinity(); n_vars];

        Dbm { n_vars, data, upper_diag }
    }

    /// Number of program variables.
    pub fn n_vars(&self) -> usize {
        self.n_vars
    }

    /// DBM dimension (2 * n_vars).
    pub fn dim(&self) -> usize {
        2 * self.n_vars
    }

    /// Total number of entries in the half-matrix (lower triangle).
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the DBM has zero variables.
    pub fn is_empty(&self) -> bool {
        self.n_vars == 0
    }

    /// Check if `(i, j)` is a same-variable upper-triangle entry: `i` is even
    /// and `j == i + 1` (i.e., `(2k, 2k+1)` for some variable `k`).
    #[inline]
    fn is_upper_diag(i: usize, j: usize) -> bool {
        i < j && (i ^ 1) == j
    }

    /// Compute the flat index for a lower-triangle entry `(i, j)` where `i >= j`.
    #[inline]
    fn lower_index(i: usize, j: usize) -> usize {
        i * (i + 1) / 2 + j
    }

    /// Compute the flat index for half-matrix entry `(i, j)`.
    ///
    /// For the lower-left triangle (`i >= j`), index = `i * (i + 1) / 2 + j`.
    ///
    /// For upper-triangle entries where `i < j`, the coherence property
    /// `m[i, j] = m[j XOR 1, i XOR 1]` (Mine 2006, Prop. 4) is used to
    /// remap to a lower-triangle entry. Same-variable pairs `(2k, 2k+1)`
    /// are handled separately via `upper_diag`.
    fn index_of(i: usize, j: usize) -> usize {
        if i >= j {
            Self::lower_index(i, j)
        } else {
            // Coherence: m[i, j] = m[j^1, i^1]
            let ci = j ^ 1;
            let cj = i ^ 1;
            // For off-block pairs, (ci, cj) is in the lower triangle
            Self::lower_index(ci, cj)
        }
    }

    /// Get the constraint value `m[i, j]`.
    ///
    /// Returns the bound for constraint `x_j - x_i <= m[i, j]`.
    pub fn get(&self, i: usize, j: usize) -> B {
        if Self::is_upper_diag(i, j) {
            // Same-variable upper-triangle: stored in upper_diag[k] where i = 2k
            self.upper_diag[i / 2]
        } else {
            let idx = Self::index_of(i, j);
            self.data[idx]
        }
    }

    /// Set the constraint value `m[i, j] = value`.
    pub fn set(&mut self, i: usize, j: usize, value: B) {
        if Self::is_upper_diag(i, j) {
            // Same-variable upper-triangle: stored in upper_diag[k] where i = 2k
            self.upper_diag[i / 2] = value;
        } else {
            let idx = Self::index_of(i, j);
            self.data[idx] = value;
        }
    }

    /// Compute the complement index: `i XOR 1`.
    ///
    /// Maps `2k -> 2k+1` and `2k+1 -> 2k`.
    #[inline]
    pub fn complement(i: usize) -> usize {
        i ^ 1
    }

    /// Access the underlying data slice (for memory layout verification).
    pub fn raw_data(&self) -> &[B] {
        &self.data
    }
}

// =============================================================================
// Tests
// =============================================================================
