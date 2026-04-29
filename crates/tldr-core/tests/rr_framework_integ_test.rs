//! regex-removal-v1 — Wave 1 framework integration tests.
//!
//! These tests exercise the structural AST taint pipeline end-to-end via
//! `compute_taint_with_tree(...)` for framework patterns currently covered only
//! by regex banks. As parity-add milestones (W1-M1..M5) land, RED tests in this
//! file transition to GREEN; once Wave 2 deletes the regex banks, this file
//! becomes the primary safety net.
//!
//! Helper shape mirrored from
//! `val002_member_access_structural_test.rs::analyze` /
//! `analyze_with_ssa`. Future W1 milestones (M2, M3, M5) append their tests
//! here.
//!
//! Milestone scope:
//! * W1-M1 — NextJS sink AST entries (this file's first 4 tests).
//! * W1-M2 — Fastify sink AST entries (`reply.send` / `.redirect` / `.header`).

use std::collections::HashMap;

use tldr_core::ast::parser::parse;
use tldr_core::cfg::get_cfg_context;
use tldr_core::dfg::get_dfg_context;
use tldr_core::security::taint::compute_taint_with_tree;
use tldr_core::ssa::construct::construct_minimal_ssa;
use tldr_core::{Language, TaintInfo, TaintSinkType};

fn statements_from(src: &str) -> HashMap<u32, String> {
    src.lines()
        .enumerate()
        .map(|(i, text)| ((i + 1) as u32, text.to_string()))
        .collect()
}

#[allow(dead_code)]
fn analyze(src: &str, lang: Language, fn_name: &str) -> TaintInfo {
    analyze_with_ssa(src, lang, fn_name, /* use_ssa */ true)
}

fn analyze_with_ssa(src: &str, lang: Language, fn_name: &str, use_ssa: bool) -> TaintInfo {
    let cfg = get_cfg_context(src, fn_name, lang).expect("CFG must succeed");
    let dfg = get_dfg_context(src, fn_name, lang).expect("DFG must succeed");
    let ssa = if use_ssa {
        construct_minimal_ssa(&cfg, &dfg).ok()
    } else {
        None
    };
    let tree = parse(src, lang).expect("parse must succeed");

    compute_taint_with_tree(
        &cfg,
        &dfg.refs,
        &statements_from(src),
        Some(&tree),
        Some(src.as_bytes()),
        lang,
        ssa.as_ref(),
    )
    .expect("taint analysis must succeed")
}

// ---------- W1-M1: NextJS sinks ----------

/// W1-M1 #1 — `NextResponse.redirect(body.url)` where `body` came from
/// `request.json()`. Open-redirect via reflected URL.
///
/// Pre-W1-M1: AST bank lacks `('NextResponse','redirect')`; this test
/// fails (RED) when the regex bank is bypassed. After W1-M1 adds the entry,
/// `result.sinks` contains a `FileWrite` (per dispatch contract mapping)
/// at the `NextResponse.redirect(...)` line.
#[test]
fn nextjs_response_redirect_open_redirect_via_compute_taint() {
    let src = "\
export async function POST(request) {
    const body = await request.json();
    return NextResponse.redirect(body.url);
}
";
    // SSA disabled: keeps M1a flow-construction path consistent with val002's
    // `member_access_still_detects_real_use_ts` regression test.
    let result = analyze_with_ssa(src, Language::TypeScript, "POST", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::FileWrite))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one FileWrite sink for NextResponse.redirect; \
         got sinks={:?}",
        result.sinks
    );
}

/// W1-M1 #2 — `NextResponse.json(data)` reflects `request.json()` body —
/// reflected XSS via response body.
#[test]
fn nextjs_response_json_reflected_xss_via_compute_taint() {
    let src = "\
export async function POST(request) {
    const data = await request.json();
    return NextResponse.json(data);
}
";
    let result = analyze_with_ssa(src, Language::TypeScript, "POST", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::FileWrite))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one FileWrite sink for NextResponse.json; \
         got sinks={:?}",
        result.sinks
    );
}

/// W1-M1 #3 — server-action helper bare `redirect(url)` from
/// `next/navigation`. This is a bare CallExpression (no receiver) — covered
/// by raw-fallback `('', 'redirect')` member_pattern post-W1-M1.
#[test]
fn nextjs_redirect_helper_via_compute_taint() {
    let src = "\
import { redirect } from 'next/navigation';

export async function action(formData) {
    const url = formData.get('next');
    redirect(url);
}
";
    let result = analyze_with_ssa(src, Language::TypeScript, "action", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::FileWrite))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one FileWrite sink for bare redirect() helper; \
         got sinks={:?}",
        result.sinks
    );
}

/// W1-M1 #4 — JSX `dangerouslySetInnerHTML` attribute reflecting tainted
/// `params.html`. JSX attribute identifier match — covered by raw-fallback
/// `('', 'dangerouslySetInnerHTML')` member_pattern post-W1-M1.
#[test]
fn nextjs_dangerously_set_inner_html_via_compute_taint() {
    let src = "\
export default function Page({ params }) {
    const html = params.html;
    return <div dangerouslySetInnerHTML={{ __html: html }} />;
}
";
    let result = analyze_with_ssa(src, Language::TypeScript, "Page", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::FileWrite))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one FileWrite sink for dangerouslySetInnerHTML; \
         got sinks={:?}",
        result.sinks
    );
}

// ---------- W1-M2: Fastify sinks ----------

/// W1-M2 #1 — `reply.send(data)` reflects `request.body` — reflected
/// response from a Fastify handler. Pre-W1-M2: AST bank lacks
/// `('reply','send')`; the regex bank's FASTIFY_PATTERNS sink
/// `\breply\.send\s*\(` is what currently catches it. Post-W1-M2: matches
/// via TYPESCRIPT_AST_SINKS member_pattern.
#[test]
fn fastify_reply_send_reflected_via_compute_taint() {
    let src = "\
async function echo(request, reply) {
    const data = request.body;
    reply.send(data);
}
";
    let result = analyze_with_ssa(src, Language::TypeScript, "echo", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::FileWrite))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one FileWrite sink for reply.send; \
         got sinks={:?}",
        result.sinks
    );
}

/// W1-M2 #2 — `reply.redirect(url)` where `url` is reflected from
/// `request.body.next` — open redirect via Fastify reply API.
#[test]
fn fastify_reply_redirect_via_compute_taint() {
    let src = "\
async function go(request, reply) {
    const target = request.body.next;
    reply.redirect(target);
}
";
    let result = analyze_with_ssa(src, Language::TypeScript, "go", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::FileWrite))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one FileWrite sink for reply.redirect; \
         got sinks={:?}",
        result.sinks
    );
}

/// W1-M2 #3 — `reply.header('X-Echo', v)` where `v` came from
/// `request.headers['x-forwarded']` — header injection via Fastify reply.
#[test]
fn fastify_reply_header_injection_via_compute_taint() {
    let src = "\
async function setHeader(request, reply) {
    const v = request.headers['x-forwarded'];
    reply.header('X-Echo', v);
}
";
    let result = analyze_with_ssa(src, Language::TypeScript, "setHeader", /* use_ssa */ false);
    let sink_lines: Vec<_> = result
        .sinks
        .iter()
        .filter(|s| matches!(s.sink_type, TaintSinkType::FileWrite))
        .map(|s| s.line)
        .collect();
    assert!(
        !sink_lines.is_empty(),
        "expected at least one FileWrite sink for reply.header; \
         got sinks={:?}",
        result.sinks
    );
}
