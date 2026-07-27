//! Transfer functions for the octagon domain.
//!
//! Transfer functions model the effect of program statements on the
//! octagonal abstract state:
//!
//! - **Assignment** (`x := expr`): Forgets old constraints on `x`,
//!   then derives new constraints from the expression.
//! - **Guard/Test** (`x > 0`, `x <= y`): Adds constraints to the DBM.
//! - **Forget** (`forget x`): Sets all constraints involving `x` to +infinity.
//!
//! # References
//!
//! - Mine 2006, Section 4.7: Transfer functions
//! - Prior art Section 6.2: Assignment transfer

use super::bound::Bound;
use super::dbm::Dbm;
use super::closure::ClosureResult;

/// The kind of expression on the RHS of an assignment.
#[derive(Debug, Clone, PartialEq)]
pub enum OctExpr {
    /// Constant: `x := c`
    Constant(i64),
    /// Variable copy: `x := y`
    Variable(usize),
    /// Variable plus constant: `x := y + c`
    VarPlusConst(usize, i64),
    /// Variable minus constant: `x := y - c`
    VarMinusConst(usize, i64),
    /// Negated variable: `x := -y`
    NegVariable(usize),
    /// Unknown expression: `x := ?` (forget all info about x)
    Unknown,
}

/// Assign `x := expr` in the octagon.
///
/// Steps:
/// 1. Forget all constraints on variable `var_idx`.
/// 2. Add new constraints based on `expr`.
/// 3. Optionally run incremental closure.
///
/// # Arguments
///
/// - `dbm`: The DBM to modify in-place.
/// - `var_idx`: The program variable index (0-based) being assigned.
/// - `expr`: The expression being assigned.
///
/// # Returns
///
/// `ClosureResult::Closed` if assignment succeeded,
/// `ClosureResult::Empty` if assignment created a contradiction.
pub fn assign<B: Bound>(dbm: &mut Dbm<B>, var_idx: usize, expr: &OctExpr) -> ClosureResult {
    let pos = 2 * var_idx;
    let neg = pos + 1;

    // Step 1: Forget all constraints on var_idx
    forget(dbm, var_idx);

    // Step 2: Add new constraints based on expression
    match *expr {
        OctExpr::Constant(c) => {
            // x := c
            // Upper bound: x <= c => m[2v+1, 2v] = 2c
            // Lower bound: x >= c => m[2v, 2v+1] = -2c
            let two_c = B::from_i64(2 * c);
            dbm.set(neg, pos, two_c);
            dbm.set(pos, neg, two_c.neg());
        }
        OctExpr::Variable(y) => {
            // x := y
            // Copy unary bounds from y to x:
            //   m[2x+1, 2x] = m[2y+1, 2y]  (upper bound)
            //   m[2x, 2x+1] = m[2y, 2y+1]  (lower bound)
            let y_pos = 2 * y;
            let y_neg = y_pos + 1;
            dbm.set(neg, pos, dbm.get(y_neg, y_pos));
            dbm.set(pos, neg, dbm.get(y_pos, y_neg));

            // Relational: x - y = 0
            //   m[2y, 2x] = 0 (x - y <= 0)
            //   m[2x, 2y] = 0 (y - x <= 0)
            dbm.set(y_pos, pos, B::zero());
            dbm.set(pos, y_pos, B::zero());

            // Also set cross-negated constraints for coherence:
            //   m[2y+1, 2x+1] = 0 (-x - (-y) <= 0, i.e., y - x <= 0)
            //   m[2x+1, 2y+1] = 0 (-y - (-x) <= 0, i.e., x - y <= 0)
            dbm.set(y_neg, neg, B::zero());
            dbm.set(neg, y_neg, B::zero());
        }
        OctExpr::VarPlusConst(y, c) => {
            // x := y + c
            // Upper bound: m[2x+1, 2x] = m[2y+1, 2y] + 2c
            // Lower bound: m[2x, 2x+1] = m[2y, 2y+1] - 2c
            let y_pos = 2 * y;
            let y_neg = y_pos + 1;
            let two_c = B::from_i64(2 * c);

            let y_upper = dbm.get(y_neg, y_pos);
            if !y_upper.is_pos_infinity() {
                dbm.set(neg, pos, y_upper.closure_add(two_c));
            }

            let y_lower = dbm.get(y_pos, y_neg);
            if !y_lower.is_pos_infinity() {
                dbm.set(pos, neg, y_lower.closure_add(two_c.neg()));
            }

            // Relational: x - y = c
            //   m[2y, 2x] = c  (x - y <= c)
            //   m[2x, 2y] = -c (y - x <= -c)
            let bc = B::from_i64(c);
            dbm.set(y_pos, pos, bc);
            dbm.set(pos, y_pos, bc.neg());

            // Cross-negated relational constraints:
            //   m[2y+1, 2x+1] = -c  (-x + y <= -c, i.e., y - x <= -c)
            //   m[2x+1, 2y+1] = c   (-y + x <= c, i.e., x - y <= c)
            dbm.set(y_neg, neg, bc.neg());
            dbm.set(neg, y_neg, bc);
        }
        OctExpr::VarMinusConst(y, c) => {
            // x := y - c is equivalent to x := y + (-c)
            let y_pos = 2 * y;
            let y_neg = y_pos + 1;
            let two_c = B::from_i64(2 * c);

            let y_upper = dbm.get(y_neg, y_pos);
            if !y_upper.is_pos_infinity() {
                dbm.set(neg, pos, y_upper.closure_add(two_c.neg()));
            }

            let y_lower = dbm.get(y_pos, y_neg);
            if !y_lower.is_pos_infinity() {
                dbm.set(pos, neg, y_lower.closure_add(two_c));
            }

            // Relational: x - y = -c
            let bc = B::from_i64(c);
            dbm.set(y_pos, pos, bc.neg());
            dbm.set(pos, y_pos, bc);

            // Cross-negated:
            dbm.set(y_neg, neg, bc);
            dbm.set(neg, y_neg, bc.neg());
        }
        OctExpr::NegVariable(y) => {
            // x := -y
            // Upper bound: x <= -lower(y) => m[2x+1, 2x] = m[2y, 2y+1]
            //   (since -lower(y) = m[2y, 2y+1] / 2, scaled: m[2y, 2y+1])
            // Lower bound: x >= -upper(y) => m[2x, 2x+1] = m[2y+1, 2y]
            //   (since -upper(y) = -m[2y+1, 2y] / 2, but as negated: m[2y+1, 2y])
            //
            // Actually in the octagon encoding, negation swaps pos/neg:
            //   m[2x+1, 2x] = m[2y, 2y+1]  (upper of x = negated lower of y)
            //   m[2x, 2x+1] = m[2y+1, 2y]  (lower of x = negated upper of y)
            let y_pos = 2 * y;
            let y_neg = y_pos + 1;
            dbm.set(neg, pos, dbm.get(y_pos, y_neg));
            dbm.set(pos, neg, dbm.get(y_neg, y_pos));

            // Relational: x + y = 0
            //   m[2y+1, 2x] = 0  (x + y <= 0 in sum form)
            //   m[2x, 2y+1] = 0
            //   m[2y, 2x+1] = 0
            //   m[2x+1, 2y] = 0
            dbm.set(y_neg, pos, B::zero());
            dbm.set(pos, y_neg, B::zero());
            dbm.set(y_pos, neg, B::zero());
            dbm.set(neg, y_pos, B::zero());
        }
        OctExpr::Unknown => {
            // x := ? -- forget already done above, nothing more to add
        }
    }

    ClosureResult::Closed
}

