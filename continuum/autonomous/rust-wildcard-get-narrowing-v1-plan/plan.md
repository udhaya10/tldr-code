# rust-wildcard-get-narrowing-v1 — Plan

**Status:** PLANNING (no source-code changes in this commit)
**Closes carry-forward:** rust-vuln-taint-pipeline-v1 premortem `dab0766` R8 (T2/E1 wildcard-get FP elephant) — quantified at M3 binary-smoke as 100% FP rate on synthetic `.get(<tainted>)` member-access callers (HashMap, Vec, BTreeMap).
**HEAD-at-planning:** `7a36df3` (rust-vuln-taint-pipeline-v1 M5 followup; 167/168 GREEN on `vuln_migration_v1_red`).

This is a **PRECISION** milestone — closes ZERO RED tests. It tightens the over-broad wildcard sink pattern that became LIVE on `.rs` files post `rust-vuln-taint-pipeline-v1` M2 (when the canonical pipeline started running on `.rs`).

---

## §0 Investigation summary — verified at HEAD `7a36df3`

### What the wildcard does today

`crates/tldr-core/src/security/taint.rs:2473-2488` (`RUST_AST_SINKS` HttpRequest entry) currently lists:

```rust
member_patterns: &[
    ("*", "get"),                                        // line 2476
    ("*", "post"),                                       // line 2477
    ("", "reqwest::get"),
    ("", "reqwest::Client"),
    ("", "reqwest::blocking::get"),                      // added by rust-vuln-taint-pipeline-v1 M2
    ("", "reqwest::blocking::Client"),                   // added by rust-vuln-taint-pipeline-v1 M2
    ("", "ureq::get"),
    ("", "ureq::post"),
    ("", "hyper::Client"),
    ("", "Url::parse"),
],
```

`member_patterns_match` (taint.rs:3856-3931) supports three matching shapes:

1. **Structural member-access** (`field_expression` / `member_expression`) — `(rcv_text, field_text)` extracted via `extract_member_access_receiver_and_field` at `crates/tldr-core/src/security/ast_utils.rs:533`. `pat_rcv == "*"` matches ANY receiver text.
2. **Call-shape with dotted name** (`request.getParameter(...)` as a single `call_expression` whose call-name text contains `.`) — split on last `.`; same wildcard semantics.
3. **Raw-substring fallback** for scoped/qualified module calls (`pat_rcv.is_empty()` AND `descendant_text.contains(pat_field)`).

**Critical finding:** `member_patterns_match` matches receiver **NAME-text**, NOT receiver **TYPE**. With `("*", "get")` it accepts ANY identifier on the LHS of `.get(...)` — `map.get(...)`, `v.get(...)`, `m.get(...)`, `opt.get(...)`, `client.get(...)`, etc.

### What is the FP rate?

From `rust-vuln-taint-pipeline-v1-plan/reports/M3-binary-smoke.json:39-87` (captured at SHA `8560ab9`, run via release binary post `rust-vuln-taint-pipeline-v1` M2):

| Synthetic Rust fixture                                                          | Ssrf findings | Verdict |
|---------------------------------------------------------------------------------|---------------|---------|
| `let key = ...env::var("KEY").unwrap(); let map: HashMap<...> = ...; map.get(&key);` | 3             | **FP**  |
| `let idx_str = ...env::var("IDX").unwrap(); let v: Vec<i32> = ...; v.get(idx);` | 2             | **FP**  |
| `let k = ...env::var("K").unwrap(); let m: BTreeMap<...> = ...; m.get(&k);`     | 3             | **FP**  |
| `let url = ...env::var("URL").unwrap(); reqwest::blocking::get(&url);`         | 3             | **TP** (matched by scoped-identifier raw-fallback, NOT wildcard) |
| `let p = ...env::var("P").unwrap(); std::fs::read_to_string(&p);`              | 5 (PathTraversal) | **TP** (FileOpen path; unrelated)        |

**FP rate on `.get(<tainted>)` member-access callers: 100% (3/3).**
**TP rate via wildcard alone: 0%** — the one TP is a scoped-identifier shape that matches via the raw-fallback path independently of the wildcard.

### Is the wildcard load-bearing for any GREEN test?

**No.** Verified by reading:

