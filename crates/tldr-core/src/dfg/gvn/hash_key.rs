//! HashKey - Structured Keys for GVN Hashing
//!
//! MIT-HASH-01b Mitigation: Use structured enum instead of string concatenation
//! to prevent collision issues like "binop:Add:ab:c" vs "binop:Add:a:bc".
//!
//! # Problem
//!
//! String-based hash keys like `format!("binop:{}:{}:{}", op, left, right)` can
//! cause false collisions when operand names contain the delimiter. For example:
//! - `binop:Add:a:bc` (a + bc)
//! - `binop:Add:ab:c` (ab + c)
//!
//! These would hash differently with strings but could collide if names are
//! manipulated incorrectly.
//!
//! # Solution
//!
//! Use a structured enum that derives Hash, ensuring each component is hashed
//! separately without delimiter confusion.

use std::cmp::Ordering;

/// Structured hash keys for GVN expressions.
///
/// Use this enum instead of string concatenation to ensure collision-free hashing.
/// The enum derives Hash, Eq, and PartialEq, so it can be used directly as a
/// HashMap key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HashKey {
    /// Constant value with type and representation
    Const {
        /// The type of the constant (e.g., "int", "str", "float").
        type_name: String,
        /// The string representation of the constant value (e.g., "42", "hello").
        repr: String,
    },

    /// Variable reference by value number (already resolved)
    VarVN {
        /// The GVN value number assigned to this variable.
        vn: usize,
    },

    /// Unresolved variable name (for parameters and initial references)
    Name {
        /// The source-level variable name before value numbering.
        name: String,
    },

    /// Binary operation with operator and operands
    /// If commutative=true, operands are normalized (sorted)
    BinOp {
        /// The binary operator (e.g., "Add", "Sub", "Mult").
        op: String,
        /// The left operand of the binary expression.
        left: Box<HashKey>,
        /// The right operand of the binary expression.
        right: Box<HashKey>,
        /// Whether the operands have been normalized for commutativity.
        commutative: bool,
    },

    /// Unary operation (e.g., -x, not x, ~x)
    UnaryOp {
        /// The unary operator (e.g., "USub", "Not", "Invert").
        op: String,
        /// The operand of the unary expression.
        operand: Box<HashKey>,
    },

    /// Boolean operation (and/or with multiple operands)
    BoolOp {
        /// The boolean operator ("And" or "Or").
        op: String,
        /// The operands of the boolean expression.
        operands: Vec<HashKey>,
    },

    /// Comparison expression (e.g., a < b < c)
    /// Parts are stored as strings for simplicity
    Compare {
        /// The comparison chain parts as strings (operators and operands interleaved).
        parts: Vec<String>,
    },

    /// Function/method call - always unique (conservative)
    /// Each call gets a unique ID since calls may have side effects
    Call {
        /// Monotonically increasing ID ensuring each call site is treated as unique.
        unique_id: usize,
    },

    /// Attribute access (obj.attr)
    Attribute {
        /// The base object expression being accessed.
        value: Box<HashKey>,
        /// The attribute name being accessed on the object.
        attr: String,
    },

    /// Subscript access (obj[key])
    Subscript {
        /// The base object expression being subscripted.
        value: Box<HashKey>,
        /// The subscript key expression.
        slice: Box<HashKey>,
    },

    /// Unique marker for expressions that should never be equivalent
    /// Used for depth-limited expressions and other special cases
    Unique {
        /// Monotonically increasing ID ensuring this expression never matches another.
        id: usize,
    },
}

// =============================================================================
// Commutativity Support
// =============================================================================

