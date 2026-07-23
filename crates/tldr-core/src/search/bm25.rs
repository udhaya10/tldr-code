//! BM25 keyword search implementation
//!
//! Implements BM25 (Best Matching 25) ranking algorithm for code search.
//! Uses code-aware tokenization for camelCase/snake_case splitting.
//!
//! # BM25 Formula
//! ```text
//! score(D, Q) = sum(IDF(qi) * (tf * (k1 + 1)) / (tf + k1 * (1 - b + b * |D|/avgdl)))
//! ```
//!
//! Where:
//! - tf: term frequency in document
//! - IDF: inverse document frequency
//! - k1: term frequency saturation parameter (default 1.5)
//! - b: document length normalization parameter (default 0.75)
//! - avgdl: average document length

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::tokenizer::Tokenizer;
use crate::types::Language;
use crate::TldrResult;

/// BM25 search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bm25Result {
    /// File path
    pub file_path: PathBuf,
    /// BM25 relevance score
    pub score: f64,
    /// Start line of the matching region
    pub line_start: u32,
    /// End line of the matching region
    pub line_end: u32,
    /// Snippet of matching content
    pub snippet: String,
    /// Terms that matched in this document
    pub matched_terms: Vec<String>,
}

/// Document in the BM25 index
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Document {
    /// Document ID (file path)
    id: String,
    /// Term frequencies
    term_freqs: HashMap<String, u32>,
    /// Total number of tokens
    length: usize,
    /// Original content for snippet extraction
    content: String,
}

/// BM25 search index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bm25Index {
    /// k1 parameter: term frequency saturation (default 1.5)
    k1: f64,
    /// b parameter: document length normalization (default 0.75)
    b: f64,
    /// All indexed documents
    documents: Vec<Document>,
    /// Document frequency for each term (how many docs contain term)
    doc_freqs: HashMap<String, usize>,
    /// Average document length
    avg_doc_length: f64,
    /// Running sum of all document lengths (integer to avoid float drift).
    /// INVARIANT: Must be recalculated if documents are ever removed.
    total_doc_length: usize,
    /// Tokenizer instance
    tokenizer: Tokenizer,
}

impl Default for Bm25Index {
    fn default() -> Self {
        Self::new(1.5, 0.75)
    }
}

impl Bm25Index {
    /// Create a new BM25 index with specified parameters
    ///
    /// # Arguments
    /// * `k1` - Term frequency saturation (default 1.5, higher = more weight to term frequency)
    /// * `b` - Document length normalization (default 0.75, 0 = no normalization, 1 = full normalization)
    pub fn new(k1: f64, b: f64) -> Self {
        Self {
            k1,
            b,
            documents: Vec::new(),
            doc_freqs: HashMap::new(),
            avg_doc_length: 0.0,
            total_doc_length: 0,
            tokenizer: Tokenizer::new(),
        }
    }

    /// Add a document to the index
    ///
    /// # Arguments
    /// * `doc_id` - Unique identifier for the document (typically file path)
    /// * `content` - Text content to index
    pub fn add_document(&mut self, doc_id: &str, content: &str) {
        let tokens = self.tokenizer.tokenize(content);
        let length = tokens.len();

        // Count term frequencies
        let mut term_freqs: HashMap<String, u32> = HashMap::new();
        let mut unique_terms: HashSet<String> = HashSet::new();

        for token in &tokens {
            *term_freqs.entry(token.clone()).or_insert(0) += 1;
            unique_terms.insert(token.clone());
        }

        // Update document frequencies
        for term in unique_terms {
            *self.doc_freqs.entry(term).or_insert(0) += 1;
        }

        // Add document
        self.documents.push(Document {
            id: doc_id.to_string(),
            term_freqs,
            length,
            content: content.to_string(),
        });

        // Update average document length in O(1) instead of O(n)
        self.total_doc_length += length;
        self.avg_doc_length = self.total_doc_length as f64 / self.documents.len() as f64;
    }

