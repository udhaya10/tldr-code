//! Guard Condition Parser and Interval Narrowing
//!
//! Parses branch condition strings from [`CfgEdge::condition`] into structured
//! [`GuardCondition`] values and applies them to narrow abstract values along
//! true/false branches.
//!
//! ## Parsing (Phase 5.1)
//!
//! The parser handles:
//! - Comparison operators: `==`, `!=`, `<`, `<=`, `>`, `>=`
//! - Reversed operand order: `0 == x`, `5 < x`
//! - Null checks across languages: Python (`is None`/`is not None`), TypeScript
//!   (`=== null`/`!== null`), Go (`== nil`/`!= nil`), Rust (`.is_some()`/`.is_none()`),
//!   C/C++ (`!= nullptr`/`!= NULL`)
//! - Truthiness: bare identifiers, `!x`, `not x`
//! - Negative integer literals: `x > -1`, `x == -5`
//!
//! Unparseable conditions return `None` (conservative: no narrowing applied).
//!
//! ## Narrowing (Phase 5.2)
//!
//! [`narrow_value`] applies a guard condition to an [`AbstractValue`] to produce
//! a tighter (subset) abstract value. [`narrow_state`] applies narrowing to the
//! relevant variable in an [`AbstractState`].
//!
//! Soundness invariant: narrowing never removes concrete values that could
//! actually occur. When in doubt, the result is conservative (unchanged).

use super::abstract_interp::{AbstractState, AbstractValue, Nullability};

/// A parsed guard condition from a CFG branch edge.
///
/// Represents the structured form of a condition string such as `"x > 0"` or
/// `"x is not None"`. Used by guard-aware transfer functions to narrow abstract
/// values along true/false branches.
#[derive(Debug, Clone, PartialEq)]
pub enum GuardCondition {
    /// `x == value` (equality comparison)
    Eq {
        /// The variable being compared.
        var: String,
        /// The integer constant on the right-hand side.
        value: i64,
    },
    /// `x != value` (inequality comparison)
    Neq {
        /// The variable being compared.
        var: String,
        /// The integer constant on the right-hand side.
        value: i64,
    },
    /// `x < value` (strictly less than)
    Lt {
        /// The variable being compared.
        var: String,
        /// The upper bound (exclusive).
        value: i64,
    },
    /// `x <= value` (less than or equal)
    Le {
        /// The variable being compared.
        var: String,
        /// The upper bound (inclusive).
        value: i64,
    },
    /// `x > value` (strictly greater than)
    Gt {
        /// The variable being compared.
        var: String,
        /// The lower bound (exclusive).
        value: i64,
    },
    /// `x >= value` (greater than or equal)
    Ge {
        /// The variable being compared.
        var: String,
        /// The lower bound (inclusive).
        value: i64,
    },
    /// Variable is not null.
    ///
    /// Recognized patterns:
    /// - Python: `x is not None`
    /// - TypeScript/JS: `x !== null`, `x != null`
    /// - Go/Ruby: `x != nil`
    /// - Rust: `x.is_some()`
    /// - C/C++: `x != nullptr`, `x != NULL`
    NotNull {
        /// The variable being checked for non-nullness.
        var: String,
    },
    /// Variable is null.
    ///
    /// Recognized patterns:
    /// - Python: `x is None`
    /// - TypeScript/JS: `x === null`, `x == null`
    /// - Go/Ruby: `x == nil`
    /// - Rust: `x.is_none()`
    IsNull {
        /// The variable being checked for nullness.
        var: String,
    },
    /// Truthiness check: bare variable `x` evaluates as truthy.
    Truthy {
        /// The variable being evaluated for truthiness.
        var: String,
    },
    /// Falsiness check: `!x` or `not x` evaluates as falsy.
    Falsy {
        /// The variable being evaluated for falsiness.
        var: String,
    },
}

/// Check whether a string is a valid simple identifier.
///
/// A valid identifier starts with an ASCII letter or underscore, followed by
/// zero or more ASCII alphanumeric characters or underscores. Does not accept
/// dotted paths (e.g., `obj.field`).
fn is_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }

    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Parse an integer literal, including negative values like `-5`.
