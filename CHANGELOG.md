# Changelog

## v0.3.0 — 2026-04-29

**MAJOR engine restructure release.**

### Engine

- **process_block taint propagation** rewired from substring matching to
  VarRef-based per-line use lookup (M1a). Eliminates the variable-shadowing
  false-positive class — short variable names like `x`, `i`, `db` no
  longer match unrelated tokens. Substring predicate at taint.rs:3761 (Definition
  arm) and :3780 (Update arm) replaced with `rhs_uses_tainted` helper.
- **SSA-versioned taint key** layered on top (M1b). `compute_taint_with_tree`
  accepts an optional `&SsaFunction`; reassignment-through-sanitizer correctly
  clears taint on the post-sanitizer SSA version. Falls back to VarRef-keyed
  mode for languages where SSA construction is partial — never panics.
- **AST member-access matching** is now structural across all 16 language
  families (M2 — closes #24). Replaces `text.contains(member_pattern)`
  with `extract_member_access_receiver_and_field` via the existing
  `field_access_info(language)` schema (covers all 18 Language variants).
  String literals containing pattern fragments (e.g., `"see req.body for details"`)
  no longer trigger sources. 217 member_patterns strings migrated from
  `&[&str]` to `&[(&str, &str)]` across 43 of 48 AST pattern banks.
  **Note:** Ruby, Elixir, and OCaml have partial `field_access_info` coverage
  (instance_variable / `@attr` / `field_get_expression` respectively);
  `Module.function` call patterns in those 3 languages retain
  `call_names` / substring fallback for v0.3.0. Structural hardening
  queued for v0.4.0.

### Deferred to v0.4.0

- **Sink dispatch flip** to AST-preferring + AST sink-parity bank work
  (TypeScript NextJS/Fastify/NestJS = 9 sinks; Python `os.spawn*` = 6
  variants — currently in regex banks only). v0.3.0 keeps additive
  dispatch (regex+AST merged) to avoid silent regression of v0.2.3 M3's
  framework patterns. v0.4.0 closes the parity as part of cross-procedural
  work — see `thoughts/shared/plans/v0.4.0-cross-procedural-design.md` §7.
- **DtoTypeIndex** (class-validator awareness for NestJS DTOs) and
  **cross-procedural taint summaries** — see v0.4.0 design doc Sections 1–2.
- **detect_sanitizer_ast** wired only via regex detect_sanitizer; AST-based
  sanitizer detection deferred to v0.4.0.

### Infrastructure

- **Multi-daemon registry** (M3) replaces v0.2.2 single-slot
  `daemon-active.json`. New commands: `tldr daemon list`,
  `tldr daemon stop --all`, `tldr daemon stop --project <abs-path>`.
  `tldr daemon status` (no flag) now errors when multiple daemons are
  running (`multiple daemons running; use --project <path> or run 'tldr daemon list'`).
  Concurrency: bounded compare-and-swap retry (3 attempts, no new
  dependency). One-shot migration shim auto-converts v0.2.x
  `daemon-active.json` on first registry access; `daemon_active` module
  marked `#[deprecated(since = "0.3.0")]`.
- **Fastembed cache fix** (M4 — closes v0.2.2 M9 deferred finding).
  `embedder.rs` honors `TLDR_FASTEMBED_CACHE` env override and defaults to
  `dirs::cache_dir().join("tldr/fastembed")`. **Default parallelism now
  works** for the test matrix; the `--test-threads=1` workaround documented
  in v0.2.2 is retired. 54 race-prone test cells annotated with
  `#[serial(embedding_cache)]` via new `serial_test = "3"` dev-dep.
  Two leaked `.fastembed_cache/` directories (~832 MB total) at workspace
  root and `crates/tldr-cli/` may be deleted:
  `rm -rf .fastembed_cache crates/tldr-cli/.fastembed_cache`

### Documentation

- v0.4.0 cross-procedural design queued at
  `thoughts/shared/plans/v0.4.0-cross-procedural-design.md` (M5).
  7 sections covering DtoTypeIndex, TaintSummary, sink dispatch flip
  + parity work, dependency graph, testing strategy, v0.4.0 milestone
  proposal M1–M7.

### Test Matrix

730/730 (`exhaustive_matrix`) + 234/234 (`language_command_matrix`) =
**964/964 at DEFAULT parallelism.** `--test-threads=1` no longer required.

### Issue close-outs

- **#24** (substring false positives in AST member-access) — closed by M2
  structural matching.

## v0.2.4 — 2026-04-28

### Fixed
- **#17 + #25** — IPC message-size enforcement before allocation. `IpcStream::recv_raw` now uses `tokio::io::AsyncReadExt::take` to bound the read at `MAX_MESSAGE_SIZE + 1` BEFORE allocating the destination String. Both Unix and Windows arms delegate to a shared `recv_raw_from<R: AsyncRead + Unpin>` helper. A 100MB no-newline payload no longer OOMs the daemon. Removed redundant post-allocation check at `read_command()`. ([commit 61e3055](https://github.com/parcadei/tldr-code/commit/61e3055))
- **#26** — `tldr surface` emits C# and Java interface methods regardless of `--include-private`. Interface methods omit `public` per language spec (implicit); the prior visibility predicate required an explicit modifier and silently dropped them. Fix mirrors the Rust trait short-circuit pattern. ([commit bc2fa83](https://github.com/parcadei/tldr-code/commit/bc2fa83))
- **#29** — `tldr imports <file> --lang <LANG>` now honored in both daemon-routed and direct-compute paths. Daemon path: new `params_with_file_lang` helper emits JSON key `"language"` to match `ImportsRequest.language` field name (was silently dropping `--lang` in the daemon hint payload). Direct-compute path: new `parse_file_with_lang(path, Option<TldrLanguage>)` sibling to `parse_file` honors caller-supplied language hint over path-extension detection; `get_imports` forwards `Some(language)`. End-to-end binary verification: `tldr imports myscript --lang python` (extensionless file, no daemon) now correctly detects imports. ([commit a3dfbc3](https://github.com/parcadei/tldr-code/commit/a3dfbc3) + [commit c034b68](https://github.com/parcadei/tldr-code/commit/c034b68))
- **#20 + #21** — Issue paperwork. Both code-fixed in v0.2.2 (M14 closed #20; M13 closed #21) and verified live in v0.2.3. Reopened pending artile confirmation; no artile activity since 2026-04-26. Closed with standard shipped-and-please-reopen-if-broken comments. ZERO source-code changes.

### Test matrix
- `cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release -- --test-threads=1`: **730/730**
- `cargo test -p tldr-cli --test language_command_matrix --features semantic --release`: **234/234**
- Combined: **964/964** + `cargo clippy --workspace --all-features --tests -- -D warnings` clean.
- New tests added: 8 (M1: 3 IPC; M2: 2 surface interface; M3: 2 unit + 1 integration).
- Pre-existing: `exhaustive_matrix` produces 676/730 under default parallelism due to fastembed-cache filesystem race (per v0.2.2 M9 investigation). Use `--test-threads=1` for canonical baseline. Real fix queued for v0.3.0.

### Issue close-outs
- **#20** (daemon status wrong project path) — confirmed shipped in v0.2.3, closed with audit comment.
- **#21** (cargo build duplicate output collisions) — confirmed shipped in v0.2.3, closed with audit comment.
- **#6, #8, #16, #22** — closed earlier this session (already-fixed-in-v0.2.x housekeeping).

## v0.2.3 — 2026-04-27

### Fixed
- **#1.D** — `tldr smells` PR-focused signal filter. New `--files <FILE>...` (repeatable, exact-path-only) for caller-supplied scoping; default behavior excludes test-file findings via existing path-only `is_test_file` helper; new `--include-tests` opts back in. New `excluded_test_smells: usize` counter on `SmellsReport`. Daemon parity (`detect_smells_with_walker_opts`). `--files` entries validated via `tldr_core::validation::validate_file_path` (errors on system dirs). ([commit 4e0b312](https://github.com/parcadei/tldr-code/commit/4e0b312))
- **#1.E** — `tldr whatbreaks` `affected_test_count` populated for Function-target queries. Bug: the function-target branch in `whatbreaks_analysis` extracted `direct_callers` and `transitive_callers` from impact JSON but never set `affected_test_count` (it stayed at default = 0 even when test modules clearly appeared in the caller tree). Fix: `run_impact_analysis` now walks the `ImpactReport`'s caller trees during JSON serialization and emits `affected_test_count` as a new JSON field; the function-target branch reads it into the summary. ([commit b3d80c9](https://github.com/parcadei/tldr-code/commit/b3d80c9))
- **#1.F** — `tldr taint` TypeScript pattern expansion: Next.js, Fastify, NestJS support added in addition to the pre-existing Express coverage. Renamed existing `TYPESCRIPT_PATTERNS` → `TYPESCRIPT_EXPRESS_PATTERNS`; added `NEXTJS_PATTERNS` (6 sources / 4 sinks / 1 sanitizer), `FASTIFY_PATTERNS` (3 sources / 3 sinks), `NESTJS_PATTERNS` (5 sources / 2 sinks; sanitizers intentionally empty). Unified `TYPESCRIPT_PATTERNS` is now the merge of all 4 banks (20 sources / 16 sinks / 3 sanitizers total). Engine semantics already supported indirect-flow propagation (CFG worklist) — patterns alone fix the bug. ([commit 191da3b](https://github.com/parcadei/tldr-code/commit/191da3b))

### Known limitations (Next.js / Fastify / NestJS taint)
- NestJS decorator-injected parameters (`(@Body() body: T)`, `@Query()`, `@Param()`) are invisible to the regex-based source matcher. Coverage focused on `@Req() request: Request` and direct `request.body` access patterns. Future engine-level work could parse decorators properly.
- NestJS pattern bank intentionally has no sanitizers — `class-validator` decorators (`@IsEmail()`, `@IsUrl()`) validate format but do not escape, so calling them sanitizers would mislead on security. Expect higher flow counts on NestJS controllers than on Express.
- `reply.send` (Fastify) and `Response.send` (NestJS) sink patterns may produce false positives on unrelated types that happen to expose a `send` method. Acceptable for v0.2.3; could be refined in a future release.

### Test matrix
- `cargo test -p tldr-cli --test exhaustive_matrix --features semantic --release -- --test-threads=1`: **730/730**
- `cargo test -p tldr-cli --test language_command_matrix --features semantic --release`: **234/234**
- Combined: **964/964** + `cargo clippy --workspace --all-features --tests -- -D warnings` clean.
- Pre-existing: `exhaustive_matrix` produces 676/730 under default parallelism due to fastembed-cache filesystem race (per v0.2.2 M9 investigation). Use `--test-threads=1` for canonical baseline. Real fix queued for v0.3.0.

## v0.2.2 — 2026-04-25

Quality release closing 9 GitHub issues filed against v0.2.0/v0.2.1, plus implementing the SSRF detection rule that was flagged as latent during v0.2.1 (the `VulnType::Ssrf` arm at `crates/tldr-core/src/security/vuln.rs:609-628` returned `vec![]` for every language, so the rule never fired despite v0.2.1's correct CWE-918 wire labelling). Seven fixes shipped across six fix commits + one feature commit; matrix held at 964/964 (730 exhaustive + 234 language-command, run with `--test-threads=1` per the test-harness embedding-mutex contention noted below); `cargo clippy --workspace --all-features --tests -- -D warnings` clean across all eight commits.

### Fixed

- **#9 + #16** — Unicode truncation sweep. Surface modules and CLI output formatters now use char-boundary-aware truncation instead of unsafe byte slicing on potentially non-ASCII text (CJK, emoji, combining marks). Triage named 15 sites; re-verification surfaced 5 additional CLI sites of the same root cause (clones tail @1641+1646, module/class/function docstring previews @2206+2261+2394 in `crates/tldr-cli/src/output.rs`) — 20 sites total fixed via shared helpers `tldr_core::util::truncate_at_char_boundary` and `truncate_at_char_boundary_from_end`. Pre-fix repro: `&s[..N]` panic with `byte index N is not a char boundary; it is inside '世'`. ([commit 88ddac6](https://github.com/parcadei/tldr-code/commit/88ddac6))
- **#18 + #6** — CFG/SSA pipeline correctness. (a) `break` statements no longer create back-edges to loop headers in the CFG (`process_break_statement` now records into `loop_exit_blocks` and the back-edge guards at the while/loop sites short-circuit on exit-block membership). (b) SSA construction no longer drops orphaned function parameters (`collect_variable_definitions` falls back to the entry block when `get_block_for_line` returns `None`, mirroring the `dfg/reaching.rs:131-134` "Orphaned definition" pattern; `fill_phi_sources` now inserts undefined-version sources rather than omitting `PhiSource` entries). ([commit 7ca7b54](https://github.com/parcadei/tldr-code/commit/7ca7b54))
- **#15 + #8** — (a) `tldr tree` no longer false-flags hardlinks as symlink cycles. The `seen_inodes` HashSet at `crates/tldr-core/src/fs/tree.rs:177-188` was unnecessary (WalkDir is configured `follow_links(false)` so symlink cycles can't occur via this code path) AND wrong (it incorrectly flagged hardlinks). Removed the entire `#[cfg(unix)]` inode block. (b) BM25 tokenizer correctly handles single-letter PascalCase prefixes like `IService` and `XRequest`. The PascalCase split rule fired on `is_upper && next_is_lower` with no length guard, splitting `IService` to `['I', 'Service']` and then dropping `'I'` via the `len >= min_length=2` filter. Added `&& current.len() > 1` guard. `HTTPRequest`-style splits preserved. ([commit 48b03f9](https://github.com/parcadei/tldr-code/commit/48b03f9))
- **#10** — Daemon callgraph + BM25 caches now actually populate and serve cached results on subsequent requests. The pre-fix shape `entry.or_insert_with(OnceCell::new).clone()` returned an INDEPENDENT uninitialized clone, so `get_or_init` initialized the clone (which got discarded), not the HashMap entry — every request rebuilt from scratch. Fix: changed HashMap value type to `Arc<OnceCell<T>>` so `.clone()` shares the cell instead of producing an independent uninitialized clone. Preserved the existing "drop write lock before await" pattern. Repro test asserts an internal rebuild counter == 1 across 2 sequential requests (was 2 pre-fix). ([commit 62ae258](https://github.com/parcadei/tldr-code/commit/62ae258))
- **#13** — Alias analysis correctly propagates points-to updates through field stores when the source variable gains new info. The `reverse_copy` index was seeded for source-propagation per inline comment, but `propagate_variable`'s third branch (re-run `propagate_field_store` when source variable changes) was unimplemented. Added `reverse_field_stores: HashMap<String, Vec<(String, String)>>` index + the missing third branch. Restores Andersen's points-to soundness for `pts(loc.field) ⊇ pts(source)` inclusion. ([commit c82e004](https://github.com/parcadei/tldr-code/commit/c82e004))
- **#14** — Daemon startup race fixed. (a) `start.rs` no longer calls `cleanup_stale_pid` before `try_acquire_lock` — the flock-based `try_acquire_lock` already handles stale PIDs safely, and the pre-lock cleanup created a TOCTOU window where two concurrent starts could both proceed. (b) `bind_unix` no longer silently unlinks an existing socket — returns `AddressInUse` instead, so a second daemon-start cannot clobber a live socket from another daemon. Verified via `std::sync::Barrier`-synchronized concurrent test (zero sleeps; 5/5 flakiness runs GREEN). ([commit d87b7f3](https://github.com/parcadei/tldr-code/commit/d87b7f3))
- **SSRF detection rule** (follow-up from v0.2.1 #11 fix) — `tldr vuln` now emits SSRF findings (CWE-918) for 8 languages: Python, TypeScript, JavaScript, Go, Java, Rust, Ruby, PHP. The empty `VulnType::Ssrf => match language` block at `crates/tldr-core/src/security/vuln.rs:609-628` (which returned `vec![]` for every language) was populated with `(pattern, description)` sink-pattern tuples mirroring the `Deserialization` arm's shape — plumbed into the existing taint-engine flow with no engine changes. `VulnType::Ssrf` was also added to the default `vuln_types` list at `vuln.rs:838-845` so the rule actually fires on the default CLI invocation path (`scan_vulnerabilities` with `vuln_filter=None`). 10 remaining languages (C, C++, Kotlin, Swift, C#, Scala, Lua, Luau, Elixir, OCaml) are explicit empty arms — deferred to v0.2.3, no behavior change vs pre-M7 for those languages. Wire format: `vuln_type` JSON field == `"ssrf"`; `cwe_id` == `"CWE-918"`. 18 tests added (15 core unit + 3 CLI integration). ([commit 372b206](https://github.com/parcadei/tldr-code/commit/372b206))
- **#1.B** — `tldr change-impact` now finds the git binary even when `/opt/homebrew/bin` (or other Homebrew/non-default paths) is not on the cargo-built binary's runtime PATH. Resolution order: `GIT_BINARY` env var → `which::which("git")` → common paths fallback (`/opt/homebrew/bin/git`, `/usr/local/bin/git`, `/usr/bin/git`). Result cached in `OnceLock<PathBuf>`. Also: when `--base <branch>` fails because only `origin/<branch>` exists locally (not the bare `<branch>`), the NoBaseline error now appends `(hint: try --base origin/<branch>)`. Reproduced via env-stripped CLI invocation; pre-fix returned NoBaseline, post-fix returns Completed with 3 real working-tree files. ([commit da377c6](https://github.com/parcadei/tldr-code/commit/da377c6))
- **#1.C** — `tldr vuln <ts-file>` now autodetects TypeScript and JavaScript without requiring `--lang`. Pre-fix exited 2 with "taint analysis for typescript is not yet supported by autodetect" even though the underlying taint engine already routes TS/JS through `TYPESCRIPT_PATTERNS` (6 sources, 7 sinks, 2 sanitizers at `taint.rs:450-487`). The fix adds `Language::TypeScript | Language::JavaScript` to `is_natively_analyzed`. Test fixture emits 10 SSRF findings (CWE-918) through the now-enabled autodetect path. ([commit c665c77](https://github.com/parcadei/tldr-code/commit/c665c77))
- **#21** — `cargo build --workspace` no longer emits "output filename collision" warnings. The standalone `tldr-daemon` and `tldr-mcp` crates declared `[[bin]]` targets that collided with the shim `[[bin]]` declarations in `tldr-cli` (which build `target/release/tldr-daemon` and `target/release/tldr-mcp` for cargo-dist's single-package distribution pattern). Removed the duplicate `[[bin]]` declarations and added `autobins = false` to suppress Cargo's auto-bin discovery. `[lib]` sections retained so the shims continue to call `tldr_daemon::run()` / `tldr_mcp::run()`. Pre-fix: 4 warnings; post-fix: 0. All 3 binaries (`tldr`, `tldr-daemon`, `tldr-mcp`) still produced. ([commit 867139c](https://github.com/parcadei/tldr-code/commit/867139c))
- **#20** — `tldr daemon status` now correctly reports a running daemon's status from any cwd (pre-fix: from a different cwd, the command computed a different socket-hash and reported `not_running` even when the daemon was alive). On `daemon start` after successful bind, an active-daemon record is written atomically to `~/Library/Caches/tldr/daemon-active.json` with `{project, pid, socket}`. `daemon status` reads this file as a fallback when `--project` is not explicitly provided, verifies the PID is alive via `kill(0)`, and uses the recorded project path to compute the socket hash. `daemon stop` removes the file. The `--project` workaround still works as a regression guard. ([commit 1a96285](https://github.com/parcadei/tldr-code/commit/1a96285))

### Notes

- The `exhaustive_matrix` test harness has a known **filesystem race on the cold fastembed model cache** under default parallel test execution. `crates/tldr-core/src/semantic/embedder.rs:122` calls `TextEmbedding::try_new(InitOptions::new(fast_model))` with no explicit `with_cache_dir(...)` override, so fastembed defaults to `<CWD>/.fastembed_cache/`. When parallel test processes spawn from a cold cache, they race on creating/extracting the ~110MB Snowflake Arctic-M model files. The first child starts a download; siblings see partially-written files and fail their integrity checks. Result: 676/730 cells under default parallelism, 730/730 with `cargo test ... -- --test-threads=1`. Pre-existing — not introduced by any v0.2.2 fix. Use single-threaded execution for the canonical matrix baseline. Recommended fix (v0.3.0): add `.with_cache_dir(dirs::cache_dir().join("tldr/fastembed"))` to move the model cache to a global location (~/Library/Caches/tldr/fastembed on macOS), eliminating the per-CWD duplication, plus `#[serial(embedding_cache)]` on the affected tests for deterministic single-flight downloads.
- `cargo install tldr-cli` and `cargo install tldr-cli --features semantic` continue to work as in v0.2.0/v0.2.1 — no new install-time requirements.
- The 4 binary targets (aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu) are built automatically by cargo-dist via `.github/workflows/release.yml` on the `v0.2.2` tag.

## v0.2.1 — 2026-04-25

Hotfix release closing 4 GitHub issues filed against v0.2.0, with scope expanded mid-flight to incorporate 2 audit-driven fixes (M6: 7 additional unguarded daemon handlers under #5; M7: request-side camelCase mismatch under #19). All 6 fixes were confirmed reproducible at their respective starting commits and fixed at root cause with new in-process integration tests pinning the bug shape. No regressions: full 964/964 matrix (730 exhaustive + 234 language-command) green across every fix commit; `cargo clippy --workspace --all-features --tests -- -D warnings` clean.

### Fixed

- **#5 (security, Unix-side path traversal)**: `tldr-daemon` IPC handlers (`secrets`, `vuln`) now route every caller-supplied absolute path through `tldr_core::validate_file_path` before any filesystem read, refusing requests for paths outside the active project root with `BAD_REQUEST`. Pre-fix, the handlers accepted any `is_absolute()` path verbatim, which on a daemon already running could be exploited to extract `/Users/<other>/.aws/credentials`-shaped secrets. The Windows TCP unauthenticated listener portion of #5 remains an open design question (multi-user daemon sharing semantics) and is deferred to v0.3.0. ([commit 00ee2dc](https://github.com/parcadei/tldr-code/commit/00ee2dc))
- **#11**: `tldr vuln --format sarif` and `--format json` now correctly label `Deserialization` findings as deserialization (CWE-502) — pre-fix, the wildcard match arm `_ => VulnType::SqlInjection` at `crates/tldr-cli/src/commands/remaining/vuln.rs:645-651` silently mislabeled them as SQL injection (CWE-89). `Ssrf` was affected by the same wildcard and is now correctly mapped to CWE-918. The match is exhaustive — future `tldr_core::security::vuln::VulnType` variants will fail to compile until they are mapped, preventing the same bug pattern from recurring. ([commit 181f929](https://github.com/parcadei/tldr-code/commit/181f929))
- **#12**: `tldr-mcp` now speaks JSON-RPC 2.0 + MCP 2024-11-05 lifecycle correctly. Three sub-bugs fixed in one commit: (a) `JsonRpcRequest.id` is now `Option<Value>` with `#[serde(default)]` so notification frames (no `id`) deserialize cleanly; (b) the dispatcher now suppresses all response emission when `id` is `None`, per JSON-RPC 2.0 §4.1 ("a server MUST NOT reply to a notification"); (c) the canonical method `notifications/initialized` is routed (the legacy bare `initialized` typo was a v0.1.x scaffold mistake — never spec-correct in any MCP draft — and was removed rather than kept as an alias to avoid masking client bugs in the wider ecosystem). ([commit 1620b6d](https://github.com/parcadei/tldr-code/commit/1620b6d))
- **#19** (filed by @etal37): `tldr-mcp`'s `initialize` response now emits `protocolVersion` and `serverInfo` in camelCase per the MCP 2024-11-05 wire spec. Pre-fix, `InitializeResult` serialized snake_case (`protocol_version`, `server_info`) which Claude Code and other spec-compliant clients reject during the lifecycle handshake — the user-facing failure was "Claude Code cannot connect to tldr-mcp". A recursive scan of the day-one handshake responses (`initialize` + `tools/list`) now returns zero snake_case keys outside JSON Schema property declarations under `inputSchema.properties` (which are user-defined argument names extracted by tool handlers, not MCP-defined wire fields). ([commit 2726358](https://github.com/parcadei/tldr-code/commit/2726358))
- **#5 (security, broader handler audit)**: Audit of `crates/tldr-daemon/src/handlers/{ast,flow,quality}.rs` found 7 additional unguarded path arguments using the same `is_absolute → accept` pattern as the original #5 fix. Each was wired through `tldr_core::validate_file_path` with `BAD_REQUEST` mapping. Affected handlers: `imports`, `cfg`, `dfg`, `slice`, `complexity`, `smells`, `maintainability`. Reproduction tests in `crates/tldr-daemon/tests/handler_path_traversal_audit_test.rs` confirm canary file content no longer leaks (canary substring `canary_xyz_42` previously appeared in `ImportInfo.module`, `CfgInfo.function`, `DfgInfo.function`, `ComplexityMetrics.function` response fields). ([commit b988c42](https://github.com/parcadei/tldr-code/commit/b988c42))
- **#19 (broader request-side audit)**: Beyond the response-side `InitializeResult` rename, audit of `crates/tldr-mcp/src/protocol.rs` found `InitializeParams` was silently dropping `protocolVersion` and `clientInfo` from spec-compliant client requests because the request-side struct lacked `#[serde(rename_all = "camelCase")]`. With `#[serde(default)]` on every field, missing-key errors degraded to `None` — Claude Code's day-1 handshake completed but the server's announced-protocol-version and client-info diagnostics became dead code. Now applied via struct-level `rename_all` attribute (future-proof against new fields silently regressing). ([commit 4204616](https://github.com/parcadei/tldr-code/commit/4204616))

### Notes

- `cargo install tldr-cli` and `cargo install tldr-cli --features semantic` continue to work as in v0.2.0 — no new install-time requirements.
- The 4 binary targets (aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu) are built automatically by cargo-dist via `.github/workflows/release.yml` on the `v0.2.1` tag.
- The original triage produced 4 fix milestones (M1–M4); the broader audit milestones M6 + M7 were dispatched after M5's release prep when latent flags surfaced during M1's and M4's work were promoted into the v0.2.1 scope rather than deferred to v0.2.2. Total v0.2.1 fixes shipped: 6 (4 originally-triaged + 2 audit-discovered).

## v0.2.0 — 2026-04-25

Major hardening release. Closes parcadei/tldr-code#1 + extends per-language coverage to all 18 supported languages across the full command surface.

### Fixes (from issue #1)

- **Walker hardening** (dc896a7): single `ProjectWalker` on `ignore::WalkBuilder` with default excludes for `node_modules`, `target`, `dist`, `build`, `.next`, `vendor`, `.git`, `__pycache__`. Replaces ~30 raw `walkdir` call sites. `tldr smells`/`secure`/`vuln` no longer descend into vendored code.
- **Language detector consolidation** (c492f49): single `Language::from_directory` with manifest-priority detection. TS projects no longer report as Python.
- **TSX parser dispatch** (9697d21): `ParserPool` selects `LANGUAGE_TSX` for `.tsx`/`.jsx` files. Resolves exponential blowup in `tldr smells` on JSX files.
- **`change-impact` honesty** (8a89f60): new `ChangeImpactStatus` enum {Completed, NoChanges, NoBaseline, DetectionFailed}. Empty results no longer return cheerful exit-0 success.
- **`vuln` autodetect + cap removed** (b1ceffa): `tldr vuln` autodetects language; emits clear error when taint backend (Python+Rust only) doesn't support detected language. Removed silent 1000-file cap.
- **Workspace discovery** (94cc6f0): call graph auto-discovers pnpm/npm/Cargo/go.work workspace roots. Multi-root tsconfig path resolution. `impact` and `whatbreaks` no longer return spurious 0-callers in monorepos.

### Coverage

- **18-language manifest detection** (d3a7e9f): added 7 missing languages (C, C++, C#, Scala, Lua, Luau, OCaml) with proper tie-breaking.
- **Cross-file call resolution** (2577737): closed gap for C, C++, Ruby, Kotlin, Swift, PHP, Luau, OCaml. All 18 languages now resolve cross-file calls.
- **Ruby bareword calls** (e3d9916): `helper` (no parens) now recognized as method call per Ruby semantics.
- **Elixir contracts** (4afae82): `def name do ... end` form now parses correctly.
- **`surface` for Luau + OCaml** (c6fe8a1): API surface extraction for the last 2 languages, including OCaml's `.mli` interface boundary.
- **`definition` for all 18 languages** (a868cbe): go-to-definition no longer Python-only.
- **`temporal` for all 18 languages** (cd81e05): method-call sequence mining no longer Python-only.

### Test infrastructure

- **234-cell command×language matrix** (2d8500c, 2577737): 13 representative commands × 18 languages, strong assertions including cross-file edge counts.
- **730-cell exhaustive matrix** (91ea0fb, c6fe8a1, a868cbe, cd81e05, e0c5e97): 38 language-applicable commands × 18 languages + orchestrator sanity.
- **Tightened weak assertions** (2cacc37, 51eb4e7, 0d35f1b, e0d2dfc): every PASS now verifies command output, not just clean exit. Surfaced and fixed 5 latent bugs (OCaml diff double-counting, OCaml `_`-pattern in structure/callgraph, bm25 hidden-root, context.rs intra-file-only, C# dead-code over-rescue).

### Known limitations

- The `semantic` feature is opt-in (`cargo install tldr-cli --features semantic`). Builds reliably on Mac; unverified on other platforms. PRs to make it portable are welcome.
- `tldr specs` is pytest-specific by design; generalizing requires per-framework parsers (Jest, RSpec, JUnit, etc.) — separate scope.
- `tldr coverage`, `tldr fix`, `tldr bugbot` operate on non-fixture inputs (XML/JSON/error-output/multi-stage) so they aren't in the per-language matrix.

### Notes

- `semantic` shipped as default in M9 was reverted to opt-in for v0.2.0 because ONNX Runtime linking is fragile on Linux aarch64 and we don't want broken `cargo install` on any platform.