    /// Search the index for relevant documents
    ///
    /// # Arguments
    /// * `query` - Search query string
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    /// Vector of search results sorted by relevance score (descending)
    ///
    /// # Coverage penalty (analysis-precision-v1, BUG-20)
    ///
    /// Plain BM25 scores documents purely by per-term contribution: a query
    /// `nonexistent_term_xyz_789` (4 query tokens after camel/snake split:
    /// `nonexistent`, `term`, `xyz`, `789`) that matches a single rare token
    /// (`xyz`) in one document still ranks that document close to a
    /// hypothetical "all four matched" maximum, because the IDF of a single
    /// rare term dominates the per-document sum.
    ///
    /// To prevent a single-token sub-match from masquerading as a near-perfect
    /// hit, we apply a coverage penalty: when fewer than half of the query
    /// tokens matched a document, multiply that document's BM25 score by the
    /// coverage ratio (`matched / total`). The threshold is set at 0.5 so that
    /// documents matching the majority of the query are not penalized — only
    /// thin matches are discounted. The penalty is *multiplicative*, so a
    /// 1-of-4 match (coverage 0.25) keeps 25% of its original score; a 3-of-4
    /// match (coverage 0.75) is left untouched.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<Bm25Result> {
        let query_tokens = self.tokenizer.tokenize(query);

        if query_tokens.is_empty() || self.documents.is_empty() {
            return Vec::new();
        }

        let n = self.documents.len() as f64;

        // Total *unique* query tokens — duplicates in the user query should not
        // inflate the denominator of the coverage ratio. matched_terms below
        // is also unique-per-document by construction (one append per term).
        // We preserve insertion order (deterministic output) while deduping.
        let unique_query_tokens: Vec<String> = {
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            let mut out: Vec<String> = Vec::with_capacity(query_tokens.len());
            for t in &query_tokens {
                if seen.insert(t.as_str()) {
                    out.push(t.clone());
                }
            }
            out
        };
        let total_query_terms = unique_query_tokens.len() as f64;

        // analysis-precision-v1: penalty kicks in when coverage < this ratio.
        // 0.5 = half of query tokens must match to avoid the penalty.
        const COVERAGE_THRESHOLD: f64 = 0.5;

        // Score each document
        let mut scores: Vec<(usize, f64, Vec<String>)> = Vec::new();

        for (doc_idx, doc) in self.documents.iter().enumerate() {
            let mut score = 0.0;
            let mut matched_terms = Vec::new();

            for term in &unique_query_tokens {
                let tf = *doc.term_freqs.get(term).unwrap_or(&0) as f64;

                if tf > 0.0 {
                    matched_terms.push(term.clone());

                    // IDF calculation
                    let df = *self.doc_freqs.get(term).unwrap_or(&0) as f64;
                    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

                    // BM25 score component
                    let doc_len = doc.length as f64;
                    let numerator = tf * (self.k1 + 1.0);
                    let denominator =
                        tf + self.k1 * (1.0 - self.b + self.b * doc_len / self.avg_doc_length);

                    score += idf * (numerator / denominator);
                }
            }

            if score > 0.0 {
                // Apply coverage penalty (analysis-precision-v1, BUG-20).
                let coverage_ratio = if total_query_terms > 0.0 {
                    matched_terms.len() as f64 / total_query_terms
                } else {
                    1.0
                };
                if coverage_ratio < COVERAGE_THRESHOLD {
                    score *= coverage_ratio;
                }
                scores.push((doc_idx, score, matched_terms));
            }
        }