/// Add a guard/test constraint to the octagon.
///
/// Supported guards:
/// - `x >= c`: Lower bound on x
/// - `x <= c`: Upper bound on x
/// - `x - y <= c`: Difference bound
/// - `x + y <= c`: Sum bound
///
/// # Returns
///
/// `ClosureResult::Empty` if the guard makes the state empty (dead branch).
pub fn guard<B: Bound>(dbm: &mut Dbm<B>, cond: &OctGuard) -> ClosureResult {
    match *cond {
        OctGuard::GtEq(x, c) => {
            // x >= c => -2x <= -2c => m[2x, 2x+1] = min(old, -2c)
            let pos = 2 * x;
            let neg = pos + 1;
            let bound = B::from_i64(-2 * c);
            dbm.set(pos, neg, dbm.get(pos, neg).min(bound));
        }
        OctGuard::LtEq(x, c) => {
            // x <= c => 2x <= 2c => m[2x+1, 2x] = min(old, 2c)
            let pos = 2 * x;
            let neg = pos + 1;
            let bound = B::from_i64(2 * c);
            dbm.set(neg, pos, dbm.get(neg, pos).min(bound));
        }
        OctGuard::Gt(x, c) => {
            // x > c => x >= c + 1 (integer domain) => -2x <= -2(c+1)
            let pos = 2 * x;
            let neg = pos + 1;
            let bound = B::from_i64(-2 * (c + 1));
            dbm.set(pos, neg, dbm.get(pos, neg).min(bound));
        }
        OctGuard::Lt(x, c) => {
            // x < c => x <= c - 1 (integer domain) => 2x <= 2(c-1)
            let pos = 2 * x;
            let neg = pos + 1;
            let bound = B::from_i64(2 * (c - 1));
            dbm.set(neg, pos, dbm.get(neg, pos).min(bound));
        }
        OctGuard::DiffLtEq(x, y, c) => {
            // x - y <= c
            // In the DBM: m[2y, 2x] encodes (+x) - (+y) <= m[2y, 2x]
            // So: m[2y, 2x] = min(old, c)
            let x_pos = 2 * x;
            let y_pos = 2 * y;
            let bound = B::from_i64(c);
            dbm.set(y_pos, x_pos, dbm.get(y_pos, x_pos).min(bound));
        }
        OctGuard::SumLtEq(x, y, c) => {
            // x + y <= c
            // In the DBM: m[2y+1, 2x] encodes (+x) - (-y) <= m[2y+1, 2x]
            //   which is (+x) + (+y) <= m[2y+1, 2x]
            // So: m[2y+1, 2x] = min(old, c)
            let x_pos = 2 * x;
            let y_neg = 2 * y + 1;
            let bound = B::from_i64(c);
            dbm.set(y_neg, x_pos, dbm.get(y_neg, x_pos).min(bound));
        }
    }

    // Check for empty state: if any diagonal is negative, the state is bottom
    let dim = dbm.dim();
    for i in 0..dim {
        let diag = dbm.get(i, i);
        if diag < B::zero() {
            return ClosureResult::Empty;
        }
    }

    ClosureResult::Closed
}

