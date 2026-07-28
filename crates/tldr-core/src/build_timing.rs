//! Bounded timing distributions shared by structural and semantic builds.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

const SLOWEST_UNITS: usize = 10;

/// One major component wall-time boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTiming {
    /// Stable phase name.
    pub name: String,
    /// Phase wall duration.
    pub duration_ms: u64,
}

/// One slow atomic unit retained in a bounded top-N list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowUnit {
    /// Source-relative file, window number, or batch number.
    pub identity: String,
    /// Unit wall duration.
    pub duration_ms: u64,
}

/// Bounded timing distribution for one atomic-unit kind and optional group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitSummary {
    /// Stable unit kind (`ast_parse`, `embedding_window`, `inference_batch`).
    pub kind: String,
    /// Optional grouping dimension, such as a source language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Number of observed units.
    pub count: u64,
    /// Sum of unit wall durations. May exceed run wall time under concurrency.
    pub total_duration_ms: u64,
    /// Minimum observed duration.
    pub min_duration_ms: u64,
    /// Approximate median derived from bounded logarithmic buckets.
    pub p50_duration_ms: u64,
    /// Approximate 95th percentile derived from bounded logarithmic buckets.
    pub p95_duration_ms: u64,
    /// Maximum observed duration.
    pub max_duration_ms: u64,
    /// Bounded slowest units, longest first.
    pub slowest: Vec<SlowUnit>,
}

/// Exact opt-in timing record written as one JSON object per line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitTimingRecord {
    /// Correlated build run.
    pub run_id: String,
    /// Process or component that measured the unit.
    pub process_role: String,
    /// Stable unit kind.
    pub kind: String,
    /// Optional grouping dimension, such as a source language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Stable unit identity.
    pub identity: String,
    /// Unit wall duration.
    pub duration_ms: u64,
}

/// Bounded aggregate collector with optional streaming raw JSONL output.
pub struct UnitTimingCollector {
    run_id: String,
    process_role: String,
    units: BTreeMap<(String, Option<String>), UnitAccumulator>,
    detail: Option<BufWriter<File>>,
    detail_error: Option<String>,
}

impl UnitTimingCollector {
    /// Create a collector. When `detail_path` is present, exact unit records
    /// are streamed instead of retained in memory.
    pub fn new(
        run_id: impl Into<String>,
        process_role: impl Into<String>,
        detail_path: Option<&Path>,
    ) -> std::io::Result<Self> {
        let detail = detail_path
            .map(|path| OpenOptions::new().create(true).append(true).open(path))
            .transpose()?
            .map(BufWriter::new);
        Ok(Self {
            run_id: run_id.into(),
            process_role: process_role.into(),
            units: BTreeMap::new(),
            detail,
            detail_error: None,
        })
    }

    /// Create aggregate-only timing collection.
    pub fn aggregate(run_id: impl Into<String>, process_role: impl Into<String>) -> Self {
        Self::new(run_id, process_role, None).expect("aggregate timing does not open files")
    }

    /// Record one timed unit.
    pub fn record(
        &mut self,
        kind: impl Into<String>,
        group: Option<String>,
        identity: impl Into<String>,
        duration_ms: u64,
    ) {
        let kind = kind.into();
        let identity = identity.into();
        self.record_aggregate(&kind, group.clone(), &identity, duration_ms);
        self.write_detail(kind, group, identity, duration_ms);
    }

    /// Record one exact unit while producing both an all-groups summary and a
    /// per-group summary.
    pub fn record_grouped(
        &mut self,
        kind: impl Into<String>,
        group: impl Into<String>,
        identity: impl Into<String>,
        duration_ms: u64,
    ) {
        let kind = kind.into();
        let group = group.into();
        let identity = identity.into();
        self.record_aggregate(&kind, None, &identity, duration_ms);
        self.record_aggregate(&kind, Some(group.clone()), &identity, duration_ms);
        self.write_detail(kind, Some(group), identity, duration_ms);
    }

    fn write_detail(
        &mut self,
        kind: String,
        group: Option<String>,
        identity: String,
        duration_ms: u64,
    ) {
        if self.detail_error.is_none() {
            if let Some(writer) = self.detail.as_mut() {
                let record = UnitTimingRecord {
                    run_id: self.run_id.clone(),
                    process_role: self.process_role.clone(),
                    kind,
                    group,
                    identity,
                    duration_ms,
                };
                if let Err(error) = serde_json::to_writer(&mut *writer, &record)
                    .and_then(|_| writer.write_all(b"\n").map_err(serde_json::Error::io))
                {
                    self.detail_error = Some(error.to_string());
                }
            }
        }
    }

