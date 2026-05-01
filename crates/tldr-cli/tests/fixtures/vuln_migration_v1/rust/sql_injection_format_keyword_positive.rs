//! vt=SqlInjection lang=rust — positive (TP guard for
//! rust-format-sql-fp-narrowing-v1).
//!
//! `format!()` with a SQL keyword in the format-string literal MUST still
//! fire the SqlInjection finding from `analyze_rust_file`. This is the
//! true-positive guard: the FP narrowing milestone tightens the predicate
//! from "line contains a SQL keyword as substring" to "format-string
//! literal contains a SQL keyword as a word", and this fixture asserts
//! the tightening did NOT close the legitimate detection case.

pub fn fetch(id: &str) -> String {
    format!("SELECT * FROM users WHERE id = {}", id)
}
