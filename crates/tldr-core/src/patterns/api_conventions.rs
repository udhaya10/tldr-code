//! API convention pattern detection
//!
//! Detects API patterns:
//! - Framework usage (FastAPI, Flask, Express, etc.)
//! - RESTful patterns
//! - ORM usage (SQLAlchemy, Prisma, GORM, etc.)
//! - GraphQL definitions

use super::signals::PatternSignals;
use crate::types::ApiConventionPattern;

/// Convert signals to API convention pattern
pub fn signals_to_pattern(
    signals: &PatternSignals,
    evidence_limit: usize,
) -> Option<ApiConventionPattern> {
    let api_conventions = &signals.api_conventions;

    if !api_conventions.has_signals() {
        return None;
    }

    let confidence = api_conventions.calculate_confidence();

    // Detect framework
    let framework = api_conventions.detect_framework();

    // Detect patterns
    let mut patterns = Vec::new();

    if !api_conventions.fastapi_decorators.is_empty()
        || !api_conventions.flask_decorators.is_empty()
        || !api_conventions.express_routes.is_empty()
    {
        patterns.push("rest_crud".to_string());
    }

    if !api_conventions.restful_patterns.is_empty() {
        patterns.push("restful_naming".to_string());
    }

    if !api_conventions.graphql_defs.is_empty() {
        patterns.push("graphql".to_string());
    }

    // Detect ORM
    let orm_usage = api_conventions.detect_orm();

    // Collect evidence (limited)
    let mut evidence = Vec::new();
    evidence.extend(
        api_conventions
            .fastapi_decorators
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        api_conventions
            .flask_decorators
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        api_conventions
            .express_routes
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        api_conventions
            .restful_patterns
            .iter()
            .take(evidence_limit)
            .cloned(),
    );
    evidence.extend(
        api_conventions
            .orm_models
            .iter()
            .take(evidence_limit)
            .map(|(_, e)| e.clone()),
    );
    evidence.truncate(evidence_limit);

    Some(ApiConventionPattern {
        confidence,
        framework,
        patterns,
        orm_usage,
        evidence,
    })
}