- `test_e2e_rust_ssrf_reqwest_get` at `crates/tldr-core/src/security/vuln.rs:1184-1193` uses `reqwest::get(target)` — scoped-identifier, matched via `("", "reqwest::get")` raw-fallback.
- `crates/tldr-cli/tests/fixtures/vuln_migration_v1/rust/ssrf_positive.rs` uses `reqwest::blocking::get(&u)` — scoped-identifier, matched via `("", "reqwest::blocking::get")` raw-fallback.
- `crates/tldr-cli/tests/fixtures/vuln_migration_v1/rust/ssrf_string_literal_fp.rs` lacks env_var source so wildcard match is irrelevant; emits zero.
- No other Rust SSRF e2e test or fixture exists in the corpus.

Therefore narrowing the wildcards is a **non-regression** change for the test suite. See §4 for the explicit RED test contract.

### Does Option B (excludelist) work?

**No.** The receiver passed to `member_patterns_match` is the **variable NAME text**, not the type name. `let m: HashMap<String,i32> = ...; m.get(&k)` has receiver text `"m"` — an excludelist on `["HashMap", "Vec", "Option", "BTreeMap"]` would not match. To make Option B work, we'd need either:
(a) a receiver-name excludelist enumerating `m`, `map`, `v`, `vec`, `cache`, `dict`, `lookup`, `idx`, `opt`, etc. — open-ended and not maintainable, OR
(b) tree-sitter type-walk inference to resolve `m`'s declared type to `HashMap` — Option C, out of scope.

Therefore Option B is **rejected** and Option A (allowlist of HTTP-client receiver names) is the cleanest closing path within this milestone's scope.

---

## §1 Bundle scope — RECOMMENDED OPTION A: receiver-NAME allowlist

### Recommended design: replace 2 wildcards with 10 explicit allowlist entries

**Pre-M2 (HEAD 7a36df3):**
```rust
member_patterns: &[
    ("*", "get"),
    ("*", "post"),
    ("", "reqwest::get"),
    ("", "reqwest::Client"),
    ("", "reqwest::blocking::get"),
    ("", "reqwest::blocking::Client"),
    ("", "ureq::get"),
    ("", "ureq::post"),
    ("", "hyper::Client"),
    ("", "Url::parse"),
],
```

**Post-M2 (target):**
```rust
member_patterns: &[
    // Receiver-NAME allowlist for HTTP-client member-access shapes —
    // narrowed from the over-broad ("*", "get") / ("*", "post") wildcards
    // per rust-wildcard-get-narrowing-v1 M2 (closes premortem dab0766 R8).
    // Idiomatic Rust HTTP usage binds clients to `client` / `agent` / `http`
    // / `request_builder` / `req`; entries below cover those conventions.
    // Does NOT cover short variable names (`c`, `r`) or composed accesses
    // (`self.client.get`); documented residual gap in the milestone CHANGELOG.
    ("client", "get"),
    ("client", "post"),
    ("agent", "get"),
    ("agent", "post"),
    ("http", "get"),
    ("http", "post"),
    ("request_builder", "get"),
    ("request_builder", "post"),
    ("req", "get"),
    ("req", "post"),
    // Scoped-identifier raw-fallback paths — UNCHANGED.
    ("", "reqwest::get"),
    ("", "reqwest::Client"),
    ("", "reqwest::blocking::get"),
    ("", "reqwest::blocking::Client"),
    ("", "ureq::get"),
    ("", "ureq::post"),
    ("", "hyper::Client"),
    ("", "Url::parse"),
],
```

### Why Option A (vs B/C/D)

| Option | Verdict | Rationale |
|--------|---------|-----------|
| **A — explicit receiver-name allowlist** | **RECOMMENDED** | Eliminates 100% of synthetic FPs. Preserves load-bearing real-world idioms (`client.get`, `agent.get`). Smallest LOC delta. Additive AND narrowing — no enum changes, no struct changes. Residual gap (`c.get`, `self.client.get`) acceptable; documented carry-forward. |
| B — receiver excludelist | REJECTED | Receiver text is the variable NAME, not type. Excludelist on type names doesn't match `let m: HashMap = ...; m.get()`. Excludelist on variable names is open-ended. |
| C — type-aware receiver filter | OUT OF SCOPE | Requires tree-sitter type-walk inference; +200-500 LOC; cross-cutting impact on other languages' wildcard semantics. Layer atop Option A in a future milestone. |
| D — remove wildcards entirely | REJECTED | Loses load-bearing real-world idioms (`let client = reqwest::Client::new(); client.get(&url)`). Larger residual gap than Option A. |