/// Returns true if the given binary operator is commutative.
///
/// Commutative operators have the property that `a op b == b op a`:
/// - Add: a + b == b + a
/// - Mult: a * b == b * a
/// - BitOr: a | b == b | a
/// - BitAnd: a & b == b & a
/// - BitXor: a ^ b == b ^ a
///
/// Non-commutative operators (Sub, Div, Mod, Pow, LShift, RShift, etc.)
/// do NOT satisfy this property.
pub fn is_commutative(op: &str) -> bool {
    matches!(
        op,
        // Python operator enum names (backward compat)
        "Add" | "Mult" | "BitOr" | "BitAnd" | "BitXor" |
        // Raw operator text (multi-language universal)
        "+" | "*" | "|" | "&" | "^" |
        // Equality (commutative across all languages)
        "==" | "!=" |
        // Boolean (Python-style)
        "and" | "or" |
        // Boolean (C-style)
        "&&" | "||"
    )
}

/// Normalize a binary operation by sorting operands for commutative operators.
///
/// For commutative operations, we sort the operands to ensure that
/// `a + b` and `b + a` produce the same HashKey.
///
/// For non-commutative operations, operand order is preserved.
pub fn normalize_binop(op: &str, left: HashKey, right: HashKey) -> HashKey {
    let commutative = is_commutative(op);

    let (normalized_left, normalized_right) = if commutative {
        // Sort operands using a consistent ordering
        match compare_hash_keys(&left, &right) {
            Ordering::Greater => (right, left),
            _ => (left, right),
        }
    } else {
        (left, right)
    };

    HashKey::BinOp {
        op: op.to_string(),
        left: Box::new(normalized_left),
        right: Box::new(normalized_right),
        commutative,
    }
}

/// Compare two HashKeys for normalization ordering.
///
/// This provides a consistent total ordering for HashKey variants,
/// used to normalize commutative operations.
fn compare_hash_keys(a: &HashKey, b: &HashKey) -> Ordering {
    // Use discriminant first, then compare fields
    match (a, b) {
        (
            HashKey::Const {
                type_name: t1,
                repr: r1,
            },
            HashKey::Const {
                type_name: t2,
                repr: r2,
            },
        ) => t1.cmp(t2).then_with(|| r1.cmp(r2)),
        (HashKey::VarVN { vn: v1 }, HashKey::VarVN { vn: v2 }) => v1.cmp(v2),
        (HashKey::Name { name: n1 }, HashKey::Name { name: n2 }) => n1.cmp(n2),
        (
            HashKey::BinOp {
                op: o1,
                left: l1,
                right: r1,
                ..
            },
            HashKey::BinOp {
                op: o2,
                left: l2,
                right: r2,
                ..
            },
        ) => o1
            .cmp(o2)
            .then_with(|| compare_hash_keys(l1, l2))
            .then_with(|| compare_hash_keys(r1, r2)),
        (
            HashKey::UnaryOp {
                op: o1,
                operand: op1,
            },
            HashKey::UnaryOp {
                op: o2,
                operand: op2,
            },
        ) => o1.cmp(o2).then_with(|| compare_hash_keys(op1, op2)),
        (HashKey::Call { unique_id: u1 }, HashKey::Call { unique_id: u2 }) => u1.cmp(u2),
        (HashKey::Unique { id: u1 }, HashKey::Unique { id: u2 }) => u1.cmp(u2),
        // Fall back to discriminant ordering for different variants
        _ => discriminant_order(a).cmp(&discriminant_order(b)),
    }
}

/// Get a numeric discriminant for ordering different HashKey variants
fn discriminant_order(key: &HashKey) -> u8 {
    match key {
        HashKey::Const { .. } => 0,
        HashKey::VarVN { .. } => 1,
        HashKey::Name { .. } => 2,
        HashKey::BinOp { .. } => 3,
        HashKey::UnaryOp { .. } => 4,
        HashKey::BoolOp { .. } => 5,
        HashKey::Compare { .. } => 6,
        HashKey::Call { .. } => 7,
        HashKey::Attribute { .. } => 8,
        HashKey::Subscript { .. } => 9,
        HashKey::Unique { .. } => 10,
    }
}

// =============================================================================
// Tests
// =============================================================================
