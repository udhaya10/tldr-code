//! Bound trait and implementations for octagon domain.
//!
//! The `Bound` trait abstracts over numeric types used in the DBM.
//! Two implementations are provided:
//!
//! - `f64`: Uses `f64::INFINITY` / `f64::NEG_INFINITY` as sentinels.
//!   Arithmetic uses `f64::next_up()` for sound over-approximation
//!   (equivalent to rounding toward +infinity per APRON convention).
//!
//! - `i64`: Uses saturating arithmetic. Overflow maps to `i64::MAX`
//!   (treated as +infinity).
//!
//! # References
//!
//! - Mine 2006, Section 4.1: Numeric representations
//! - APRON: `octD` (double), `octI` (long int) backends
//! - Prior art Section 6, Q4: f64 with explicit next_up rounding

use std::fmt::Debug;

/// Trait for numeric bounds used in the DBM.
///
/// Implementations must provide:
/// - Infinity sentinels (positive and negative)
/// - Sound addition (over-approximating for upper bounds)
/// - Comparison operations (min, max)
/// - A zero element
/// - Division by 2 (for strengthening pass)
pub trait Bound: Copy + Clone + Debug + PartialEq + PartialOrd + Send + Sync + 'static {
    /// Positive infinity sentinel.
    fn infinity() -> Self;

    /// Negative infinity sentinel.
    fn neg_infinity() -> Self;

    /// The zero element.
    fn zero() -> Self;

    /// Whether this value represents positive infinity.
    fn is_pos_infinity(self) -> bool;

    /// Whether this value represents negative infinity.
    fn is_neg_infinity(self) -> bool;

    /// Sound addition: a + b, rounding toward +infinity for soundness.
    ///
    /// If either operand is +infinity, result is +infinity.
    /// For `f64`, uses `next_up()` after addition.
    /// For `i64`, uses saturating arithmetic; overflow yields `i64::MAX`.
    fn add(self, other: Self) -> Self;

    /// Minimum of two bounds.
    fn min(self, other: Self) -> Self;

    /// Maximum of two bounds.
    fn max(self, other: Self) -> Self;

    /// Division by 2 (for the strengthening pass in strong closure).
    ///
    /// For integers, uses floor division: `floor(x / 2)`.
    /// For floats, exact division (no rounding needed for /2).
    fn half(self) -> Self;

    /// Negation: -x.
    fn neg(self) -> Self;

    /// Floor operation for tight closure on integers.
    /// For floats, this is a no-op (returns self).
    /// For integers, returns `2 * floor(x / 2)`.
    fn tighten(self) -> Self;

    /// Exact addition for closure algorithms (no rounding).
    ///
    /// Unlike `add()`, this does NOT apply `next_up()` rounding.
    /// Used internally by Floyd-Warshall and incremental closure where
    /// exact shortest-path arithmetic is required (APRON convention:
    /// closure operates on exact values, rounding is applied at the
    /// boundary when constraints are introduced).
    ///
    /// Infinity propagation rules are the same as `add()`.
    fn closure_add(self, other: Self) -> Self;

    /// Convert an `i64` value to this bound type.
    ///
    /// Used by transfer functions to convert integer constants from
    /// `OctExpr` into the DBM's bound representation.
    fn from_i64(val: i64) -> Self;
}

// =============================================================================
// f64 Bound implementation
// =============================================================================

impl Bound for f64 {
    #[inline]
    fn infinity() -> Self {
        f64::INFINITY
    }

    #[inline]
    fn neg_infinity() -> Self {
        f64::NEG_INFINITY
    }

    #[inline]
    fn zero() -> Self {
        0.0
    }

    #[inline]
    fn is_pos_infinity(self) -> bool {
        self == f64::INFINITY
    }

    #[inline]
    fn is_neg_infinity(self) -> bool {
        self == f64::NEG_INFINITY
    }

    #[inline]
    fn add(self, other: Self) -> Self {
        if self.is_pos_infinity() || other.is_pos_infinity() {
            return f64::INFINITY;
        }
        if self.is_neg_infinity() || other.is_neg_infinity() {
            return f64::NEG_INFINITY;
        }
        // Sound rounding: next_up() ensures over-approximation
        // (rounding toward +infinity per APRON convention, Mine 2006 Section 4.1)
        (self + other).next_up()
    }

    #[inline]
    fn min(self, other: Self) -> Self {
        f64::min(self, other)
    }

    #[inline]
    fn max(self, other: Self) -> Self {
        f64::max(self, other)
    }

    #[inline]
    fn half(self) -> Self {
        self / 2.0
    }

    #[inline]
    fn neg(self) -> Self {
        -self
    }

    #[inline]
    fn tighten(self) -> Self {
        // No-op for floats (tight closure only meaningful for integers)
        self
    }

    #[inline]
    fn closure_add(self, other: Self) -> Self {
        if self.is_pos_infinity() || other.is_pos_infinity() {
            return f64::INFINITY;
        }
        if self.is_neg_infinity() || other.is_neg_infinity() {
            return f64::NEG_INFINITY;
        }
        // Exact addition without next_up rounding, for closure algorithms
        self + other
    }

    #[inline]
    fn from_i64(val: i64) -> Self {
        val as f64
    }
}

// =============================================================================
// i64 Bound implementation
// =============================================================================

impl Bound for i64 {
    #[inline]
    fn infinity() -> Self {
        i64::MAX
    }

    #[inline]
    fn neg_infinity() -> Self {
        i64::MIN
    }

    #[inline]
    fn zero() -> Self {
        0
    }

    #[inline]
    fn is_pos_infinity(self) -> bool {
        self == i64::MAX
    }

    #[inline]
    fn is_neg_infinity(self) -> bool {
        self == i64::MIN
    }

    #[inline]
    fn add(self, other: Self) -> Self {
        if self == i64::MAX || other == i64::MAX {
            return i64::MAX;
        }
        if self == i64::MIN || other == i64::MIN {
            return i64::MIN;
        }
        // Saturating arithmetic: overflow -> i64::MAX (treated as +infinity)
        // TODO: implement properly
        self.saturating_add(other)
    }

    #[inline]
    fn min(self, other: Self) -> Self {
        Ord::min(self, other)
    }

    #[inline]
    fn max(self, other: Self) -> Self {
        Ord::max(self, other)
    }

    #[inline]
    fn half(self) -> Self {
        // Floor division for integers
        if self >= 0 {
            self / 2
        } else {
            (self - 1) / 2
        }
    }

    #[inline]
    fn neg(self) -> Self {
        if self == i64::MIN {
            i64::MAX
        } else if self == i64::MAX {
            i64::MIN
        } else {
            -self
        }
    }

    #[inline]
    fn tighten(self) -> Self {
        // For tight closure: 2 * floor(x / 2)
        // This ensures diagonal entries m[2i,2i+1] are even
        if self == i64::MAX || self == i64::MIN {
            return self;
        }
        let h = self.half();
        h.saturating_mul(2)
    }

    #[inline]
    fn closure_add(self, other: Self) -> Self {
        // For i64, closure_add is identical to add (saturating arithmetic)
        if self == i64::MAX || other == i64::MAX {
            return i64::MAX;
        }
        if self == i64::MIN || other == i64::MIN {
            return i64::MIN;
        }
        self.saturating_add(other)
    }

    #[inline]
    fn from_i64(val: i64) -> Self {
        val
    }
}

// =============================================================================
// Tests
// =============================================================================