fn parse_int_literal(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

/// Flip a comparison operator to its mirror.
///
/// When the constant is on the left side (`5 < x`), we flip the operator
/// to normalize to variable-on-left form (`x > 5`).
fn flip_operator(op: &str) -> Option<&'static str> {
    match op {
        "==" => Some("=="),
        "!=" => Some("!="),
        "<" => Some(">"),
        "<=" => Some(">="),
        ">" => Some("<"),
        ">=" => Some("<="),
        _ => None,
    }
}

/// Parse a condition string from [`CfgEdge::condition`] into a structured
/// [`GuardCondition`].
///
/// Returns `None` for unparseable conditions (conservative: no narrowing).
/// Follows the same text-level string-parsing approach as `parse_rhs_abstract`.
///
/// # Examples
///
/// ```rust,ignore
/// use tldr_core::dataflow::guard::parse_guard_condition;
///
/// let cond = parse_guard_condition("x > 0");
/// assert_eq!(cond, Some(GuardCondition::Gt { var: "x".into(), value: 0 }));
///
/// let cond = parse_guard_condition("x is not None");
/// assert_eq!(cond, Some(GuardCondition::NotNull { var: "x".into() }));
///
/// let cond = parse_guard_condition("f(x)");
/// assert_eq!(cond, None); // function call, can't parse
/// ```
pub fn parse_guard_condition(condition: &str) -> Option<GuardCondition> {
    let s = condition.trim();

    // Empty or whitespace-only
    if s.is_empty() {
        return None;
    }

    // --- Rust method-call null checks: x.is_some() / x.is_none() ---
    if let Some(rest) = s.strip_suffix(".is_some()") {
        let var = rest.trim();
        if is_identifier(var) {
            return Some(GuardCondition::NotNull {
                var: var.to_string(),
            });
        }
    }
    if let Some(rest) = s.strip_suffix(".is_none()") {
        let var = rest.trim();
        if is_identifier(var) {
            return Some(GuardCondition::IsNull {
                var: var.to_string(),
            });
        }
    }

    // --- Python-style null checks: "x is not None" / "x is None" ---
    if let Some(var_part) = s.strip_suffix(" is not None") {
        let var = var_part.trim();
        if is_identifier(var) {
            return Some(GuardCondition::NotNull {
                var: var.to_string(),
            });
        }
    }
    if let Some(var_part) = s.strip_suffix(" is None") {
        let var = var_part.trim();
        if is_identifier(var) {
            return Some(GuardCondition::IsNull {
                var: var.to_string(),
            });
        }
    }

    // --- Python-style falsy: "not x" ---
    if let Some(rest) = s.strip_prefix("not ") {
        let var = rest.trim();
        if is_identifier(var) {
            return Some(GuardCondition::Falsy {
                var: var.to_string(),
            });
        }
        // "not" followed by something non-identifier: unparseable
        return None;
    }

    // --- Comparison operators (two-operand) ---
    // Try splitting on two-char operators first, then single-char.
    // Order matters: check `!==`, `===`, `>=`, `<=`, `!=`, `==` before `<`, `>`.
    let two_char_ops = ["!==", "===", ">=", "<=", "!=", "=="];
    let one_char_ops = [">", "<"];

    for &op in &two_char_ops {
        if let Some(result) = try_comparison_split(s, op) {
            return Some(result);
        }
    }
    for &op in &one_char_ops {
        if let Some(result) = try_comparison_split(s, op) {
            return Some(result);
        }
    }

    // --- C-style falsy: "!x" ---
    if let Some(rest) = s.strip_prefix('!') {
        let var = rest.trim();
        if is_identifier(var) {
            return Some(GuardCondition::Falsy {
                var: var.to_string(),
            });
        }
        return None;
    }

    // --- Bare identifier → Truthy ---
    if is_identifier(s) {
        return Some(GuardCondition::Truthy {
            var: s.to_string(),
        });
    }

    // --- Unparseable ---
    None
}

/// Null-like keywords recognized across languages.
const NULL_KEYWORDS: &[&str] = &["null", "nil", "nullptr", "NULL", "None"];