### What Option A explicitly preserves

- All 4 currently-GREEN Rust positives (`rust_command_injection_positive`, `rust_path_traversal_positive`, `rust_ssrf_positive`, `rust_deserialization_positive`) — they don't depend on wildcards.
- All 4 string-literal FP regression-guards (`rust_*_string_literal_fp`) — they don't depend on wildcards (no env_var sources).
- All 9 `test_analyze_rust_*` unit tests — they call `analyze_rust_file` directly, decoupled from the canonical pipeline.
- All 36 `test_e2e_*` tests in `tldr-core/security/vuln.rs` — they use scoped-identifier shapes for Rust SSRF.
- Real-world idioms `let client = reqwest::Client::new(); client.get(&url)`, `let agent = ureq::agent(); agent.get(...)`, `http.get(...)`, `req.get(...)` — covered by new allowlist entries.

### What Option A explicitly does NOT preserve (documented residual gaps)

- Short-variable-name HTTP calls: `let c = reqwest::Client::new(); c.get(&url)` — receiver text `"c"` not in allowlist. Documented carry-forward.
- Composed-access HTTP calls: `self.client.get(&url)` (in methods) — receiver text `"self.client"` not in allowlist. Documented carry-forward.
- Custom-named HTTP clients: `let github = reqwest::Client::new(); github.get(&url)` — receiver text `"github"` not in allowlist. Documented carry-forward.

These gaps are acceptable for v1: (1) idiomatic Rust HTTP code overwhelmingly uses `client` / `agent`; (2) scoped-identifier paths still cover module-call forms; (3) future Option C work can layer type-aware filtering without conflicting with the allowlist.

---

## §2 Sub-milestone list

3 milestones. M2 is the atomic narrowing; M1 is RED-confirm + FP-baseline; M3 is CHANGELOG + tag.

### M1 — RED-confirm + synthetic FP-baseline capture

- **depends:** []
- **atomic_commit:** false
- **files_modified:**
  - `continuum/autonomous/rust-wildcard-get-narrowing-v1-plan/reports/M1-report.json`
  - `continuum/autonomous/rust-wildcard-get-narrowing-v1-plan/reports/M1-baseline-fp.json`
  - `continuum/autonomous/rust-wildcard-get-narrowing-v1-plan/reports/M1-green-capture.txt`
- **scope:**
  - Run `cargo test -p tldr-cli --release --test vuln_migration_v1_red rust_` at HEAD; capture all 4 positives + 4 string-literal-FPs GREEN (167/168 baseline).
  - Run `cargo test -p tldr-cli --release --lib commands::remaining::vuln::tests` capturing 9/9 GREEN.
  - Run all 36 `test_e2e_*` in `tldr-core/security/vuln.rs`.
  - Build release binary with `--features semantic`.
  - Construct 6 ad-hoc synthetic Rust fixtures (NOT added to test corpus); run `tldr vuln <fixture> --lang rust --format json` on each:
    - **(i)** HashMap::get on tainted key → expect ≥1 Ssrf finding (FP, pre-M2 baseline)
    - **(ii)** Vec::get on tainted index → expect ≥1 Ssrf finding (FP, pre-M2 baseline)
    - **(iii)** BTreeMap::get on tainted key → expect ≥1 Ssrf finding (FP, pre-M2 baseline)
    - **(iv)** `let client = reqwest::Client::new(); client.get(&url)` on tainted url → expect ≥1 Ssrf finding (TP, load-bearing real-world idiom)
    - **(v)** `let agent = ureq::agent(); agent.get(&url)` on tainted url → expect ≥1 Ssrf finding (TP, load-bearing real-world idiom)
    - **(vi)** `reqwest::blocking::get(&url)` on tainted url → expect ≥1 Ssrf finding (TP, scoped-identifier path)
- **stop_thresholds:** see `dispatch-contract.json` M1 stop_thresholds.

### M2 — ATOMIC narrow wildcards + verify

