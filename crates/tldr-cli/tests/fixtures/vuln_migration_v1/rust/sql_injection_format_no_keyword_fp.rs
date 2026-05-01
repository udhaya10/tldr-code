//! vt=SqlInjection lang=rust — FP regression-guard (rust-format-sql-fp-narrowing-v1).
//!
//! Empirical pre-fix repro on `tldr vuln --lang rust /tmp/repos/ripgrep/crates`
//! produced 4 critical-severity SqlInjection findings on `format!()` callsites
//! with ZERO SQL anywhere in the file. Root cause: the `contains_sql_keyword`
//! predicate uppercased the WHOLE line, causing `char::from(` and
//! `Box::<...>::from(format!(...))` to substring-match keyword `FROM`.
//!
//! This fixture exercises the three independent pre-fix FP shapes:
//!   T1. `format!("-{}", char::from(short))` (bash/fish/powershell flag fmt)
//!   T2. `Box::<...>::from(format!($($tt)*))` (err! macro pass-through)
//!   T3. `format!("count = {}", n)` (plain interpolation, NO SQL keyword)
//!
//! Post-fix, the line scanner's SqlInjection trigger MUST emit ZERO
//! findings on this fixture. Other unrelated findings (e.g. UnsafeCode,
//! MemorySafety) are also absent here — the fixture is intentionally
//! kept minimal to make the `all_findings(...).is_empty()` assertion
//! sound.

pub fn bash_flag(short: u8) -> String {
    format!("-{}", char::from(short))
}

pub fn fish_flag(byte: u8) -> String {
    format!("-s {}", char::from(byte))
}

pub fn powershell_flag(byte: u8) -> String {
    format!("-{}", char::from(byte))
}

pub fn err_macro_passthrough(msg: &str) -> Box<dyn std::error::Error + Send + Sync> {
    Box::<dyn std::error::Error + Send + Sync>::from(format!("error: {}", msg))
}

pub fn plain_interp(n: usize) -> String {
    format!("count = {}", n)
}