/// Try splitting a condition on a comparison operator and parse both sides.
///
/// Handles both `var op literal` and `literal op var` (reversed) forms.
/// For null-keyword comparisons, produces `IsNull`/`NotNull` variants.
fn try_comparison_split(s: &str, op: &str) -> Option<GuardCondition> {
    // Find the operator in the string. We need to be careful with substrings:
    // "!==" contains "!=", so we try longer operators first in the caller.
    let idx = s.find(op)?;

    let lhs = s[..idx].trim();
    let rhs = s[idx + op.len()..].trim();

    if lhs.is_empty() || rhs.is_empty() {
        return None;
    }

    // Determine the "canonical" operator for null checks.
    // `===` and `!==` map to `==` and `!=` semantically for null comparisons.
    let canonical_op = match op {
        "===" => "==",
        "!==" => "!=",
        other => other,
    };

    // Case 1: var <op> null_keyword
    if is_identifier(lhs) && NULL_KEYWORDS.contains(&rhs) {
        return match canonical_op {
            "==" => Some(GuardCondition::IsNull {
                var: lhs.to_string(),
            }),
            "!=" => Some(GuardCondition::NotNull {
                var: lhs.to_string(),
            }),
            _ => None, // `x < null` doesn't make sense
        };
    }

    // Case 2: null_keyword <op> var (reversed null check)
    if NULL_KEYWORDS.contains(&lhs) && is_identifier(rhs) {
        return match canonical_op {
            "==" => Some(GuardCondition::IsNull {
                var: rhs.to_string(),
            }),
            "!=" => Some(GuardCondition::NotNull {
                var: rhs.to_string(),
            }),
            _ => None,
        };
    }

    // Case 3: var <op> int_literal
    if is_identifier(lhs) {
        if let Some(value) = parse_int_literal(rhs) {
            return make_guard(lhs, canonical_op, value);
        }
    }

    // Case 4: int_literal <op> var (reversed operand order)
    if is_identifier(rhs) {
        if let Some(value) = parse_int_literal(lhs) {
            // Flip the operator: `5 < x` means `x > 5`
            let flipped = flip_operator(canonical_op)?;
            return make_guard(rhs, flipped, value);
        }
    }

    None
}

/// Construct a [`GuardCondition`] from a variable name, canonical operator, and
/// integer value.
fn make_guard(var: &str, op: &str, value: i64) -> Option<GuardCondition> {
    let var = var.to_string();
    match op {
        "==" => Some(GuardCondition::Eq { var, value }),
        "!=" => Some(GuardCondition::Neq { var, value }),
        "<" => Some(GuardCondition::Lt { var, value }),
        "<=" => Some(GuardCondition::Le { var, value }),
        ">" => Some(GuardCondition::Gt { var, value }),
        ">=" => Some(GuardCondition::Ge { var, value }),
        _ => None,
    }
}

/// Extract the variable name from a [`GuardCondition`].
///
/// Every guard condition variant contains a `var` field identifying the
/// variable being tested. This helper extracts it.
pub fn guard_variable(guard: &GuardCondition) -> &str {
    match guard {
        GuardCondition::Eq { var, .. }
        | GuardCondition::Neq { var, .. }
        | GuardCondition::Lt { var, .. }
        | GuardCondition::Le { var, .. }
        | GuardCondition::Gt { var, .. }
        | GuardCondition::Ge { var, .. }
        | GuardCondition::NotNull { var }
        | GuardCondition::IsNull { var }
        | GuardCondition::Truthy { var }
        | GuardCondition::Falsy { var } => var.as_str(),
    }
}