- **depends:** [M1]
- **atomic_commit:** TRUE — wildcard removal + 10-entry allowlist addition + doc-comment update ship in ONE commit. Splitting risks an intermediate SHA where wildcards are removed but allowlist is missing → regresses real-world `client.get` coverage between commits.
- **files_modified:**
  - `crates/tldr-core/src/security/taint.rs` (RUST_AST_SINKS HttpRequest member_patterns + doc-comment update at lines 2459-2488)
  - `continuum/autonomous/rust-wildcard-get-narrowing-v1-plan/reports/M2-report.json`
  - `continuum/autonomous/rust-wildcard-get-narrowing-v1-plan/reports/M2-green-capture.txt`
- **scope:** §1 narrowing + post-M2 binary-smoke on the same 6 synthetic fixtures from M1. Verify (i)/(ii)/(iii) drop to 0 Ssrf findings, (iv)/(v) stay ≥1, (vi) stays ≥1.
- **green_files (must remain GREEN):** all 4 Rust positives + 4 FP regression-guards + 9 `test_analyze_rust_*` + 36 `test_e2e_*` + workspace-wide `cargo test --workspace`.
- **loc_delta:** +8 LOC source code (10 new entries - 2 removed entries; ~5 LOC doc-comment update).
- **stop_thresholds:** see `dispatch-contract.json` M2 stop_thresholds.
- **rollback:** `git revert <M2_sha>` reverts wildcards + allowlist atomically. If real-world receiver names beyond the allowlist surface as needed, ADD them in a follow-on M4 (additive); do NOT re-introduce the wildcard.

### M3 — CHANGELOG + local tag

- **depends:** [M2]
- **atomic_commit:** false
- **files_modified:**
  - `CHANGELOG.md`
  - `continuum/autonomous/rust-wildcard-get-narrowing-v1-plan/reports/M3-report.json`
  - `continuum/autonomous/rust-wildcard-get-narrowing-v1-plan/reports/M3-tags-state.json`
- **scope:** §6 CHANGELOG draft. Apply local tag `rust-wildcard-get-narrowing-v1` at the M3 CHANGELOG commit SHA.
- **stop_thresholds:** CHANGELOG merged; `cargo build --workspace` PASS; local tag applied; no push, no publish, no version bump.

---

## §3 Design decision — WHY Option A

Decision matrix above (§1). Empirical justification:

1. **FP elimination:** 100% on the 3 synthetic FP fixtures captured by `rust-vuln-taint-pipeline-v1` M3 (HashMap/Vec/BTreeMap.get on tainted). Zero FPs post-M2 because none of those receiver names (`map`, `v`, `m`) are in the allowlist `{client, agent, http, request_builder, req}`.
2. **Load-bearing preservation:** Real-world reqwest convention is `let client = reqwest::Client::new();`; ureq is `let agent = ureq::agent();`; hyper is often `let http = ...`. The allowlist is hand-picked from these conventions, ensuring `<canonical-name>.get(<url>)` continues to detect Ssrf.
3. **Non-regression on GREEN tests:** §0 verified. The wildcard is dead-load-bearing — providing zero TP coverage on the test suite while emitting 100% FPs on synthetic real-world-shaped fixtures.
4. **Additive AND narrowing:** Removed match-set is the wildcard's universe; added match-set is a STRICT SUBSET of that universe. No new sinks introduced.
5. **No enum / struct / public-API changes.**

---

## §4 RED test contract

### Currently-GREEN tests that MUST remain GREEN

`crates/tldr-cli/tests/vuln_migration_v1_red.rs` — 167/168 baseline at HEAD `7a36df3`:

| Test | Line | Pre-M2 status | Post-M2 status |
|------|------|---------------|----------------|
| rust_command_injection_positive | 1003 | GREEN | GREEN |
| rust_path_traversal_positive | 1417 | GREEN | GREEN |
| rust_ssrf_positive | 1647 | GREEN | GREEN (uses scoped-identifier path) |
| rust_deserialization_positive | 1923 | GREEN | GREEN |
| rust_command_injection_string_literal_fp | 1014 | GREEN | GREEN |
| rust_path_traversal_string_literal_fp | 1428 | GREEN | GREEN |
| rust_ssrf_string_literal_fp | 1658 | GREEN | GREEN |
| rust_deserialization_string_literal_fp | 1934 | GREEN | GREEN |

Plus all 36 `test_e2e_*` in `crates/tldr-core/src/security/vuln.rs` and all 9 `test_analyze_rust_*` in `crates/tldr-cli/src/commands/remaining/vuln.rs:925-1037`.

