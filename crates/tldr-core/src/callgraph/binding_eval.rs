//! Labeled precision/recall gate for AST-derived binding resolution.
//!
//! The binding extractor intentionally changes call-graph output, so byte
//! parity with the retired text oracle is not a useful rollout criterion.
//! This module compares resolved edge identities against reviewed labels.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::CrossFileCallEdge;

/// Stable edge identity used by reviewed binding-resolution corpora.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BindingEdgeLabel {
    /// Root-relative caller file.
    pub source_file: String,
    /// Calling function.
    pub caller: String,
    /// Root-relative destination file.
    pub destination_file: String,
    /// Resolved callee.
    pub callee: String,
}

impl BindingEdgeLabel {
    /// Normalize a produced call edge into the label schema.
    pub fn from_edge(edge: &CrossFileCallEdge) -> Self {
        Self {
            source_file: normalize_path(&edge.src_file),
            caller: edge.src_func.clone(),
            destination_file: normalize_path(&edge.dst_file),
            callee: edge.dst_func.clone(),
        }
    }
}

/// Precision/recall report with reviewable false-positive and false-negative
/// edge identities.
#[derive(Clone, Debug, Serialize)]
pub struct BindingEvalReport {
    /// Correctly produced labels.
    pub true_positives: usize,
    /// Produced edges absent from the labels.
    pub false_positives: usize,
    /// Labeled edges absent from the produced graph.
    pub false_negatives: usize,
    /// `TP / (TP + FP)`.
    pub precision: f64,
    /// `TP / (TP + FN)`.
    pub recall: f64,
    /// Harmonic mean of precision and recall.
    pub f1: f64,
    /// Unexpected produced edges in deterministic order.
    pub unexpected: Vec<BindingEdgeLabel>,
    /// Missing labeled edges in deterministic order.
    pub missing: Vec<BindingEdgeLabel>,
}

/// Compare a selected set of produced edges with reviewed labels.
///
/// Callers may intentionally select only the binding-sensitive slice (for
/// example, method calls named `save`) so unrelated direct/constructor edges
/// do not dilute the gate.
pub fn evaluate_binding_edges<'a>(
    expected: impl IntoIterator<Item = BindingEdgeLabel>,
    actual: impl IntoIterator<Item = &'a CrossFileCallEdge>,
) -> BindingEvalReport {
    let expected = expected.into_iter().collect::<BTreeSet<_>>();
    let actual = actual
        .into_iter()
        .map(BindingEdgeLabel::from_edge)
        .collect::<BTreeSet<_>>();
    let true_positives = expected.intersection(&actual).count();
    let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();
    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    let precision = ratio(true_positives, true_positives + unexpected.len());
    let recall = ratio(true_positives, true_positives + missing.len());
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    BindingEvalReport {
        true_positives,
        false_positives: unexpected.len(),
        false_negatives: missing.len(),
        precision,
        recall,
        f1,
        unexpected,
        missing,
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn normalize_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{evaluate_binding_edges, BindingEdgeLabel};
    use crate::callgraph::{build_project_call_graph_v2, BuildConfig};

    #[test]
    fn ast_binding_fixture_has_perfect_precision_and_recall() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("binding_eval")
            .join("python");
        let graph = build_project_call_graph_v2(
            &fixture,
            BuildConfig {
                language: "python".into(),
                use_type_resolution: true,
                ..Default::default()
            },
        )
        .expect("build labeled fixture");
        let actual = graph
            .edges
            .iter()
            .filter(|edge| edge.dst_func.ends_with(".save"));
        let report = evaluate_binding_edges(
            [BindingEdgeLabel {
                source_file: "service.py".into(),
                caller: "persist".into(),
                destination_file: "models.py".into(),
                callee: "User.save".into(),
            }],
            actual,
        );
        assert_eq!(report.precision, 1.0, "{:?}", report.unexpected);
        assert_eq!(report.recall, 1.0, "{:?}", report.missing);
    }

    #[test]
    fn report_exposes_false_positives_and_negatives() {
        use crate::callgraph::{CallType, CrossFileCallEdge};

        let actual = CrossFileCallEdge {
            src_file: PathBuf::from("caller.py"),
            src_func: "run".into(),
            dst_file: PathBuf::from("wrong.py"),
            dst_func: "Wrong.save".into(),
            call_type: CallType::Method,
            via_import: None,
        };
        let report = evaluate_binding_edges(
            [BindingEdgeLabel {
                source_file: "caller.py".into(),
                caller: "run".into(),
                destination_file: "right.py".into(),
                callee: "Right.save".into(),
            }],
            [&actual],
        );
        assert_eq!(report.false_positives, 1);
        assert_eq!(report.false_negatives, 1);
        assert_eq!(report.f1, 0.0);
    }
}