/// Narrow an [`AbstractValue`] based on a guard condition and branch polarity.
///
/// `is_true_branch`: `true` means we are on the branch where the condition
/// holds, `false` means we are on the branch where it does NOT hold.
///
/// Returns a narrowed `AbstractValue` that is always a **subset** of the input
/// (soundness invariant: never removes concrete values that could actually
/// occur).
///
/// If narrowing produces an empty range (contradiction), returns
/// [`AbstractValue::bottom()`].
pub fn narrow_value(
    value: &AbstractValue,
    guard: &GuardCondition,
    is_true_branch: bool,
) -> AbstractValue {
    if !is_true_branch {
        // FALSE branch: negate the guard and apply as TRUE branch
        let complement = negate_guard(guard);
        return narrow_value(value, &complement, true);
    }

    // TRUE branch narrowing
    let mut result = value.clone();

    match guard {
        GuardCondition::Eq { value: c, .. } => {
            // x == c: range becomes [c, c] intersected with input
            result.range_ = intersect_range(value.range_, Some((Some(*c), Some(*c))));
            // Clear constant since we are narrowing, not propagating
        }
        GuardCondition::Neq { value: c, .. } => {
            // x != c: narrow when c is at a boundary of the range.
            //
            // Cases handled:
            // 1. Range is exactly [c, c] -> contradiction (bottom)
            // 2. c is the lower bound -> tighten to [c+1, hi]
            // 3. c is the upper bound -> tighten to [lo, c-1]
            // 4. c is interior to range or no range -> conservative (can't split)
            match value.range_ {
                Some((Some(lo), Some(hi))) if lo == *c && hi == *c => {
                    return AbstractValue::bottom();
                }
                Some((Some(lo), hi)) if lo == *c => {
                    if let Some(new_lo) = c.checked_add(1) {
                        result.range_ = Some((Some(new_lo), hi));
                    }
                }
                Some((lo, Some(hi))) if hi == *c => {
                    if let Some(new_hi) = c.checked_sub(1) {
                        result.range_ = Some((lo, Some(new_hi)));
                    }
                }
                _ => {
                    // c is interior to range or no range info: can't split
                    // intervals, leave unchanged (conservative).
                }
            }
        }
        GuardCondition::Lt { value: c, .. } => {
            // x < c: upper bound is c-1 (integer semantics)
            let upper = c.checked_sub(1);
            let guard_range = match upper {
                Some(u) => Some((None, Some(u))),
                None => {
                    // c is i64::MIN, c-1 overflows -> empty (nothing < i64::MIN)
                    return AbstractValue::bottom();
                }
            };
            result.range_ = intersect_range(value.range_, guard_range);
        }
        GuardCondition::Le { value: c, .. } => {
            // x <= c: upper bound is c
            let guard_range = Some((None, Some(*c)));
            result.range_ = intersect_range(value.range_, guard_range);
        }
        GuardCondition::Gt { value: c, .. } => {
            // x > c: lower bound is c+1
            let lower = c.checked_add(1);
            let guard_range = match lower {
                Some(l) => Some((Some(l), None)),
                None => {
                    // c is i64::MAX, c+1 overflows -> empty (nothing > i64::MAX)
                    return AbstractValue::bottom();
                }
            };
            result.range_ = intersect_range(value.range_, guard_range);
        }
        GuardCondition::Ge { value: c, .. } => {
            // x >= c: lower bound is c
            let guard_range = Some((Some(*c), None));
            result.range_ = intersect_range(value.range_, guard_range);
        }
        GuardCondition::NotNull { .. } => {
            result.nullable = Nullability::Never;
        }
        GuardCondition::IsNull { .. } => {
            result.nullable = Nullability::Always;
        }
        GuardCondition::Truthy { .. } => {
            // Truthy: not null AND not zero
            result.nullable = Nullability::Never;
            // Try to exclude 0 from range
            result.range_ = exclude_zero(value.range_);
        }
        GuardCondition::Falsy { .. } => {
            // Falsy: could be null OR zero. Conservative: don't narrow range,
            // set nullable to Maybe (it could be null).
            result.nullable = Nullability::Maybe;
        }
    }

    // Check for bottom after range narrowing
    if is_empty_range(result.range_) {
        return AbstractValue::bottom();
    }

    result
}

/// Narrow an entire [`AbstractState`] by applying a guard condition.
///
/// Only narrows the variable mentioned in the guard condition. Other variables
/// are passed through unchanged. If the variable is not in the state, the
/// state is returned unchanged.
pub fn narrow_state(
    state: &AbstractState,
    guard: &GuardCondition,
    is_true_branch: bool,
) -> AbstractState {
    let var = guard_variable(guard);
    let current = state.get(var);
    let narrowed = narrow_value(&current, guard, is_true_branch);
    state.set(var, narrowed)
}