### Synthetic fixtures (NOT added to test corpus; M1/M2 binary-smoke ONLY)

| Fixture id | Source (one-liner) | Pre-M2 expected | Post-M2 expected | Class |
|-----------|--------------------|-----------------|------------------|-------|
| (i) hashmap_get_fp | `let k=env::var("K").unwrap(); let m: HashMap<...>=...; m.get(&k);` | ≥1 Ssrf | **0 Ssrf** | FP-elimination |
| (ii) vec_get_fp | `let i_s=env::var("I").unwrap(); let v: Vec<i32>=...; v.get(i);` | ≥1 Ssrf | **0 Ssrf** | FP-elimination |
| (iii) btreemap_get_fp | `let k=env::var("K").unwrap(); let m: BTreeMap<...>=...; m.get(&k);` | ≥1 Ssrf | **0 Ssrf** | FP-elimination |
| (iv) client_get_tp | `let url=env::var("URL").unwrap(); let client=reqwest::Client::new(); client.get(&url);` | ≥1 Ssrf | **≥1 Ssrf** | Load-bearing TP |
| (v) agent_get_tp | `let url=env::var("URL").unwrap(); let agent=ureq::agent(); agent.get(&url);` | ≥1 Ssrf | **≥1 Ssrf** | Load-bearing TP |
| (vi) reqwest_blocking_get_tp | `let url=env::var("URL").unwrap(); reqwest::blocking::get(&url);` | ≥1 Ssrf | **≥1 Ssrf** | Scoped-identifier (unchanged) |

**Quantification:** pre-M2 FP rate = 100% (3/3). Post-M2 FP rate = 0% (0/3). +100 percentage-point precision improvement on this synthetic class.

---

## §5 Risk register

### R1 — Real-world reqwest user binds client to non-allowlist name (TIGER, MEDIUM)

**Failure mode:** A user writes `let github = reqwest::Client::new(); github.get(&url);` — post-M2 the wildcard no longer matches `github.get`, and `github` is not in the allowlist → Ssrf no longer detected on this real-world idiom.

**Mitigation:** (a) Idiomatic Rust HTTP code overwhelmingly uses `client` (per the `reqwest` README and `ureq` docs); custom-named clients are minority. (b) The scoped-identifier `("", "reqwest::Client")` entry still fires on the constructor expression itself if it's tainted — partial coverage. (c) Documented carry-forward in CHANGELOG + M3 report; future Option C type-aware filter can layer atop the allowlist without conflict.

### R2 — Composed-access `self.client.get(...)` regresses (TIGER, LOW-MEDIUM)