    fn record_aggregate(
        &mut self,
        kind: &str,
        group: Option<String>,
        identity: &str,
        duration_ms: u64,
    ) {
        self.units
            .entry((kind.to_string(), group))
            .or_default()
            .record(identity.to_string(), duration_ms);
    }

    /// Produce stable summaries ordered by kind and group.
    pub fn summaries(&self) -> Vec<UnitSummary> {
        self.units
            .iter()
            .map(|((kind, group), timings)| timings.summary(kind, group.clone()))
            .collect()
    }

    /// Flush raw output and surface any deferred write failure.
    pub fn finish(&mut self) -> std::io::Result<()> {
        if let Some(error) = self.detail_error.take() {
            return Err(std::io::Error::other(error));
        }
        if let Some(writer) = self.detail.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct UnitAccumulator {
    count: u64,
    total: u64,
    min: u64,
    max: u64,
    buckets: [u64; 64],
    slowest: Vec<SlowUnit>,
}

impl Default for UnitAccumulator {
    fn default() -> Self {
        Self {
            count: 0,
            total: 0,
            min: u64::MAX,
            max: 0,
            buckets: [0; 64],
            slowest: Vec::new(),
        }
    }
}

impl UnitAccumulator {
    fn record(&mut self, identity: String, duration_ms: u64) {
        self.count = self.count.saturating_add(1);
        self.total = self.total.saturating_add(duration_ms);
        self.min = self.min.min(duration_ms);
        self.max = self.max.max(duration_ms);
        let bucket = if duration_ms == 0 {
            0
        } else {
            (u64::BITS - duration_ms.leading_zeros()) as usize
        }
        .min(self.buckets.len() - 1);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.slowest.push(SlowUnit {
            identity,
            duration_ms,
        });
        self.slowest
            .sort_by(|left, right| right.duration_ms.cmp(&left.duration_ms));
        self.slowest.truncate(SLOWEST_UNITS);
    }

    fn percentile(&self, numerator: u64, denominator: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = self
            .count
            .saturating_mul(numerator)
            .div_ceil(denominator)
            .max(1);
        let mut seen = 0u64;
        for (index, count) in self.buckets.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= target {
                return if index == 0 { 0 } else { 1u64 << (index - 1) };
            }
        }
        self.max
    }

    fn summary(&self, kind: &str, group: Option<String>) -> UnitSummary {
        UnitSummary {
            kind: kind.to_string(),
            group,
            count: self.count,
            total_duration_ms: self.total,
            min_duration_ms: if self.count == 0 { 0 } else { self.min },
            p50_duration_ms: self.percentile(50, 100),
            p95_duration_ms: self.percentile(95, 100),
            max_duration_ms: self.max,
            slowest: self.slowest.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregation_is_grouped_and_slowest_is_bounded() {
        let mut collector = UnitTimingCollector::aggregate("run", "artifact_producer");
        for index in 0..25 {
            collector.record_grouped("ast_parse", "rust", format!("src/{index}.rs"), index);
        }
        collector.record_grouped("ast_parse", "python", "tool.py", 7);

        let summaries = collector.summaries();
        assert_eq!(summaries.len(), 3);
        let rust = summaries
            .iter()
            .find(|summary| summary.group.as_deref() == Some("rust"))
            .unwrap();
        assert_eq!(rust.count, 25);
        assert_eq!(rust.total_duration_ms, (0_u64..25).sum::<u64>());
        assert_eq!(rust.slowest.len(), SLOWEST_UNITS);
        assert_eq!(rust.slowest[0].identity, "src/24.rs");
    }

    #[test]
    fn detailed_records_stream_as_jsonl() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("units.jsonl");
        let mut collector =
            UnitTimingCollector::new("run-1", "semantic_worker", Some(&path)).unwrap();
        collector.record("embedding_window", None, "0", 12);
        collector.record("embedding_window", None, "1", 8);
        collector.finish().unwrap();

        let records = std::fs::read_to_string(path).unwrap();
        let decoded = records
            .lines()
            .map(|line| serde_json::from_str::<UnitTimingRecord>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].run_id, "run-1");
        assert_eq!(decoded[1].identity, "1");
    }
}