/// Guard/test constraint.
#[derive(Debug, Clone, PartialEq)]
pub enum OctGuard {
    /// x >= c (lower bound)
    GtEq(usize, i64),
    /// x <= c (upper bound)
    LtEq(usize, i64),
    /// x > c (strict lower bound, encoded as x >= c+1 for integers)
    Gt(usize, i64),
    /// x < c (strict upper bound, encoded as x <= c-1 for integers)
    Lt(usize, i64),
    /// x - y <= c (difference)
    DiffLtEq(usize, usize, i64),
    /// x + y <= c (sum)
    SumLtEq(usize, usize, i64),
}

/// Forget all constraints involving variable `var_idx`.
///
/// Sets all DBM entries in the row and column corresponding to
/// both `2*var_idx` and `2*var_idx + 1` to +infinity (except diagonal).
pub fn forget<B: Bound>(dbm: &mut Dbm<B>, var_idx: usize) {
    let dim = dbm.dim();
    let pos = 2 * var_idx;
    let neg = 2 * var_idx + 1;

    for k in 0..dim {
        // Skip self-loops (diagonal entries must stay 0)
        if k != pos {
            dbm.set(pos, k, B::infinity());
            dbm.set(k, pos, B::infinity());
        }
        if k != neg {
            dbm.set(neg, k, B::infinity());
            dbm.set(k, neg, B::infinity());
        }
    }
}

// =============================================================================
// Tests
// =============================================================================
