//! Import organization pattern detection
//!
//! Detects import patterns:
//! - Absolute vs relative imports
//! - Import grouping (stdlib, third-party, local)
//! - Star import usage
//! - Common alias conventions (np, pd, etc.)

use std::collections::HashMap;

use super::signals::PatternSignals;
use crate::types::{
    AliasConvention, Evidence, ImportGrouping, ImportPattern, ImportStyle, StarImportUsage,
};

/// Convert signals to import pattern
pub fn signals_to_pattern(
    signals: &PatternSignals,
    evidence_limit: usize,
) -> Option<ImportPattern> {
    let import_patterns = &signals.import_patterns;

    if !import_patterns.has_signals() {
        return None;
    }

    // Determine absolute vs relative preference
    let absolute_count = import_patterns.absolute_imports.len();
    let relative_count = import_patterns.relative_imports.len();
    let total_imports = absolute_count + relative_count;

    let absolute_vs_relative = if total_imports == 0 {
        ImportStyle::Mixed
    } else {
        let ratio = absolute_count as f64 / total_imports as f64;
        if ratio >= 0.8 {
            ImportStyle::Absolute
        } else if ratio <= 0.2 {
            ImportStyle::Relative
        } else {
            ImportStyle::Mixed
        }
    };

    // Determine star import usage
    let star_import_count = import_patterns.star_imports.len();
    let star_imports = if star_import_count == 0 {
        StarImportUsage::None
    } else if star_import_count <= 2 {
        StarImportUsage::Rare
    } else {
        StarImportUsage::Common
    };

    // Detect grouping style from collected groupings
    let grouping_style = detect_grouping_style(&import_patterns.groupings);

    // Convert aliases to AliasConvention
    let alias_conventions = convert_aliases(&import_patterns.aliases);

    // Collect evidence (limited)
    let evidence: Vec<Evidence> = import_patterns
        .star_imports
        .iter()
        .take(evidence_limit)
        .cloned()
        .collect();

    Some(ImportPattern {
        grouping_style,
        absolute_vs_relative,
        star_imports,
        alias_conventions,
        evidence,
    })
}

/// Detect the import grouping style from collected groupings
fn detect_grouping_style(groupings: &[super::signals::ImportGrouping]) -> ImportGrouping {
    if groupings.is_empty() {
        return ImportGrouping::Ungrouped;
    }

    // Count patterns observed across files
    let mut stdlib_first_count = 0;
    let mut local_first_count = 0;
    let mut third_party_first_count = 0;

    for grouping in groupings {
        // Determine which type appears first (non-empty)
        if !grouping.stdlib_imports.is_empty() {
            if grouping.third_party_imports.is_empty() || !grouping.local_imports.is_empty() {
                stdlib_first_count += 1;
            }
        } else if !grouping.third_party_imports.is_empty() {
            third_party_first_count += 1;
        } else if !grouping.local_imports.is_empty() {
            local_first_count += 1;
        }
    }

    // Determine majority pattern
    if stdlib_first_count >= third_party_first_count && stdlib_first_count >= local_first_count {
        if stdlib_first_count > 0 {
            ImportGrouping::StdlibFirst
        } else {
            ImportGrouping::Ungrouped
        }
    } else if third_party_first_count >= local_first_count {
        ImportGrouping::ThirdPartyFirst
    } else {
        ImportGrouping::LocalFirst
    }
}

/// Convert alias map to AliasConvention list, filtering out identity aliases
/// where the alias name equals the original module name (e.g. `echo -> echo`).
fn convert_aliases(aliases: &HashMap<String, String>) -> Vec<AliasConvention> {
    aliases
        .iter()
        .filter(|(module, alias)| module != alias)
        .map(|(module, alias)| AliasConvention {
            module: module.clone(),
            alias: alias.clone(),
            count: 1,
        })
        .collect()
}