        // Sort by score descending
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Convert to results
        scores
            .into_iter()
            .take(top_k)
            .map(|(idx, score, matched_terms)| {
                let doc = &self.documents[idx];
                let (line_start, line_end, snippet) = extract_snippet(&doc.content, &matched_terms);

                Bm25Result {
                    file_path: PathBuf::from(&doc.id),
                    score,
                    line_start,
                    line_end,
                    snippet,
                    matched_terms,
                }
            })
            .collect()
    }

    /// Build an index from all code files in a project directory
    ///
    /// # Arguments
    /// * `root` - Root directory to index
    /// * `language` - Language to filter by (only index files of this language)
    pub fn from_project(root: &Path, language: Language) -> TldrResult<Self> {
        let mut index = Self::default();
        let extensions: Vec<&'static str> = language
            .scan_extensions()
            .iter()
            .copied()
            .filter_map(|extension| extension.strip_prefix('.'))
            .collect();

        for entry in crate::walker::ProjectWalker::new(root)
            .lang_hint(language)
            .extensions(&extensions)
            .iter()
        {
            if !entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            let path = entry.path();

            // Read and index file
            if let Ok(content) = fs::read_to_string(path) {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();

                index.add_document(&relative, &content);
            }
        }

        Ok(index)
    }

    /// Get the number of documents in the index
    pub fn document_count(&self) -> usize {
        self.documents.len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

/// Extract a relevant snippet from content based on matched terms
fn extract_snippet(content: &str, matched_terms: &[String]) -> (u32, u32, String) {
    let lines: Vec<&str> = content.lines().collect();

    if lines.is_empty() {
        return (1, 1, String::new());
    }

    // Find the line with the most matched terms
    let mut best_line_idx = 0;
    let mut best_score = 0;

    for (idx, line) in lines.iter().enumerate() {
        let line_lower = line.to_lowercase();
        let score = matched_terms
            .iter()
            .filter(|term| line_lower.contains(term.as_str()))
            .count();

        if score > best_score {
            best_score = score;
            best_line_idx = idx;
        }
    }

    // Get context around best line (3 lines total)
    let start = best_line_idx.saturating_sub(1);
    let end = (best_line_idx + 2).min(lines.len());

    let snippet = lines[start..end].join("\n");

    ((start + 1) as u32, end as u32, snippet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bm25_add_document() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "def process_data items");
        index.add_document("file2", "class DataProcessor");

        assert_eq!(index.document_count(), 2);
    }

    #[test]
    fn test_bm25_search_basic() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "process data items data data");
        index.add_document("file2", "process something else");

        let results = index.search("data", 10);
        assert!(!results.is_empty());
        // file1 should rank higher (more occurrences of "data")
        assert_eq!(results[0].file_path, PathBuf::from("file1"));
    }

    #[test]
    fn test_bm25_returns_scores() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "process data");

        let results = index.search("data", 10);
        assert!(!results.is_empty());
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn test_bm25_returns_matched_terms() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "process user data");

        let results = index.search("process data", 10);
        assert!(!results.is_empty());
        assert!(results[0].matched_terms.contains(&"process".to_string()));
        assert!(results[0].matched_terms.contains(&"data".to_string()));
    }

    #[test]
    fn test_bm25_respects_top_k() {
        let mut index = Bm25Index::new(1.5, 0.75);
        for i in 0..10 {
            index.add_document(&format!("file{}", i), "process data");
        }

        let results = index.search("data", 5);
        assert!(results.len() <= 5);
    }

    #[test]
    fn test_bm25_tokenizes_camel_case() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "processData ItemProcessor");

        let results = index.search("process", 10);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_bm25_tokenizes_snake_case() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "process_data item_processor");

        let results = index.search("process", 10);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_bm25_case_insensitive() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "PROCESS_DATA");

        let results = index.search("process", 10);
        assert!(!results.is_empty());
    }

    #[test]
    fn test_bm25_empty_query() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "process data");

        let results = index.search("", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_bm25_from_project_respects_tldrignore_and_source_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".tldrignore"), "generated/\n").unwrap();
        std::fs::write(tmp.path().join("src.cpp"), "int source_value = 1;\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("generated")).unwrap();
        std::fs::write(
            tmp.path().join("generated/ignored.cpp"),
            "int generated_value = 1;\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("events.csv"), "generated_value\n").unwrap();
        std::fs::write(tmp.path().join("server.log"), "generated_value\n").unwrap();

        let index = Bm25Index::from_project(tmp.path(), Language::Cpp).unwrap();
        let paths: Vec<_> = index
            .documents
            .iter()
            .map(|document| document.id.as_str())
            .collect();

        assert_eq!(paths, vec!["src.cpp"]);
    }

    #[test]
    fn test_bm25_no_match() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "process data");

        let results = index.search("nonexistent", 10);
        assert!(results.is_empty());
    }

    /// Regression test for issue #8 — BM25 search must match a
    /// single-letter PascalCase prefix identifier (`IService`) regardless
    /// of the query's case. Pre-fix the tokenizer split `IService` into
    /// `["I", "Service"]` then dropped `"I"` (min_length=2), so the
    /// canonical `iservice` token was never indexed and the query found
    /// zero matches.
    #[test]
    fn test_bm25_single_letter_pascal_prefix_match() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "interface IService { method(): void; }");

        let lower_results = index.search("iservice", 10);
        assert!(
            !lower_results.is_empty(),
            "BM25 search for 'iservice' against IService-containing fixture must return >= 1 result; got 0 results"
        );

        let upper_results = index.search("IService", 10);
        assert!(
            !upper_results.is_empty(),
            "BM25 search for 'IService' against IService-containing fixture must return >= 1 result; got 0 results"
        );
    }

    /// analysis-precision-v1, BUG-20: a 4-token query that only matches a
    /// single rare token in a single document must NOT score near the
    /// hypothetical 4-of-4 maximum. The coverage penalty multiplies the
    /// BM25 sum by `matched_terms.len() / total_query_terms` whenever
    /// coverage < 0.5.
    ///
    /// Before the fix: `nonexistent_term_xyz_789` (tokens: nonexistent,
    /// term, xyz, 789) against a corpus containing only `xyz` in one doc
    /// scored ~0.92 (close to BM25 max). After the fix the same hit gets
    /// multiplied by 1/4 = 0.25, dropping well below 0.5.
    #[test]
    fn test_search_low_coverage_score_discounted() {
        let mut index = Bm25Index::new(1.5, 0.75);
        // 5 unrelated documents to give "xyz" a real IDF weight.
        index.add_document("file1", "client.get(base_url=\"http://xyz.other.test\")");
        index.add_document("file2", "fn main() { println!(\"hello world\"); }");
        index.add_document("file3", "let total = compute_sum(items);");
        index.add_document("file4", "import os; from pathlib import Path");
        index.add_document("file5", "struct Config { timeout: u64 }");

        let results = index.search("nonexistent_term_xyz_789", 10);

        // Only file1 contains a matching token (`xyz`). 1-of-4 coverage = 0.25.
        // Top result must exist (we still want to surface a hit) but its
        // score must be heavily discounted (< 0.5 — the test's hard ceiling).
        assert_eq!(
            results.len(),
            1,
            "expected exactly 1 sub-match, got {}",
            results.len()
        );
        assert_eq!(results[0].matched_terms, vec!["xyz".to_string()]);
        assert!(
            results[0].score < 0.5,
            "low-coverage BM25 score must be < 0.5 (BUG-20 coverage penalty); got {}",
            results[0].score
        );
    }

    /// Companion test: a query whose tokens ALL match must NOT be penalized.
    /// Coverage = 1.0, so the score equals plain BM25.
    #[test]
    fn test_search_full_coverage_score_unchanged() {
        let mut index = Bm25Index::new(1.5, 0.75);
        index.add_document("file1", "process user data items");
        index.add_document("file2", "render html template");
        index.add_document("file3", "compile rust code");

        // 2-token query, both tokens in file1 → coverage = 1.0, no penalty.
        let results = index.search("process data", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].matched_terms.len(), 2);
        // Score should be the un-penalized BM25 sum (well above the
        // 0.5 ceiling that low-coverage hits get clamped under).
        assert!(
            results[0].score > 0.5,
            "full-coverage match must keep BM25 score (no penalty); got {}",
            results[0].score
        );
    }
}