/// Negate a guard condition (compute its complement).
///
/// Used for FALSE-branch narrowing: if we know the condition does NOT hold,
/// we apply the negated condition as if it DOES hold.
fn negate_guard(guard: &GuardCondition) -> GuardCondition {
    match guard {
        GuardCondition::Eq { var, value } => GuardCondition::Neq {
            var: var.clone(),
            value: *value,
        },
        GuardCondition::Neq { var, value } => GuardCondition::Eq {
            var: var.clone(),
            value: *value,
        },
        GuardCondition::Lt { var, value } => GuardCondition::Ge {
            var: var.clone(),
            value: *value,
        },
        GuardCondition::Le { var, value } => GuardCondition::Gt {
            var: var.clone(),
            value: *value,
        },
        GuardCondition::Gt { var, value } => GuardCondition::Le {
            var: var.clone(),
            value: *value,
        },
        GuardCondition::Ge { var, value } => GuardCondition::Lt {
            var: var.clone(),
            value: *value,
        },
        GuardCondition::NotNull { var } => GuardCondition::IsNull { var: var.clone() },
        GuardCondition::IsNull { var } => GuardCondition::NotNull { var: var.clone() },
        GuardCondition::Truthy { var } => GuardCondition::Falsy { var: var.clone() },
        GuardCondition::Falsy { var } => GuardCondition::Truthy { var: var.clone() },
    }
}

/// Intersect two ranges, producing their overlap.
///
/// Each range is `Option<(Option<i64>, Option<i64>)>` where:
/// - Outer `None` means "no range info" (treated as unbounded `(-inf, +inf)`)
/// - Inner `None` for lo/hi means unbounded in that direction
///
/// Returns `None` only if both inputs are `None`. Otherwise returns
/// `Some((lo, hi))` with the tighter of the two bounds.
fn intersect_range(
    a: Option<(Option<i64>, Option<i64>)>,
    b: Option<(Option<i64>, Option<i64>)>,
) -> Option<(Option<i64>, Option<i64>)> {
    match (a, b) {
        (None, None) => None,
        (None, Some(r)) | (Some(r), None) => Some(r),
        (Some((a_lo, a_hi)), Some((b_lo, b_hi))) => {
            let lo = match (a_lo, b_lo) {
                (None, None) => None,
                (Some(v), None) | (None, Some(v)) => Some(v),
                (Some(a), Some(b)) => Some(a.max(b)),
            };
            let hi = match (a_hi, b_hi) {
                (None, None) => None,
                (Some(v), None) | (None, Some(v)) => Some(v),
                (Some(a), Some(b)) => Some(a.min(b)),
            };
            Some((lo, hi))
        }
    }
}

/// Check if a range is empty (lo > hi).
///
/// An empty range represents a contradiction (unreachable state).
/// Unbounded sides (None) are never empty.
fn is_empty_range(range: Option<(Option<i64>, Option<i64>)>) -> bool {
    match range {
        None => false,
        Some((Some(lo), Some(hi))) => lo > hi,
        _ => false, // unbounded on either side -> not empty
    }
}

/// Exclude zero from a range if possible.
///
/// For `Truthy` narrowing: the value is known to be nonzero.
/// - If range is exactly [0, 0], returns bottom-range (empty).
/// - If lo == 0, bumps lo to 1.
/// - If hi == 0, bumps hi to -1.
/// - Otherwise returns unchanged (conservative: zero may be interior).
fn exclude_zero(range: Option<(Option<i64>, Option<i64>)>) -> Option<(Option<i64>, Option<i64>)> {
    match range {
        None => None, // no range info, can't narrow
        Some((lo, hi)) => {
            let lo_val = lo.unwrap_or(i64::MIN);
            let hi_val = hi.unwrap_or(i64::MAX);

            if lo_val == 0 && hi_val == 0 {
                // Exactly [0, 0] -> empty (contradiction handled by caller)
                Some((Some(1), Some(0))) // empty range: lo > hi
            } else if lo_val == 0 {
                // [0, hi] -> [1, hi]
                Some((Some(1), hi))
            } else if hi_val == 0 {
                // [lo, 0] -> [lo, -1]
                Some((lo, Some(-1)))
            } else {
                // Zero is interior to range, can't split intervals -> conservative
                Some((lo, hi))
            }
        }
    }
}