**Failure mode:** Inside a method body, `self.client.get(&url)` — receiver text is `self.client` (or possibly the AST yields `self` and field=`client`, which then doesn't match either). Wildcard previously matched `field=="get"`; allowlist `("client", "get")` requires receiver text exactly `"client"`.

**Mitigation:** Verify behavior at M2 binary-smoke (worker constructs an additional ad-hoc fixture to test). If `self.client.get` is treated as receiver `self.client` field `get`, allowlist misses. Document as carry-forward; route to follow-on Option C work. Acceptable because: (1) `self.client` constructions almost always involve a `reqwest::Client::new()` somewhere upstream — that scoped-identifier path may already produce findings on the construction-site line; (2) `tldr taint` (the lower-level command) will still trace data flows even if `tldr vuln` SSRF detection drops; (3) precision over recall is the explicit milestone goal.

### R3 — Allowlist entries don't match because of call-shape vs member-access nuance (TIGER, LOW)

**Failure mode:** `client.get(&url)` is parsed as a `call_expression` whose call-name is `"client.get"` — handled by the call-shape path at taint.rs:3897-3917 which splits on last `.`. Verified at planning: the structural-match path at L3863 takes the wildcard or allowlist entry; the call-shape path at L3898 uses identical `pat_rcv == "*"` semantics. Both paths support the new allowlist entries identically.

**Mitigation:** Verify via M1 baseline that `client.get(...)` produces ≥1 Ssrf finding pre-M2 (it must, since wildcard fires); verify post-M2 that it STILL produces ≥1 finding (allowlist `("client", "get")` matches the same shape). M2 stop_threshold (iv)/(v) gates this empirically.

### R4 — Behavior change is not a public-API break, but consumers may surface tickets (ELEPHANT, LOW)

**Failure mode:** A user pinned to "tldr vuln on Rust files emits N Ssrf findings" sees N drop post-M2. Surfaces as user-reported "regression" even though it's a precision improvement.

**Mitigation:** CHANGELOG entry §6 explicitly flags the FP-rate reduction and quantifies it (3/3 synthetic FPs eliminated). Consumers using exhaustive vuln_type aggregation handle this transparently.

### R5 — Doc-comment drift causes confusion in future milestones (TIGER, LOW)

**Failure mode:** Updating the doc-comment at taint.rs:2459-2472 must also retire the wildcard-receiver remark; if drift, future maintainers re-introduce wildcards thinking they're missing.

**Mitigation:** M2 explicitly updates the doc-comment per §1 spec; diff verification in M2 stop_threshold.

---

## §6 CHANGELOG draft

```markdown
## rust-wildcard-get-narrowing-v1 — internal milestone

### Changed

- **vuln** (Rust): `RUST_AST_SINKS` HttpRequest member_patterns narrowed —
  the wildcard entries `("*", "get")` and `("*", "post")` (which fired on
  ANY `<receiver>.get(<tainted>)` / `.post(<tainted>)` member-access
  shape) are replaced with an explicit allowlist of HTTP-client receiver
  names: `client`, `agent`, `http`, `request_builder`, `req`. This
  eliminates the 100% false-positive rate on `HashMap::get` /
  `Vec::get` / `BTreeMap::get` / `Option::get` on tainted args
  surfaced post `rust-vuln-taint-pipeline-v1` M2 (premortem `dab0766`
  R8). Real-world idioms like `let client = reqwest::Client::new();
  client.get(&url)` continue to be detected via the new allowlist
  entries. Scoped-identifier paths (`reqwest::get`, `reqwest::Client`,
  `reqwest::blocking::get`, `reqwest::blocking::Client`, `ureq::get`,
  `ureq::post`, `hyper::Client`, `Url::parse`) are UNCHANGED.

### Known residual gaps (out of scope; documented carry-forward)

- HTTP clients bound to short variable names (`let c = reqwest::Client::new(); c.get(&url)`)
  no longer trigger Ssrf detection on the member-access shape.
- Composed-access HTTP calls (`self.client.get(&url)` inside methods)
  may not match — the receiver text is composed, not a single
  identifier in the allowlist.
- Custom-named HTTP clients (`let github = reqwest::Client::new();
  github.get(&url)`) require additional allowlist entries OR future
  type-aware receiver filtering (out of scope here).

These residual gaps are accepted in exchange for eliminating the
universal `.get(<tainted>)` false-positive class. Future
`rust-wildcard-receiver-type-aware-v1` work (not yet planned) can
layer tree-sitter type-walk inference atop the allowlist.
```

---

## §7 Self-validation — validator_mandates

```yaml
validator_mandates:
  no_regression_on_4_rust_red_tests: |
    All 4 Rust positives closed by rust-vuln-taint-pipeline-v1 M2
    (rust_command_injection_positive, rust_path_traversal_positive,
    rust_ssrf_positive, rust_deserialization_positive) MUST stay
    GREEN at every milestone gate. rust_ssrf_positive uses
    `reqwest::blocking::get(&u)` — a scoped-identifier shape matched
    via the raw-fallback `("", "reqwest::blocking::get")` entry,
    untouched by this narrowing. The 4 string-literal-FP regression
    guards (rust_*_string_literal_fp) STAY GREEN — they lack env_var
    sources entirely. Verified by reading the fixture files at
    planning time.

  no_public_api_change: |
    AstSinkPattern struct shape unchanged. VulnFinding shape
    unchanged. tldr_core::security::vuln::scan_vulnerabilities
    signature unchanged. Public-API surface binary-compatible
    pre/post M2.

  no_new_enum_variants: |
    VulnType, TaintSinkType, TaintSourceType — no new variants. The
    narrowing operates entirely within the static
    RUST_AST_SINKS HttpRequest member_patterns array (a `&[(&str,
    &str)]`).

  additive_OR_narrowing_only_no_loosening: |
    Pre-M2 wildcard match universe: { all member-access pairs
    (rcv, "get") + (rcv, "post") for any receiver text }. Post-M2
    allowlist match universe: { (client/agent/http/request_builder/
    req) × (get/post) }. Post-M2 set is a STRICT SUBSET of pre-M2
    set. No NEW pairs introduced. NO LOOSENING.

  atomic_narrowing_commit: |
    M2 ships the 2-line wildcard removal + 10-line allowlist addition
    + ~5-line doc-comment update in a SINGLE commit. Splitting risks
    an intermediate SHA where wildcards are removed but allowlist is
    missing — would regress `client.get(&url)` real-world coverage
    between commits.

  synthetic_fp_quantification_pre_post: |
    M1 captures pre-M2 finding counts on 6 ad-hoc synthetic fixtures
    (3 FP shapes + 3 TP shapes); M2 verifies post-M2 0 findings on
    the 3 FP shapes + ≥1 finding on each of the 3 TP shapes. The
    pre/post FP rate (100% → 0%) is recorded in M2-report.json.

  fixtures_not_added_to_test_corpus: |
    Synthetic fixtures used for M1/M2 quantification are CONSTRUCTED
    ad-hoc and run via the binary; NOT added to
    crates/tldr-cli/tests/fixtures/ NOR to vuln_migration_v1_red.rs.
    Adds zero LOC to the test suite.

  e2e_test_preservation_mandatory: |
    All 36 test_e2e_* tests in tldr-core/security/vuln.rs STAY GREEN.
    Specifically test_e2e_rust_ssrf_reqwest_get (vuln.rs:1184) STAYS
    GREEN — uses `reqwest::get(target)` (scoped-identifier).

  test_analyze_rust_unit_tests_preserved: |
    All 9 #[test] fns in commands::remaining::vuln::tests module
    STAY GREEN. They call analyze_rust_file directly; do not
    exercise the canonical taint pipeline.

  staging_method_explicit: |
    Each commit stages exactly the listed files via `git add
    <path>...`. NO `git add -A` / `git add .`. Cargo.lock NEVER
    staged (`git checkout HEAD -- Cargo.lock` if dirty).

  no_push_no_publish_no_version_bump: |
    All milestones internal. Local tag
    `rust-wildcard-get-narrowing-v1` only at M3. NO `cargo publish`,
    NO `git push`, NO version bump in Cargo.toml. USER STANDING RULE.
```

---

## §8 /autonomous-readiness

**No premortem required.** This is a precision narrowing that closes ZERO RED tests and operates entirely within a static array. The risk surface is minimal:

- Risk register §5 documents 5 risks (R1-R5), all LOW or MEDIUM, all with concrete mitigations.
- The change is mechanical: 2 lines removed, 10 lines added, ~5 lines doc-comment update.
- M1 baseline captures every relevant pre-M2 state empirically; M2 verifies post-M2 on the same set.
- No cross-cutting impact (RUST_AST_SINKS is Rust-only by construction).

### Conditions to declare ready

- [x] §0 investigation summary verified empirically (taint.rs:2473-2488 read; member_patterns_match read; FP rate from rust-vuln-taint-pipeline-v1 M3 binary smoke; load-bearing analysis confirmed wildcard provides zero TP coverage on test suite)
- [x] §1 design choice locked (Option A — receiver-NAME allowlist)
- [x] §2 milestones enumerate 3 sub-milestones with stop_thresholds
- [x] §3 design decision matrix tabulates A/B/C/D rejected/accepted
- [x] §4 RED test contract identifies the 4 Rust positives + 4 FP-guards + 9 unit tests + 36 e2e tests as mandatory-GREEN; 6 synthetic fixtures defined
- [x] §5 risk register identifies 5 risks with mitigations
- [x] §6 CHANGELOG draft prepared with explicit residual-gap documentation
- [x] §7 validator_mandates declared
- [x] **CONDITION:** verified `("*", "get")` / `("*", "post")` are NOT load-bearing for any GREEN test (read fixtures + e2e tests at planning time)
- [x] **CONDITION:** verified `member_patterns_match` semantics (receiver-NAME-text match, not type match)
- [x] **CONDITION:** FP quantification numbers sourced from `rust-vuln-taint-pipeline-v1-plan/reports/M3-binary-smoke.json` (in-repo evidence, not speculation)

Declared **/autonomous-ready** for autonomous M2 execution. Recommended worker: kraken (Opus, full implementation context — though the change is small enough that a single-pass kraken invocation should suffice).
