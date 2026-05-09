//! Schema assembly — build a Weave schema document from inferred elements.
//!
//! Takes the combined results of profiling, distribution fitting, temporal
//! pattern recognition, and relationship detection, and assembles them into
//! a valid Weave schema (either as a [`DataModel`] or as a human-readable DSL).

use std::collections::BTreeMap;
use std::fmt::Write;

use tracing::{debug, info};

use crate::core::{
    ActorRelationship, CountSpec, DataModel, DistributionKind, DistributionSpec, Entity, Field,
    GeneratorSpec, NullSpec, Persona, Relationship, RelationshipKind as CoreRelKind, Value,
    WeightedChoice,
};

use crate::learn::actor_graph::ActorRelationshipSpec;
use crate::learn::clustering::PersonaSpec;
use crate::learn::correlation::Correlation;
use crate::learn::fitting::{Distribution, FitResult};
use crate::learn::relationships::{RelationshipCandidate, RelationshipKind};
use crate::learn::temporal::TemporalPatternSpec;
use crate::learn::type_inference::{InferredType, StringPattern};

/// Analysis results for a single table, used as input to schema assembly.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TableAnalysis {
    /// Table / entity name.
    pub name: String,
    /// Per-column analysis results.
    pub columns: Vec<ColumnAnalysis>,
    /// Detected outgoing relationships (FKs from this table).
    pub relationships: Vec<RelationshipCandidate>,
    /// Detected correlations involving this table's columns.
    pub correlations: Vec<Correlation>,
    /// Number of rows observed in the source data.
    pub row_count: u64,
    /// Discovered personas for this table (if it's an actor entity).
    pub personas: Vec<PersonaSpec>,
    /// Discovered actor-to-actor relationship specs (from actor_graph analysis).
    pub actor_relationships: Vec<ActorRelationshipSpec>,
}

impl TableAnalysis {
    /// Create a new `TableAnalysis` with the given name, columns, and row count.
    pub fn new(name: String, columns: Vec<ColumnAnalysis>, row_count: u64) -> Self {
        Self {
            name,
            columns,
            relationships: Vec::new(),
            correlations: Vec::new(),
            row_count,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }
    }
}

/// Analysis results for a single column.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ColumnAnalysis {
    /// Column name.
    pub name: String,
    /// Whether this is a primary key.
    pub is_primary_key: bool,
    /// Distribution fit result (for numeric columns).
    pub distribution: Option<FitResult>,
    /// Temporal pattern (for timestamp columns).
    pub temporal_pattern: Option<TemporalPatternSpec>,
    /// Categorical weights (category → weight).
    pub categorical_weights: Option<Vec<(String, f64)>>,
    /// Null rate (0.0–1.0).
    pub null_rate: f64,
    /// Confidence score for the overall inference.
    pub confidence: f64,
    /// Semantic type inferred from string content (UUID, date, email, etc.).
    pub inferred_type: Option<InferredType>,
    /// Detected string patterns (email, phone, URL) with match rates.
    pub string_patterns: Vec<(StringPattern, f64)>,
    /// Whether a numeric column contains only integer values.
    pub is_integer_valued: bool,
    /// Whether a temporal column has time-of-day precision (vs date-only).
    pub has_time_component: bool,
    /// Min/max timestamps as seconds since epoch (for temporal range).
    pub temporal_range: Option<(f64, f64)>,
    /// Original Arrow DataType from the source data (for type precision, e.g. Int32 vs Int64).
    pub source_arrow_type: Option<arrow::datatypes::DataType>,
    /// Maximum decimal places observed in float values (from profiling).
    pub max_decimal_places: Option<u8>,
    /// Whether this column was explicitly marked as an actor column (via --actor-column).
    pub is_actor_column: bool,
}

impl ColumnAnalysis {
    /// Create a new `ColumnAnalysis` with required fields; optional fields default to `None`/empty.
    pub fn new(name: String, null_rate: f64, confidence: f64) -> Self {
        Self {
            name,
            is_primary_key: false,
            distribution: None,
            temporal_pattern: None,
            categorical_weights: None,
            null_rate,
            confidence,
            inferred_type: None,
            string_patterns: vec![],
            is_integer_valued: false,
            has_time_component: false,
            temporal_range: None,
            source_arrow_type: None,
            max_decimal_places: None,
            is_actor_column: false,
        }
    }
}

/// Assemble a Weave schema document from table analyses.
///
/// Produces a human-readable schema string with generator specifications,
/// relationship references, and confidence annotations.
pub fn assemble_schema(tables: &[TableAnalysis]) -> String {
    let mut out = String::with_capacity(4096);

    writeln!(out, "# Auto-generated Weave schema").unwrap();
    writeln!(out, "# Generated by knit-learn").unwrap();
    writeln!(out).unwrap();

    for table in tables {
        debug!(table = %table.name, cols = table.columns.len(), "assembling table schema");
        assemble_table(&mut out, table);
        writeln!(out).unwrap();
    }

    info!(tables = tables.len(), bytes = out.len(), "schema assembled");
    out
}

/// Assemble a proper [`DataModel`] from table analyses.
///
/// Unlike [`assemble_schema`], this returns a structured model that can be
/// serialized to valid Weave TOML/JSON by the caller.
pub fn assemble_data_model(name: &str, tables: &[TableAnalysis]) -> DataModel {
    let mut entities = Vec::with_capacity(tables.len());
    let mut relationships = Vec::new();
    let mut correlations = Vec::new();
    let mut personas = Vec::new();
    let mut actor_relationships = Vec::new();

    for table in tables {
        let (entity, rels, corrs) = build_entity(table);
        entities.push(entity);
        relationships.extend(rels);
        correlations.extend(corrs);

        // Convert discovered personas into core types
        // Personas are emitted for actor entities and for activity entities
        // that have actor columns (indicating --actors was used during learn).
        if !table.personas.is_empty() {
            let has_actor_columns = entities
                .last()
                .map(|e| e.fields.iter().any(|f| f.actor_column))
                .unwrap_or(false);
            let should_emit =
                entities.last().map(|e| e.actor).unwrap_or(false) || has_actor_columns;

            if should_emit {
                if let Some(entity) = entities.last() {
                    let entity_name = &entity.name;
                    for spec in &table.personas {
                        let namespaced = format!("{}_{}", entity_name, spec.name);
                        personas.push(Persona {
                            name: namespaced,
                            weight: spec.weight,
                            traits: spec.traits.clone(),
                        });
                    }
                }

                if let Some(entity) = entities.last_mut() {
                    entity.persona_distribution = Some("personas".into());
                }
            }
        }

        // Convert discovered actor relationships into core types
        for spec in &table.actor_relationships {
            actor_relationships.push(ActorRelationship {
                name: spec.name.clone(),
                from_entity: spec.from_entity.clone(),
                to_entity: spec.to_entity.clone(),
                graph_type: spec.graph_type.clone(),
                params: spec.params.clone(),
                community_count: spec.community_count.map(|c| CountSpec::Fixed(c as u64)),
                hierarchy_depth: spec.hierarchy_depth,
            });
        }
    }

    let actor_count = entities.iter().filter(|e| e.actor).count();
    let actor_col_count: usize = entities
        .iter()
        .flat_map(|e| e.fields.iter())
        .filter(|f| f.actor_column)
        .count();

    info!(
        entities = entities.len(),
        relationships = relationships.len(),
        actor_entities = actor_count,
        actor_columns = actor_col_count,
        personas = personas.len(),
        actor_relationships = actor_relationships.len(),
        "data model assembled"
    );

    DataModel {
        name: name.to_string(),
        description: Some("Auto-generated by knit learn".to_string()),
        seed: 42,
        locale: "en_US".to_string(),
        timezone: "UTC".to_string(),
        entities,
        relationships,
        noise_profiles: Vec::new(),
        correlations,
        params: BTreeMap::new(),
        schema_version: "1.0".to_string(),
        personas,
        actor_relationships,
        custom_types: Vec::new(),
        mixins: Vec::new(),
    }
}

/// Build an [`Entity`] from a `TableAnalysis`, extracting relationships
/// and correlations as separate top-level items.
fn build_entity(table: &TableAnalysis) -> (Entity, Vec<Relationship>, Vec<crate::core::Correlation>) {
    let mut fields = Vec::with_capacity(table.columns.len());

    for col in &table.columns {
        let fk = table
            .relationships
            .iter()
            .find(|r| r.from_column == col.name && r.from_table == table.name);

        let generator = build_generator(col, fk);
        let data_type = infer_data_type(col, fk);
        let nullable = if col.null_rate > 0.01 {
            NullSpec::Probability(col.null_rate)
        } else {
            NullSpec::Never
        };

        // Infer precision for float columns from source data
        let precision = if data_type == crate::core::DataType::Float {
            col.max_decimal_places
        } else {
            None
        };

        fields.push(Field {
            name: col.name.clone(),
            description: None,
            data_type,
            generator: Some(generator),
            nullable,
            primary_key: if col.is_primary_key { Some(true) } else { None },
            precision,
            actor_column: false,
            fields: vec![],
        });
    }

    // Detect paired temporal columns (Start/End) and rewrite End generators
    // to use Relative offsets from Start, ensuring EndDate ≥ StartDate.
    rewrite_temporal_pairs(&mut fields, &table.columns);

    // Build top-level Relationship entries from detected FKs
    let rels: Vec<Relationship> = table
        .relationships
        .iter()
        .map(|r| Relationship {
            name: format!("{}_{}_fk", r.from_table, r.from_column),
            from: r.from_table.clone(),
            to: r.to_table.clone(),
            kind: match r.kind {
                RelationshipKind::OneToOne => CoreRelKind::OneToOne,
                RelationshipKind::OneToMany => CoreRelKind::OneToMany,
                RelationshipKind::ManyToMany => CoreRelKind::ManyToMany,
            },
            foreign_key: Some(r.from_column.clone()),
            cardinality: None,
        })
        .collect();

    // Build top-level Correlation entries
    let corrs: Vec<crate::core::Correlation> = table
        .correlations
        .iter()
        .filter_map(|c| {
            let fields = vec![c.column_a.clone(), c.column_b.clone()];
            // Only emit if coefficient is meaningful
            if c.coefficient.abs() < 0.3 {
                return None;
            }
            Some(crate::core::Correlation {
                entity: table.name.clone(),
                fields,
                matrix: vec![vec![1.0, c.coefficient], vec![c.coefficient, 1.0]],
                conditional: Vec::new(),
                copula: None,
            })
        })
        .collect();

    // Detect actor columns by name heuristics and mark them
    let actor_scores = detect_actor_columns(&table.columns);

    // An entity is an actor if it has a PK that looks actor-like (person table)
    let is_actor = is_actor_entity(&table.name, &actor_scores, &fields);

    // Mark actor columns from heuristics, but skip PKs on actor entities
    for (col_name, _score) in &actor_scores {
        if let Some(field) = fields.iter_mut().find(|f| &f.name == col_name) {
            let is_pk_on_actor = is_actor && field.primary_key == Some(true);
            if !is_pk_on_actor {
                field.actor_column = true;
            }
        }
    }

    // Also mark explicitly flagged actor columns (from --actor-column)
    for col in &table.columns {
        if col.is_actor_column {
            if let Some(field) = fields.iter_mut().find(|f| f.name == col.name) {
                field.actor_column = true;
            }
        }
    }

    let entity = Entity {
        name: table.name.clone(),
        description: None,
        count: CountSpec::Fixed(table.row_count),
        fields,
        constraints: Vec::new(),
        topology: None,
        actor: is_actor,
        persona_distribution: None,
        activity_count: None,
        mixin_refs: None,
    };

    (entity, rels, corrs)
}

// ── Actor column detection ──────────────────────────────────────────

/// Actor-related name prefixes that suggest a human/person column.
const ACTOR_PREFIXES: &[&str] = &[
    "user",
    "person",
    "employee",
    "customer",
    "member",
    "agent",
    "author",
    "owner",
    "sender",
    "receiver",
    "recipient",
    "creator",
    "assignee",
    "requester",
    "approver",
    "reviewer",
    "manager",
    "patient",
    "student",
    "teacher",
    "driver",
    "passenger",
];

/// Score a column name for actor likelihood using name-based heuristics.
///
/// Returns a score in 0.0–1.0. Based on the patterns from the design doc:
/// - `*_id` with actor prefix → 0.95
/// - `*_by` suffix → 0.85
/// - `sender`, `receiver`, `from`, `to` standalone → 0.80
/// - `*_name` with actor prefix → 0.70
pub fn score_actor_column(name: &str) -> f64 {
    let lower = name.to_lowercase();

    // Pattern: {actor_prefix}_id or {actor_prefix}id
    if lower.ends_with("_id") || lower.ends_with("id") {
        let stem = if lower.ends_with("_id") {
            &lower[..lower.len() - 3]
        } else {
            &lower[..lower.len() - 2]
        };
        if ACTOR_PREFIXES
            .iter()
            .any(|p| stem == *p || stem.ends_with(&format!("_{}", p)))
        {
            return 0.95;
        }
    }

    // Pattern: *_by — restricted to known verb stems to avoid false positives
    // (e.g. group_by, sort_by, order_by should NOT match)
    if lower.ends_with("_by") {
        let action_verbs = [
            "created",
            "updated",
            "modified",
            "assigned",
            "approved",
            "rejected",
            "reviewed",
            "submitted",
            "completed",
            "closed",
            "opened",
            "resolved",
            "owned",
            "managed",
            "handled",
            "processed",
            "requested",
            "reported",
            "sent",
            "received",
            "initiated",
            "authorized",
            "verified",
        ];
        let stem = &lower[..lower.len() - 3];
        if action_verbs.contains(&stem) {
            return 0.85;
        }
    }

    // Pattern: standalone actor names in messaging/role context
    let standalone_actors = [
        "sender",
        "receiver",
        "recipient",
        "assignee",
        "owner",
        "author",
        "reviewer",
        "manager",
        "creator",
        "requester",
        "approver",
    ];
    if standalone_actors.contains(&lower.as_str()) {
        return 0.80;
    }

    // Pattern: {actor_prefix}_name
    if lower.ends_with("_name") {
        let stem = &lower[..lower.len() - 5];
        if ACTOR_PREFIXES.contains(&stem) {
            return 0.70;
        }
    }

    // Pattern: {actor_prefix}_email or {actor_prefix}_code
    if lower.ends_with("_email") || lower.ends_with("_code") {
        let suffix_len = if lower.ends_with("_email") { 6 } else { 5 };
        let stem = &lower[..lower.len() - suffix_len];
        if ACTOR_PREFIXES.contains(&stem) {
            return 0.70;
        }
    }

    0.0
}

/// Detect actor columns in a table, returning columns with scores above threshold.
///
/// Only returns columns scoring ≥ 0.6 (the design doc threshold).
fn detect_actor_columns(columns: &[ColumnAnalysis]) -> Vec<(String, f64)> {
    let mut results = Vec::new();
    for col in columns {
        let score = score_actor_column(&col.name);
        if score >= 0.6 {
            debug!(column = %col.name, score, "detected actor column");
            results.push((col.name.clone(), score));
        }
    }
    results
}

/// Determine if an entity itself represents actors (a "person table").
///
/// An entity is considered an actor if its table name matches actor patterns
/// OR if it has a primary key column that is actor-like.
fn is_actor_entity(table_name: &str, actor_scores: &[(String, f64)], fields: &[Field]) -> bool {
    let lower_name = table_name.to_lowercase();

    // Check if the table name contains an actor keyword (handles prefixed
    // tables like app_users, dim_customer, hr_employees, user_accounts)
    let actor_keywords = [
        "user",
        "employee",
        "customer",
        "member",
        "agent",
        "person",
        "people",
        "author",
        "owner",
        "student",
        "teacher",
        "driver",
        "passenger",
        "patient",
    ];
    // Split on common delimiters and check tokens
    let tokens: Vec<&str> = lower_name.split(['_', '-', '.']).collect();
    for keyword in &actor_keywords {
        // Check singular or plural form in any token
        let plural = format!("{}s", keyword);
        if tokens.iter().any(|t| *t == *keyword || *t == plural) {
            return true;
        }
    }

    // Check if a PK column is actor-like (e.g., user_id as PK)
    for (col_name, score) in actor_scores {
        if *score >= 0.9 {
            if let Some(field) = fields.iter().find(|f| &f.name == col_name) {
                if field.primary_key == Some(true) {
                    return true;
                }
            }
        }
    }

    false
}

/// Build a [`GeneratorSpec`] for a column based on inferred properties.
fn build_generator(col: &ColumnAnalysis, fk: Option<&RelationshipCandidate>) -> GeneratorSpec {
    // FK → Lookup (only for non-string sources; string FKs use categorical)
    if let Some(rel) = fk {
        let source_is_string = matches!(
            col.source_arrow_type,
            Some(arrow::datatypes::DataType::Utf8) | Some(arrow::datatypes::DataType::LargeUtf8)
        );
        if !source_is_string {
            return GeneratorSpec::Lookup {
                entity: rel.to_table.clone(),
                field: rel.to_column.clone(),
            };
        }
        // String FK: fall through to categorical/string handling below
    }

    // PK → Sequence (or UuidGen for UUID columns)
    if col.is_primary_key {
        if matches!(col.inferred_type, Some(InferredType::Uuid)) {
            return GeneratorSpec::UuidGen { version: 4 };
        }
        // String PKs with numeric content: use sequence with empty prefix to produce string output
        let source_is_string = matches!(
            col.source_arrow_type,
            Some(arrow::datatypes::DataType::Utf8) | Some(arrow::datatypes::DataType::LargeUtf8)
        );
        return GeneratorSpec::Sequence {
            start: 1,
            step: 1,
            prefix: if source_is_string {
                Some(String::new())
            } else {
                None
            },
        };
    }

    // Temporal pattern
    if col.temporal_pattern.is_some() {
        return build_temporal_generator(col);
    }

    // Distribution
    if let Some(fit) = &col.distribution {
        // For string sources with low-cardinality numeric content, prefer categorical
        // to preserve exact source values (e.g., "1", "2", "3" stay as-is)
        let source_is_string = matches!(
            col.source_arrow_type,
            Some(arrow::datatypes::DataType::Utf8) | Some(arrow::datatypes::DataType::LargeUtf8)
        );
        if source_is_string && col.categorical_weights.is_some() {
            // Fall through to categorical handling below
        } else {
            return build_distribution_generator(&fit.best.distribution, col.is_integer_valued);
        }
    }

    // Boolean (check before categorical since bool columns store weights there)
    // But for string-sourced booleans, prefer categorical to preserve original casing (e.g., "TRUE")
    if matches!(col.inferred_type, Some(InferredType::Boolean)) {
        let source_is_string = matches!(
            col.source_arrow_type,
            Some(arrow::datatypes::DataType::Utf8) | Some(arrow::datatypes::DataType::LargeUtf8)
        );
        if !source_is_string {
            if let Some(weights) = &col.categorical_weights {
                let true_w = weights
                    .iter()
                    .find(|(k, _)| k == "true")
                    .map(|(_, w)| *w)
                    .unwrap_or(0.5);
                let false_w = weights
                    .iter()
                    .find(|(k, _)| k == "false")
                    .map(|(_, w)| *w)
                    .unwrap_or(0.5);
                return GeneratorSpec::OneOf {
                    choices: vec![
                        WeightedChoice {
                            value: Value::Bool(true),
                            weight: true_w,
                        },
                        WeightedChoice {
                            value: Value::Bool(false),
                            weight: false_w,
                        },
                    ],
                };
            }
            return GeneratorSpec::OneOf {
                choices: vec![
                    WeightedChoice {
                        value: Value::Bool(true),
                        weight: 0.5,
                    },
                    WeightedChoice {
                        value: Value::Bool(false),
                        weight: 0.5,
                    },
                ],
            };
        }
        // String-sourced booleans fall through to categorical below
    }

    // Categorical
    if let Some(weights) = &col.categorical_weights {
        // For numeric source types, produce integer-valued categoricals
        let is_int_source = is_narrow_int_source(col)
            || matches!(
                col.source_arrow_type,
                Some(arrow::datatypes::DataType::Int64)
                    | Some(arrow::datatypes::DataType::UInt32)
                    | Some(arrow::datatypes::DataType::UInt64)
            );
        if is_int_source {
            return build_int_categorical_generator(weights);
        }
        return build_categorical_generator(weights);
    }

    // Semantic type from string inference
    if let Some(ref inferred) = col.inferred_type {
        match inferred {
            InferredType::Uuid => {
                return GeneratorSpec::UuidGen { version: 4 };
            }
            InferredType::Date(_) => {
                let method = if col.has_time_component {
                    "datetime"
                } else {
                    "date"
                };
                return GeneratorSpec::Faker {
                    method: method.into(),
                    args: vec![],
                };
            }
            _ => {}
        }
    }

    // Column name heuristic — map common names to appropriate faker methods.
    // Placed before string pattern matching so semantic names override generic patterns
    // (e.g., "PostalCode" → zip_code instead of phone pattern match).
    if let Some(method) = faker_method_from_column_name(&col.name) {
        return GeneratorSpec::Faker {
            method: method.into(),
            args: vec![],
        };
    }

    // String pattern → Faker
    if let Some((pattern, _rate)) = col.string_patterns.first() {
        match pattern {
            StringPattern::Email => {
                return GeneratorSpec::Faker {
                    method: "email".into(),
                    args: vec![],
                };
            }
            StringPattern::Phone => {
                return GeneratorSpec::Faker {
                    method: "phone".into(),
                    args: vec![],
                };
            }
            StringPattern::Name => {
                return GeneratorSpec::Faker {
                    method: "name".into(),
                    args: vec![],
                };
            }
            StringPattern::Date => {
                return GeneratorSpec::Faker {
                    method: "date".into(),
                    args: vec![],
                };
            }
            StringPattern::HexString(len) => {
                return GeneratorSpec::Faker {
                    method: "hex_string".into(),
                    args: vec![crate::core::Value::Int(*len as i64)],
                };
            }
            _ => {}
        }
    }

    // Fallback: use faker("word") for string/text columns, sequence for others
    if matches!(
        col.inferred_type,
        Some(InferredType::Text) | Some(InferredType::Categorical)
    ) || !col.string_patterns.is_empty()
    {
        return GeneratorSpec::Faker {
            method: "word".into(),
            args: vec![],
        };
    }
    GeneratorSpec::Sequence {
        start: 1,
        step: 1,
        prefix: None,
    }
}

/// Map a fitted distribution to a [`GeneratorSpec::Distribution`].
fn build_distribution_generator(dist: &Distribution, round: bool) -> GeneratorSpec {
    let (kind, params) = match dist {
        Distribution::Normal(mean, std_dev) => {
            let mut p = BTreeMap::new();
            p.insert("mean".into(), *mean);
            p.insert("std_dev".into(), *std_dev);
            (DistributionKind::Normal, p)
        }
        Distribution::LogNormal(mu, sigma) => {
            let mut p = BTreeMap::new();
            p.insert("mu".into(), *mu);
            p.insert("sigma".into(), *sigma);
            (DistributionKind::LogNormal, p)
        }
        Distribution::Exponential(lambda) => {
            let mut p = BTreeMap::new();
            p.insert("lambda".into(), *lambda);
            (DistributionKind::Exponential, p)
        }
        Distribution::Uniform(min, max) => {
            let mut p = BTreeMap::new();
            p.insert("min".into(), *min);
            p.insert("max".into(), *max);
            (DistributionKind::Uniform, p)
        }
        Distribution::Poisson(lambda) => {
            let mut p = BTreeMap::new();
            p.insert("lambda".into(), *lambda);
            (DistributionKind::Poisson, p)
        }
        Distribution::Beta(alpha, beta) => {
            let mut p = BTreeMap::new();
            p.insert("alpha".into(), *alpha);
            p.insert("beta".into(), *beta);
            (DistributionKind::Beta, p)
        }
        Distribution::Gamma(shape, rate) => {
            let mut p = BTreeMap::new();
            p.insert("shape".into(), *shape);
            // Generator expects scale (= 1/rate)
            p.insert("scale".into(), 1.0 / *rate);
            (DistributionKind::Gamma, p)
        }
        Distribution::Pareto(x_m, alpha) => {
            let mut p = BTreeMap::new();
            p.insert("scale".into(), *x_m);
            p.insert("shape".into(), *alpha);
            (DistributionKind::Pareto, p)
        }
        Distribution::Zipf(n, s) => {
            let mut p = BTreeMap::new();
            p.insert("n".into(), *n as f64);
            p.insert("s".into(), *s);
            (DistributionKind::Zipf, p)
        }
    };

    GeneratorSpec::Distribution {
        spec: DistributionSpec {
            kind,
            params,
            round,
        },
    }
}

/// Map categorical weights to a [`GeneratorSpec::OneOf`].
fn build_categorical_generator(weights: &[(String, f64)]) -> GeneratorSpec {
    let choices: Vec<WeightedChoice> = weights
        .iter()
        .take(200)
        .map(|(val, w)| WeightedChoice {
            value: Value::String(val.clone()),
            weight: *w,
        })
        .collect();

    GeneratorSpec::OneOf { choices }
}

/// Build a categorical generator that produces integer values.
/// Used when source column is an integer type with few distinct values.
fn build_int_categorical_generator(weights: &[(String, f64)]) -> GeneratorSpec {
    let choices: Vec<WeightedChoice> = weights
        .iter()
        .take(200)
        .map(|(val, w)| {
            let int_val = val.parse::<i64>().unwrap_or(0);
            WeightedChoice {
                value: Value::Int(int_val),
                weight: *w,
            }
        })
        .collect();

    GeneratorSpec::OneOf { choices }
}

/// Map a temporal pattern to a [`GeneratorSpec`].
fn build_temporal_generator(col: &ColumnAnalysis) -> GeneratorSpec {
    let method = if col.has_time_component {
        "datetime"
    } else {
        "date"
    };
    let args = if let Some((min_s, max_s)) = col.temporal_range {
        // Convert epoch seconds to ISO date strings for args
        let min_days = (min_s / 86_400.0).floor() as i64;
        let max_days = (max_s / 86_400.0).floor() as i64;
        let (y1, m1, d1) = days_to_ymd(min_days);
        let (y2, m2, d2) = days_to_ymd(max_days);
        vec![
            Value::String(format!("{y1:04}-{m1:02}-{d1:02}")),
            Value::String(format!("{y2:04}-{m2:02}-{d2:02}")),
        ]
    } else {
        vec![]
    };
    GeneratorSpec::Faker {
        method: method.into(),
        args,
    }
}

/// Detect paired temporal columns (e.g. StartDate/EndDate) and rewrite the "end"
/// field's generator to `Relative`, ensuring generated EndDate ≥ StartDate.
///
/// Matching strategy: find columns whose names share a common base with one
/// containing "start" and the other "end" (case-insensitive). Uses the temporal
/// ranges from column analysis to estimate the mean offset in seconds.
fn rewrite_temporal_pairs(fields: &mut [Field], columns: &[ColumnAnalysis]) {
    // Build a map of column name → temporal range for quick lookup
    let range_map: BTreeMap<&str, (f64, f64)> = columns
        .iter()
        .filter_map(|c| c.temporal_range.map(|r| (c.name.as_str(), r)))
        .collect();

    // Find start/end pairs by name pattern
    let pairs = find_temporal_pairs(fields);

    for (start_name, end_idx) in pairs {
        // Compute mean offset from temporal ranges
        let offset_seconds = match (
            range_map.get(start_name.as_str()),
            range_map.get(fields[end_idx].name.as_str()),
        ) {
            (Some(&(s_min, s_max)), Some(&(e_min, e_max))) => {
                // Use midpoint difference as the mean offset
                let s_mid = (s_min + s_max) / 2.0;
                let e_mid = (e_min + e_max) / 2.0;
                let diff = e_mid - s_mid;
                if diff > 0.0 {
                    diff
                } else {
                    86_400.0
                } // default 1 day if ranges overlap completely
            }
            _ => 86_400.0, // default 1 day
        };

        debug!(
            start = %start_name,
            end = %fields[end_idx].name,
            offset_s = offset_seconds,
            "rewriting end field as Relative to start"
        );

        fields[end_idx].generator = Some(GeneratorSpec::Relative {
            field: start_name,
            offset: Value::Float(offset_seconds),
        });
    }
}

/// Find temporal column pairs where one is a "start" and the other an "end".
/// Only considers fields with temporal data types (Datetime, DatetimeUs, Date).
/// Returns (start_field_name, end_field_index) pairs.
fn find_temporal_pairs(fields: &[Field]) -> Vec<(String, usize)> {
    use crate::core::DataType;

    let mut pairs = Vec::new();

    let is_temporal = |dt: &DataType| {
        matches!(
            dt,
            DataType::Datetime | DataType::DatetimeUs | DataType::Date | DataType::Datetimetz
        )
    };

    // Common patterns: StartDate/EndDate, start_time/end_time, StartedAt/EndedAt
    let start_patterns: &[&str] = &["start", "begin", "from"];
    let end_patterns: &[&str] = &["end", "finish", "until"];

    // Index fields by lowercase name
    let field_names: Vec<String> = fields.iter().map(|f| f.name.to_lowercase()).collect();

    for (ei, end_lower) in field_names.iter().enumerate() {
        // Only consider temporal fields
        if !is_temporal(&fields[ei].data_type) {
            continue;
        }

        // Check if this field matches an "end" pattern
        let end_match = end_patterns.iter().find(|&&pat| end_lower.contains(pat));
        if end_match.is_none() {
            continue;
        }
        let end_pat = *end_match.unwrap();

        // Try to find a corresponding start field
        for (si, start_lower) in field_names.iter().enumerate() {
            if si == ei {
                continue;
            }
            if !is_temporal(&fields[si].data_type) {
                continue;
            }

            let start_match = start_patterns
                .iter()
                .find(|&&pat| start_lower.contains(pat));
            if start_match.is_none() {
                continue;
            }
            let start_pat = *start_match.unwrap();

            // Check they share a common suffix/prefix (e.g., both end with "Date" or "Time")
            let end_base = end_lower.replace(end_pat, "");
            let start_base = start_lower.replace(start_pat, "");
            if end_base == start_base {
                pairs.push((fields[si].name.clone(), ei));
                break;
            }
        }
    }

    pairs
}
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Map common column names to faker methods using keyword heuristics.
/// More specific patterns are checked first to avoid false positives
/// (e.g., "company_url" should match url, not company).
fn faker_method_from_column_name(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    // URL-like patterns (checked first — most specific)
    if lower.contains("url") || lower.contains("website") || lower.contains("homepage") {
        return Some("url");
    }
    // Geographic / address patterns
    if lower.contains("address") || lower.contains("street") {
        return Some("address");
    }
    if lower == "city" || lower.ends_with("_city") || lower.starts_with("city_") {
        return Some("city");
    }
    if lower == "state" || lower == "province" || lower.ends_with("_state") {
        return Some("state");
    }
    if lower == "country" || lower.ends_with("_country") || lower.starts_with("country_") {
        return Some("country");
    }
    if lower.contains("zip") || lower.contains("postal") {
        return Some("zip_code");
    }
    // Organization — match "company" but exclude non-semantic suffixes like _id, _count
    if (lower.contains("company")
        || lower.contains("organization")
        || lower.contains("organisation"))
        && !lower.ends_with("_id")
        && !lower.ends_with("_count")
        && !lower.ends_with("_num")
        && !lower.starts_with("is_")
        && !lower.starts_with("has_")
    {
        return Some("company");
    }
    // Domain (exact or boundary match to avoid "domain_id" → faker)
    if lower == "domain" || lower.ends_with("_domain") || lower == "domain_name" {
        return Some("domain");
    }
    None
}

/// Check whether a column's source Arrow type is a narrow integer (Int32 or smaller).
fn is_narrow_int_source(col: &ColumnAnalysis) -> bool {
    matches!(
        col.source_arrow_type,
        Some(arrow::datatypes::DataType::Int8)
            | Some(arrow::datatypes::DataType::Int16)
            | Some(arrow::datatypes::DataType::Int32)
            | Some(arrow::datatypes::DataType::UInt8)
            | Some(arrow::datatypes::DataType::UInt16)
    )
}

/// Check whether source timestamp uses microsecond precision.
fn is_microsecond_timestamp(col: &ColumnAnalysis) -> bool {
    matches!(
        col.source_arrow_type,
        Some(arrow::datatypes::DataType::Timestamp(
            arrow::datatypes::TimeUnit::Microsecond,
            _
        ))
    )
}

/// Check whether source is a List/LargeList type.
fn is_list_source(col: &ColumnAnalysis) -> bool {
    matches!(
        col.source_arrow_type,
        Some(arrow::datatypes::DataType::List(_)) | Some(arrow::datatypes::DataType::LargeList(_))
    )
}

/// Check whether source is a Map type.
fn is_map_source(col: &ColumnAnalysis) -> bool {
    matches!(
        col.source_arrow_type,
        Some(arrow::datatypes::DataType::Map(_, _))
    )
}

/// Select the appropriate datetime DataType based on source precision.
fn resolve_datetime_type(col: &ColumnAnalysis) -> crate::core::DataType {
    if is_microsecond_timestamp(col) {
        crate::core::DataType::DatetimeUs
    } else {
        crate::core::DataType::Datetime
    }
}

/// Infer a [`crate::core::DataType`] from column analysis.
fn infer_data_type(
    col: &ColumnAnalysis,
    fk: Option<&RelationshipCandidate>,
) -> crate::core::DataType {
    // Complex types (List, Map) → preserve as Array/Map
    if is_list_source(col) {
        return crate::core::DataType::Array;
    }
    if is_map_source(col) {
        return crate::core::DataType::Map;
    }

    // Respect source string type: if the source column was stored as a string,
    // preserve it as String regardless of content analysis (numeric-looking strings
    // should remain strings to maintain fidelity with the source data).
    let source_is_string = matches!(
        col.source_arrow_type,
        Some(arrow::datatypes::DataType::Utf8) | Some(arrow::datatypes::DataType::LargeUtf8)
    );

    // UUID columns keep their type even as PK/FK
    if matches!(col.inferred_type, Some(InferredType::Uuid)) {
        return crate::core::DataType::Uuid;
    }
    if matches!(col.inferred_type, Some(InferredType::Boolean)) {
        // If source is string, keep as categorical string rather than bool
        if source_is_string {
            return crate::core::DataType::String;
        }
        return crate::core::DataType::Bool;
    }
    if matches!(col.inferred_type, Some(InferredType::Date(_))) {
        // String-encoded dates: preserve as datetime/date (these are genuine temporal values)
        return if col.has_time_component {
            resolve_datetime_type(col)
        } else {
            crate::core::DataType::Date
        };
    }
    if fk.is_some() || col.is_primary_key {
        // If the source was a string column (e.g. UUID FK), keep as String
        if source_is_string {
            return crate::core::DataType::String;
        }
        return if is_narrow_int_source(col) {
            crate::core::DataType::Int32
        } else {
            crate::core::DataType::Int
        };
    }
    if col.temporal_pattern.is_some() {
        return if col.has_time_component {
            resolve_datetime_type(col)
        } else {
            crate::core::DataType::Date
        };
    }
    if col.distribution.is_some() {
        // If source is string, keep as string even though content is numeric
        if source_is_string {
            return crate::core::DataType::String;
        }
        // Check if all values are whole numbers → Int
        if col.is_integer_valued {
            return if is_narrow_int_source(col) {
                crate::core::DataType::Int32
            } else {
                crate::core::DataType::Int
            };
        }
        return crate::core::DataType::Float;
    }
    if col.categorical_weights.is_some() {
        // If source is numeric (Int32/Int*), preserve the int type even for categoricals
        if is_narrow_int_source(col) {
            return crate::core::DataType::Int32;
        }
        if matches!(
            col.source_arrow_type,
            Some(arrow::datatypes::DataType::Int64)
                | Some(arrow::datatypes::DataType::UInt32)
                | Some(arrow::datatypes::DataType::UInt64)
        ) {
            return crate::core::DataType::Int;
        }
        return crate::core::DataType::String;
    }
    // Fallback for numeric source types that didn't match any other pattern
    // (e.g., constant-valued int32 columns with no variance for distribution fitting)
    if is_narrow_int_source(col) {
        return crate::core::DataType::Int32;
    }
    if matches!(
        col.source_arrow_type,
        Some(arrow::datatypes::DataType::Int64)
            | Some(arrow::datatypes::DataType::UInt32)
            | Some(arrow::datatypes::DataType::UInt64)
    ) {
        return crate::core::DataType::Int;
    }
    if matches!(
        col.source_arrow_type,
        Some(arrow::datatypes::DataType::Float32) | Some(arrow::datatypes::DataType::Float64)
    ) {
        return crate::core::DataType::Float;
    }
    crate::core::DataType::String
}

fn assemble_table(out: &mut String, table: &TableAnalysis) {
    writeln!(out, "entity {} {{", table.name).unwrap();

    for col in &table.columns {
        assemble_column(out, col, &table.relationships, &table.name);
    }

    writeln!(out, "}}").unwrap();
}

fn assemble_column(
    out: &mut String,
    col: &ColumnAnalysis,
    relationships: &[RelationshipCandidate],
    table_name: &str,
) {
    // Check if this column is a FK
    let fk = relationships
        .iter()
        .find(|r| r.from_column == col.name && r.from_table == table_name);

    let generator = if let Some(rel) = fk {
        format!("ref({}.{})", rel.to_table, rel.to_column)
    } else if col.is_primary_key {
        if matches!(col.inferred_type, Some(InferredType::Uuid)) {
            "uuid()".to_string()
        } else {
            "auto_increment()".to_string()
        }
    } else if let Some(spec) = &col.temporal_pattern {
        if spec.generator_expr.is_empty() {
            "timestamp()".to_string()
        } else {
            spec.generator_expr.clone()
        }
    } else if let Some(fit) = &col.distribution {
        distribution_to_generator(&fit.best.distribution)
    } else if let Some(weights) = &col.categorical_weights {
        categorical_to_generator(weights)
    } else if matches!(col.inferred_type, Some(InferredType::Uuid)) {
        "uuid()".to_string()
    } else if matches!(col.inferred_type, Some(InferredType::Boolean)) {
        "one_of(\"true\" => 50%, \"false\" => 50%)".to_string()
    } else if matches!(col.inferred_type, Some(InferredType::Date(_))) {
        "faker(\"date\")".to_string()
    } else if col
        .string_patterns
        .iter()
        .any(|(p, _)| *p == StringPattern::Email)
    {
        "faker(\"email\")".to_string()
    } else if col
        .string_patterns
        .iter()
        .any(|(p, _)| *p == StringPattern::Phone)
    {
        "faker(\"phone\")".to_string()
    } else if col
        .string_patterns
        .iter()
        .any(|(p, _)| *p == StringPattern::Name)
    {
        "faker(\"name\")".to_string()
    } else if col
        .string_patterns
        .iter()
        .any(|(p, _)| matches!(p, StringPattern::HexString(_)))
    {
        let len = col
            .string_patterns
            .iter()
            .find_map(|(p, _)| {
                if let StringPattern::HexString(n) = p {
                    Some(*n)
                } else {
                    None
                }
            })
            .unwrap_or(32);
        format!("faker(\"hex_string\", {})", len)
    } else {
        "unknown()".to_string()
    };

    let null_suffix = if col.null_rate > 0.01 {
        format!(" | null({})", format_pct(col.null_rate))
    } else {
        String::new()
    };

    // Confidence comment
    let comment = if col.confidence < 1.0 {
        format!("  # confidence: {:.0}%", col.confidence * 100.0)
    } else {
        String::new()
    };

    writeln!(
        out,
        "  {} = {}{}{}",
        col.name, generator, null_suffix, comment
    )
    .unwrap();
}

/// Map a fitted distribution to a Weave generator expression.
fn distribution_to_generator(dist: &Distribution) -> String {
    match dist {
        Distribution::Normal(mean, std_dev) => {
            format!("normal({:.2}, {:.2})", mean, std_dev)
        }
        Distribution::LogNormal(mu, sigma) => {
            format!("log_normal({:.2}, {:.2})", mu, sigma)
        }
        Distribution::Exponential(lambda) => {
            format!("exponential({:.4})", lambda)
        }
        Distribution::Uniform(min, max) => {
            format!("uniform({:.2}, {:.2})", min, max)
        }
        Distribution::Poisson(lambda) => {
            format!("poisson({:.2})", lambda)
        }
        Distribution::Beta(alpha, beta) => {
            format!("beta({:.2}, {:.2})", alpha, beta)
        }
        Distribution::Gamma(shape, rate) => {
            format!("gamma(shape={:.2}, scale={:.2})", shape, 1.0 / rate)
        }
        Distribution::Pareto(x_m, alpha) => {
            format!("pareto(scale={:.2}, shape={:.2})", x_m, alpha)
        }
        Distribution::Zipf(n, s) => {
            format!("zipf({}, {:.2})", n, s)
        }
    }
}

/// Map categorical weights to a `one_of(...)` generator.
fn categorical_to_generator(weights: &[(String, f64)]) -> String {
    if weights.is_empty() {
        return "unknown()".to_string();
    }
    let items: Vec<String> = weights
        .iter()
        .take(20) // Cap displayed items
        .map(|(val, w)| format!("\"{}\" => {:.0}%", val, w * 100.0))
        .collect();
    format!("one_of({})", items.join(", "))
}

fn format_pct(rate: f64) -> String {
    format!("{:.0}%", rate * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::fitting::CandidateFit;

    fn make_fit(dist: Distribution) -> FitResult {
        FitResult {
            best: CandidateFit {
                distribution: dist.clone(),
                ks_stat: 0.05,
                p_value: 0.5,
                aic: 100.0,
                bic: 105.0,
            },
            alternatives: vec![CandidateFit {
                distribution: dist,
                ks_stat: 0.05,
                p_value: 0.5,
                aic: 100.0,
                bic: 105.0,
            }],
        }
    }

    #[test]
    fn assemble_simple_schema() {
        let tables = vec![TableAnalysis {
            name: "users".into(),
            columns: vec![
                ColumnAnalysis {
                    name: "id".into(),
                    is_primary_key: true,
                    distribution: None,
                    temporal_pattern: None,
                    categorical_weights: None,
                    null_rate: 0.0,
                    confidence: 1.0,
                    inferred_type: None,
                    string_patterns: vec![],
                    is_integer_valued: false,
                    has_time_component: false,
                    temporal_range: None,
                    source_arrow_type: None,
                    max_decimal_places: None,
                    is_actor_column: false,
                },
                ColumnAnalysis {
                    name: "age".into(),
                    is_primary_key: false,
                    distribution: Some(make_fit(Distribution::Normal(30.0, 10.0))),
                    temporal_pattern: None,
                    categorical_weights: None,
                    null_rate: 0.02,
                    confidence: 0.85,
                    inferred_type: None,
                    string_patterns: vec![],
                    is_integer_valued: false,
                    has_time_component: false,
                    temporal_range: None,
                    source_arrow_type: None,
                    max_decimal_places: None,
                    is_actor_column: false,
                },
            ],
            relationships: vec![],
            correlations: vec![],
            row_count: 5000,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }];

        let schema = assemble_schema(&tables);
        assert!(schema.contains("entity users"));
        assert!(schema.contains("id = auto_increment()"));
        assert!(schema.contains("normal(30.00, 10.00)"));
        assert!(schema.contains("confidence: 85%"));
        assert!(schema.contains("null(2%)"));
    }

    #[test]
    fn assemble_with_fk() {
        let tables = vec![TableAnalysis {
            name: "orders".into(),
            columns: vec![ColumnAnalysis {
                name: "user_id".into(),
                is_primary_key: false,
                distribution: None,
                temporal_pattern: None,
                categorical_weights: None,
                null_rate: 0.0,
                confidence: 0.9,
                inferred_type: None,
                string_patterns: vec![],
                is_integer_valued: false,
                has_time_component: false,
                temporal_range: None,
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            }],
            relationships: vec![RelationshipCandidate {
                from_table: "orders".into(),
                from_column: "user_id".into(),
                to_table: "users".into(),
                to_column: "id".into(),
                kind: RelationshipKind::OneToMany,
                confidence: 0.9,
                is_self_ref: false,
            }],
            correlations: vec![],
            row_count: 1000,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }];

        let schema = assemble_schema(&tables);
        assert!(schema.contains("ref(users.id)"), "schema: {}", schema);
    }

    #[test]
    fn assemble_categorical() {
        let tables = vec![TableAnalysis {
            name: "items".into(),
            columns: vec![ColumnAnalysis {
                name: "status".into(),
                is_primary_key: false,
                distribution: None,
                temporal_pattern: None,
                categorical_weights: Some(vec![("active".into(), 0.7), ("inactive".into(), 0.3)]),
                null_rate: 0.0,
                confidence: 0.95,
                inferred_type: None,
                string_patterns: vec![],
                is_integer_valued: false,
                has_time_component: false,
                temporal_range: None,
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            }],
            relationships: vec![],
            correlations: vec![],
            row_count: 500,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }];

        let schema = assemble_schema(&tables);
        assert!(schema.contains("one_of("), "schema: {}", schema);
        assert!(schema.contains("active"));
    }

    #[test]
    fn assemble_temporal() {
        let tables = vec![TableAnalysis {
            name: "events".into(),
            columns: vec![ColumnAnalysis {
                name: "created_at".into(),
                is_primary_key: false,
                distribution: None,
                temporal_pattern: Some(crate::learn::temporal::TemporalPatternSpec {
                    pattern: crate::learn::temporal::TemporalPattern::FixedInterval {
                        interval_secs: 3600.0,
                    },
                    generator_expr: "time_series(interval=3600s)".into(),
                    confidence: 0.9,
                }),
                categorical_weights: None,
                null_rate: 0.0,
                confidence: 0.9,
                inferred_type: None,
                string_patterns: vec![],
                is_integer_valued: false,
                has_time_component: false,
                temporal_range: None,
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            }],
            relationships: vec![],
            correlations: vec![],
            row_count: 2000,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }];

        let schema = assemble_schema(&tables);
        assert!(
            schema.contains("time_series(interval=3600s)"),
            "schema: {}",
            schema
        );
    }

    #[test]
    fn assemble_empty() {
        let schema = assemble_schema(&[]);
        assert!(schema.contains("Auto-generated"));
    }

    #[test]
    fn distribution_generators() {
        assert!(distribution_to_generator(&Distribution::Normal(0.0, 1.0)).contains("normal"));
        assert!(distribution_to_generator(&Distribution::Exponential(0.5)).contains("exponential"));
        assert!(distribution_to_generator(&Distribution::Uniform(0.0, 100.0)).contains("uniform"));
        assert!(distribution_to_generator(&Distribution::Beta(2.0, 5.0)).contains("beta"));
        let gamma = distribution_to_generator(&Distribution::Gamma(1.0, 2.0));
        assert!(gamma.contains("gamma"), "gamma: {}", gamma);
        assert!(
            gamma.contains("shape="),
            "gamma should have shape param: {}",
            gamma
        );
        assert!(
            gamma.contains("scale="),
            "gamma should have scale param: {}",
            gamma
        );
        let pareto = distribution_to_generator(&Distribution::Pareto(1.0, 2.0));
        assert!(pareto.contains("pareto"), "pareto: {}", pareto);
        assert!(
            pareto.contains("scale="),
            "pareto should have scale param: {}",
            pareto
        );
        assert!(
            pareto.contains("shape="),
            "pareto should have shape param: {}",
            pareto
        );
    }

    #[test]
    fn build_distribution_generator_param_names() {
        use crate::learn::fitting::Distribution;

        // Gamma: shape/scale (rate converted to scale)
        let spec = build_distribution_generator(&Distribution::Gamma(2.0, 0.5), false);
        if let GeneratorSpec::Distribution { spec: ds } = &spec {
            assert_eq!(ds.kind, DistributionKind::Gamma);
            assert!(ds.params.contains_key("shape"), "Gamma missing shape param");
            assert!(ds.params.contains_key("scale"), "Gamma missing scale param");
            assert!((ds.params["shape"] - 2.0).abs() < 1e-10);
            assert!((ds.params["scale"] - 2.0).abs() < 1e-10); // scale = 1/rate = 1/0.5
        } else {
            panic!("Expected Distribution spec for Gamma");
        }

        // Pareto: scale/shape
        let spec = build_distribution_generator(&Distribution::Pareto(1.0, 3.0), false);
        if let GeneratorSpec::Distribution { spec: ds } = &spec {
            assert_eq!(ds.kind, DistributionKind::Pareto);
            assert!(
                ds.params.contains_key("scale"),
                "Pareto missing scale param"
            );
            assert!(
                ds.params.contains_key("shape"),
                "Pareto missing shape param"
            );
            assert!((ds.params["scale"] - 1.0).abs() < 1e-10);
            assert!((ds.params["shape"] - 3.0).abs() < 1e-10);
        } else {
            panic!("Expected Distribution spec for Pareto");
        }

        // Beta: alpha/beta (unchanged)
        let spec = build_distribution_generator(&Distribution::Beta(2.0, 5.0), false);
        if let GeneratorSpec::Distribution { spec: ds } = &spec {
            assert_eq!(ds.kind, DistributionKind::Beta);
            assert!(ds.params.contains_key("alpha"), "Beta missing alpha param");
            assert!(ds.params.contains_key("beta"), "Beta missing beta param");
        } else {
            panic!("Expected Distribution spec for Beta");
        }
    }

    #[test]
    fn row_count_preserved() {
        let tables = vec![TableAnalysis {
            name: "big_table".into(),
            columns: vec![ColumnAnalysis {
                name: "id".into(),
                is_primary_key: true,
                distribution: None,
                temporal_pattern: None,
                categorical_weights: None,
                null_rate: 0.0,
                confidence: 1.0,
                inferred_type: None,
                string_patterns: vec![],
                is_integer_valued: false,
                has_time_component: false,
                temporal_range: None,
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            }],
            relationships: vec![],
            correlations: vec![],
            row_count: 50_000,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }];

        let model = assemble_data_model("test", &tables);
        assert_eq!(model.entities[0].count, CountSpec::Fixed(50_000));
    }

    #[test]
    fn uuid_pk_uses_uuid_gen() {
        let tables = vec![TableAnalysis {
            name: "items".into(),
            columns: vec![ColumnAnalysis {
                name: "id".into(),
                is_primary_key: true,
                distribution: None,
                temporal_pattern: None,
                categorical_weights: None,
                null_rate: 0.0,
                confidence: 0.95,
                inferred_type: Some(InferredType::Uuid),
                string_patterns: vec![],
                is_integer_valued: false,
                has_time_component: false,
                temporal_range: None,
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            }],
            relationships: vec![],
            correlations: vec![],
            row_count: 100,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }];

        let model = assemble_data_model("test", &tables);
        let field = &model.entities[0].fields[0];
        assert!(
            matches!(field.generator, Some(GeneratorSpec::UuidGen { version: 4 })),
            "expected UuidGen, got {:?}",
            field.generator,
        );
        assert_eq!(field.data_type, crate::core::DataType::Uuid);
    }

    #[test]
    fn uuid_non_pk_uses_uuid_gen() {
        let col = ColumnAnalysis {
            name: "trace_id".into(),
            is_primary_key: false,
            distribution: None,
            temporal_pattern: None,
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.95,
            inferred_type: Some(InferredType::Uuid),
            string_patterns: vec![],
            is_integer_valued: false,
            has_time_component: false,
            temporal_range: None,
            source_arrow_type: None,
            max_decimal_places: None,
            is_actor_column: false,
        };
        let gen = build_generator(&col, None);
        assert!(matches!(gen, GeneratorSpec::UuidGen { version: 4 }));
    }

    #[test]
    fn boolean_uses_one_of() {
        let col = ColumnAnalysis {
            name: "active".into(),
            is_primary_key: false,
            distribution: None,
            temporal_pattern: None,
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.9,
            inferred_type: Some(InferredType::Boolean),
            string_patterns: vec![],
            is_integer_valued: false,
            has_time_component: false,
            temporal_range: None,
            source_arrow_type: None,
            max_decimal_places: None,
            is_actor_column: false,
        };
        let gen = build_generator(&col, None);
        assert!(matches!(gen, GeneratorSpec::OneOf { .. }));
    }

    #[test]
    fn email_pattern_uses_faker() {
        let col = ColumnAnalysis {
            name: "email".into(),
            is_primary_key: false,
            distribution: None,
            temporal_pattern: None,
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.9,
            inferred_type: Some(InferredType::Text),
            string_patterns: vec![(StringPattern::Email, 0.95)],
            is_integer_valued: false,
            has_time_component: false,
            temporal_range: None,
            source_arrow_type: None,
            max_decimal_places: None,
            is_actor_column: false,
        };
        let gen = build_generator(&col, None);
        assert!(
            matches!(gen, GeneratorSpec::Faker { ref method, .. } if method == "email"),
            "expected Faker(email), got {:?}",
            gen,
        );
    }

    #[test]
    fn phone_pattern_uses_faker() {
        let col = ColumnAnalysis {
            name: "phone".into(),
            is_primary_key: false,
            distribution: None,
            temporal_pattern: None,
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.9,
            inferred_type: Some(InferredType::Text),
            string_patterns: vec![(StringPattern::Phone, 0.9)],
            is_integer_valued: false,
            has_time_component: false,
            temporal_range: None,
            source_arrow_type: None,
            max_decimal_places: None,
            is_actor_column: false,
        };
        let gen = build_generator(&col, None);
        assert!(
            matches!(gen, GeneratorSpec::Faker { ref method, .. } if method == "phone"),
            "expected Faker(phone), got {:?}",
            gen,
        );
    }

    #[test]
    fn uuid_dsl_output() {
        let tables = vec![TableAnalysis {
            name: "traces".into(),
            columns: vec![ColumnAnalysis {
                name: "trace_id".into(),
                is_primary_key: false,
                distribution: None,
                temporal_pattern: None,
                categorical_weights: None,
                null_rate: 0.0,
                confidence: 0.95,
                inferred_type: Some(InferredType::Uuid),
                string_patterns: vec![],
                is_integer_valued: false,
                has_time_component: false,
                temporal_range: None,
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            }],
            relationships: vec![],
            correlations: vec![],
            row_count: 100,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }];
        let schema = assemble_schema(&tables);
        assert!(schema.contains("uuid()"), "schema: {}", schema);
    }

    #[test]
    fn email_dsl_output() {
        let tables = vec![TableAnalysis {
            name: "contacts".into(),
            columns: vec![ColumnAnalysis {
                name: "email".into(),
                is_primary_key: false,
                distribution: None,
                temporal_pattern: None,
                categorical_weights: None,
                null_rate: 0.0,
                confidence: 0.9,
                inferred_type: Some(InferredType::Text),
                string_patterns: vec![(StringPattern::Email, 0.95)],
                is_integer_valued: false,
                has_time_component: false,
                temporal_range: None,
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            }],
            relationships: vec![],
            correlations: vec![],
            row_count: 100,
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }];
        let schema = assemble_schema(&tables);
        assert!(schema.contains("faker(\"email\")"), "schema: {}", schema);
    }

    #[test]
    fn column_name_heuristic() {
        assert_eq!(faker_method_from_column_name("Address"), Some("address"));
        assert_eq!(
            faker_method_from_column_name("street_address"),
            Some("address")
        );
        assert_eq!(faker_method_from_column_name("City"), Some("city"));
        assert_eq!(faker_method_from_column_name("Country"), Some("country"));
        assert_eq!(
            faker_method_from_column_name("PostalCode"),
            Some("zip_code")
        );
        assert_eq!(faker_method_from_column_name("ZipCode"), Some("zip_code"));
        assert_eq!(
            faker_method_from_column_name("CompanyName"),
            Some("company")
        );
        assert_eq!(faker_method_from_column_name("company_url"), Some("url"));
        // Should NOT match non-semantic suffixes
        assert_eq!(faker_method_from_column_name("company_id"), None);
        assert_eq!(faker_method_from_column_name("is_company_verified"), None);
        assert_eq!(faker_method_from_column_name("company_count"), None);
        // Domain
        assert_eq!(faker_method_from_column_name("domain"), Some("domain"));
        assert_eq!(faker_method_from_column_name("domain_name"), Some("domain"));
        // Should not match arbitrary columns
        assert_eq!(faker_method_from_column_name("status"), None);
        assert_eq!(faker_method_from_column_name("created_at"), None);
    }

    #[test]
    fn infer_int32_from_narrow_source() {
        let col = ColumnAnalysis {
            name: "seconds".into(),
            is_primary_key: false,
            distribution: Some(make_fit(Distribution::Uniform(0.0, 86400.0))),
            temporal_pattern: None,
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.9,
            inferred_type: None,
            string_patterns: vec![],
            is_integer_valued: true,
            has_time_component: false,
            temporal_range: None,
            source_arrow_type: Some(arrow::datatypes::DataType::Int32),
            max_decimal_places: None,
            is_actor_column: false,
        };
        assert_eq!(infer_data_type(&col, None), crate::core::DataType::Int32);
    }

    #[test]
    fn infer_int64_from_wide_source() {
        let col = ColumnAnalysis {
            name: "big_val".into(),
            is_primary_key: false,
            distribution: Some(make_fit(Distribution::Uniform(0.0, 1e12))),
            temporal_pattern: None,
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.9,
            inferred_type: None,
            string_patterns: vec![],
            is_integer_valued: true,
            has_time_component: false,
            temporal_range: None,
            source_arrow_type: Some(arrow::datatypes::DataType::Int64),
            max_decimal_places: None,
            is_actor_column: false,
        };
        assert_eq!(infer_data_type(&col, None), crate::core::DataType::Int);
    }

    #[test]
    fn pk_preserves_int32_from_source() {
        let col = ColumnAnalysis {
            name: "id".into(),
            is_primary_key: true,
            distribution: None,
            temporal_pattern: None,
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.9,
            inferred_type: None,
            string_patterns: vec![],
            is_integer_valued: true,
            has_time_component: false,
            temporal_range: None,
            source_arrow_type: Some(arrow::datatypes::DataType::Int32),
            max_decimal_places: None,
            is_actor_column: false,
        };
        assert_eq!(infer_data_type(&col, None), crate::core::DataType::Int32);
    }

    #[test]
    fn temporal_preserves_microsecond_precision() {
        let col = ColumnAnalysis {
            name: "start_date".into(),
            is_primary_key: false,
            distribution: None,
            temporal_pattern: Some(crate::learn::temporal::TemporalPatternSpec {
                pattern: crate::learn::temporal::TemporalPattern::FixedInterval {
                    interval_secs: 86400.0,
                },
                generator_expr: "time_series(interval=86400s)".into(),
                confidence: 0.9,
            }),
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.9,
            inferred_type: None,
            string_patterns: vec![],
            is_integer_valued: false,
            has_time_component: true,
            temporal_range: Some((1700000000.0, 1710000000.0)),
            source_arrow_type: Some(arrow::datatypes::DataType::Timestamp(
                arrow::datatypes::TimeUnit::Microsecond,
                None,
            )),
            max_decimal_places: None,
            is_actor_column: false,
        };
        assert_eq!(infer_data_type(&col, None), crate::core::DataType::DatetimeUs);
    }

    #[test]
    fn temporal_defaults_to_nanosecond() {
        let col = ColumnAnalysis {
            name: "created_at".into(),
            is_primary_key: false,
            distribution: None,
            temporal_pattern: Some(crate::learn::temporal::TemporalPatternSpec {
                pattern: crate::learn::temporal::TemporalPattern::FixedInterval {
                    interval_secs: 86400.0,
                },
                generator_expr: "time_series(interval=86400s)".into(),
                confidence: 0.9,
            }),
            categorical_weights: None,
            null_rate: 0.0,
            confidence: 0.9,
            inferred_type: None,
            string_patterns: vec![],
            is_integer_valued: false,
            has_time_component: true,
            temporal_range: Some((1700000000.0, 1710000000.0)),
            source_arrow_type: Some(arrow::datatypes::DataType::Timestamp(
                arrow::datatypes::TimeUnit::Nanosecond,
                None,
            )),
            max_decimal_places: None,
            is_actor_column: false,
        };
        assert_eq!(infer_data_type(&col, None), crate::core::DataType::Datetime);
    }

    #[test]
    fn temporal_pair_detection_start_end_date() {
        let fields = vec![
            Field {
                name: "StartDate".into(),
                description: None,
                data_type: crate::core::DataType::Datetime,
                generator: Some(GeneratorSpec::Faker {
                    method: "datetime".into(),
                    args: vec![],
                }),
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
            },
            Field {
                name: "EndDate".into(),
                description: None,
                data_type: crate::core::DataType::Datetime,
                generator: Some(GeneratorSpec::Faker {
                    method: "datetime".into(),
                    args: vec![],
                }),
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
            },
        ];
        let pairs = find_temporal_pairs(&fields);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "StartDate");
        assert_eq!(pairs[0].1, 1); // EndDate index
    }

    #[test]
    fn temporal_pair_skips_non_temporal_fields() {
        let fields = vec![
            Field {
                name: "start_balance".into(),
                description: None,
                data_type: crate::core::DataType::Float,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
            },
            Field {
                name: "end_balance".into(),
                description: None,
                data_type: crate::core::DataType::Float,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
            },
        ];
        let pairs = find_temporal_pairs(&fields);
        assert!(pairs.is_empty(), "non-temporal fields should not be paired");
    }

    #[test]
    fn rewrite_temporal_pairs_uses_offset() {
        let cols = vec![
            ColumnAnalysis {
                name: "StartDate".into(),
                is_primary_key: false,
                distribution: None,
                temporal_pattern: None,
                categorical_weights: None,
                null_rate: 0.0,
                confidence: 0.9,
                inferred_type: None,
                string_patterns: vec![],
                is_integer_valued: false,
                has_time_component: true,
                temporal_range: Some((1_000_000.0, 1_100_000.0)),
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            },
            ColumnAnalysis {
                name: "EndDate".into(),
                is_primary_key: false,
                distribution: None,
                temporal_pattern: None,
                categorical_weights: None,
                null_rate: 0.0,
                confidence: 0.9,
                inferred_type: None,
                string_patterns: vec![],
                is_integer_valued: false,
                has_time_component: true,
                temporal_range: Some((1_050_000.0, 1_200_000.0)),
                source_arrow_type: None,
                max_decimal_places: None,
                is_actor_column: false,
            },
        ];
        let mut fields = vec![
            Field {
                name: "StartDate".into(),
                description: None,
                data_type: crate::core::DataType::Datetime,
                generator: Some(GeneratorSpec::Faker {
                    method: "datetime".into(),
                    args: vec![],
                }),
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
            },
            Field {
                name: "EndDate".into(),
                description: None,
                data_type: crate::core::DataType::Datetime,
                generator: Some(GeneratorSpec::Faker {
                    method: "datetime".into(),
                    args: vec![],
                }),
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
            },
        ];
        rewrite_temporal_pairs(&mut fields, &cols);

        // EndDate should now be Relative
        match &fields[1].generator {
            Some(GeneratorSpec::Relative { field, offset }) => {
                assert_eq!(field, "StartDate");
                // Midpoint diff: (1_125_000 - 1_050_000) = 75_000
                if let Value::Float(v) = offset {
                    assert!(*v > 0.0, "offset should be positive");
                } else {
                    panic!("expected Float offset");
                }
            }
            other => panic!("expected Relative generator, got {other:?}"),
        }
    }

    // ── Actor detection tests ───────────────────────────────────────

    #[test]
    fn score_actor_column_user_id() {
        assert_eq!(score_actor_column("user_id"), 0.95);
        assert_eq!(score_actor_column("customer_id"), 0.95);
        assert_eq!(score_actor_column("employee_id"), 0.95);
        assert_eq!(score_actor_column("User_ID"), 0.95);
    }

    #[test]
    fn score_actor_column_by_suffix() {
        assert_eq!(score_actor_column("created_by"), 0.85);
        assert_eq!(score_actor_column("assigned_by"), 0.85);
        assert_eq!(score_actor_column("approved_by"), 0.85);
    }

    #[test]
    fn score_actor_column_standalone() {
        assert_eq!(score_actor_column("sender"), 0.80);
        assert_eq!(score_actor_column("receiver"), 0.80);
        assert_eq!(score_actor_column("recipient"), 0.80);
    }

    #[test]
    fn score_actor_column_name_pattern() {
        assert_eq!(score_actor_column("user_name"), 0.70);
        assert_eq!(score_actor_column("author_name"), 0.70);
        assert_eq!(score_actor_column("user_email"), 0.70);
    }

    #[test]
    fn score_actor_column_non_actor() {
        assert_eq!(score_actor_column("order_id"), 0.0);
        assert_eq!(score_actor_column("amount"), 0.0);
        assert_eq!(score_actor_column("description"), 0.0);
        assert_eq!(score_actor_column("transaction_id"), 0.0);
    }

    #[test]
    fn detect_actor_columns_filters_by_threshold() {
        let columns = vec![
            ColumnAnalysis::new("user_id".into(), 0.0, 1.0),
            ColumnAnalysis::new("order_id".into(), 0.0, 1.0),
            ColumnAnalysis::new("created_by".into(), 0.0, 1.0),
            ColumnAnalysis::new("amount".into(), 0.0, 1.0),
        ];
        let results = detect_actor_columns(&columns);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "user_id");
        assert_eq!(results[1].0, "created_by");
    }

    #[test]
    fn is_actor_entity_by_table_name() {
        assert!(is_actor_entity("users", &[], &[]));
        assert!(is_actor_entity("employees", &[], &[]));
        assert!(is_actor_entity("customers", &[], &[]));
        assert!(!is_actor_entity("orders", &[], &[]));
        assert!(!is_actor_entity("transactions", &[], &[]));
    }

    #[test]
    fn is_actor_entity_by_pk_column() {
        let fields = vec![Field {
            name: "user_id".into(),
            description: None,
            data_type: crate::core::DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: Some(true),
            precision: None,
            actor_column: false,
            fields: vec![],
        }];
        let scores = vec![("user_id".to_string(), 0.95)];
        assert!(is_actor_entity("some_table", &scores, &fields));
    }

    #[test]
    fn build_entity_marks_actor_columns() {
        let columns = vec![
            {
                let mut col = ColumnAnalysis::new("sender_id".into(), 0.0, 1.0);
                col.is_primary_key = false;
                col
            },
            ColumnAnalysis::new("subject".into(), 0.0, 1.0),
        ];
        let table = TableAnalysis::new("emails".into(), columns, 100);
        let (entity, _, _) = build_entity(&table);
        let sender = entity
            .fields
            .iter()
            .find(|f| f.name == "sender_id")
            .unwrap();
        assert!(
            sender.actor_column,
            "sender_id should be marked as actor_column"
        );
        let subject = entity.fields.iter().find(|f| f.name == "subject").unwrap();
        assert!(!subject.actor_column, "subject should not be actor_column");
    }

    #[test]
    fn build_entity_marks_actor_entity() {
        let columns = vec![{
            let mut col = ColumnAnalysis::new("user_id".into(), 0.0, 1.0);
            col.is_primary_key = true;
            col
        }];
        let table = TableAnalysis::new("users".into(), columns, 100);
        let (entity, _, _) = build_entity(&table);
        assert!(entity.actor, "users entity should be marked as actor");
        let pk = entity.fields.iter().find(|f| f.name == "user_id").unwrap();
        assert!(
            !pk.actor_column,
            "PK on actor entity should not be marked actor_column"
        );
    }

    #[test]
    fn build_entity_non_actor_table() {
        let columns = vec![{
            let mut col = ColumnAnalysis::new("order_id".into(), 0.0, 1.0);
            col.is_primary_key = true;
            col
        }];
        let table = TableAnalysis::new("orders".into(), columns, 100);
        let (entity, _, _) = build_entity(&table);
        assert!(!entity.actor, "orders entity should not be marked as actor");
    }

    #[test]
    fn score_actor_column_by_rejects_false_positives() {
        // group_by, sort_by, order_by should NOT match
        assert_eq!(score_actor_column("group_by"), 0.0);
        assert_eq!(score_actor_column("sort_by"), 0.0);
        assert_eq!(score_actor_column("order_by"), 0.0);
        // But created_by, approved_by should match
        assert!(score_actor_column("created_by") > 0.8);
        assert!(score_actor_column("approved_by") > 0.8);
    }

    #[test]
    fn score_actor_column_standalone_roles() {
        // Roles from ACTOR_PREFIXES that are also standalone names
        assert!(score_actor_column("owner") > 0.7);
        assert!(score_actor_column("author") > 0.7);
        assert!(score_actor_column("reviewer") > 0.7);
        assert!(score_actor_column("manager") > 0.7);
        assert!(score_actor_column("creator") > 0.7);
        // from/to should not match (excluded for false positive risk)
        assert_eq!(score_actor_column("from"), 0.0);
        assert_eq!(score_actor_column("to"), 0.0);
    }

    #[test]
    fn is_actor_entity_prefixed_table_names() {
        // Tables like app_users, dim_customer, hr_employees should match
        assert!(is_actor_entity("app_users", &[], &[]));
        assert!(is_actor_entity("dim_customer", &[], &[]));
        assert!(is_actor_entity("hr_employees", &[], &[]));
        assert!(is_actor_entity("user_accounts", &[], &[]));
        // But not unrelated tables
        assert!(!is_actor_entity("app_orders", &[], &[]));
        assert!(!is_actor_entity("dim_product", &[], &[]));
    }

    // ── Schema emission (personas & relationships) ──────────────────

    #[test]
    fn assemble_emits_personas_for_actor_entity() {
        let mut table = TableAnalysis::new(
            "users".into(),
            vec![ColumnAnalysis::new("user_id".into(), 0.0, 0.95)],
            1000,
        );
        table.personas = vec![
            PersonaSpec {
                name: "power_user".into(),
                weight: 0.3,
                traits: BTreeMap::from([("activity_rate".into(), Value::Float(50.0))]),
            },
            PersonaSpec {
                name: "casual_user".into(),
                weight: 0.7,
                traits: BTreeMap::from([("activity_rate".into(), Value::Float(5.0))]),
            },
        ];

        let model = assemble_data_model("test", &[table]);

        assert_eq!(model.personas.len(), 2);
        assert_eq!(model.personas[0].name, "users_power_user");
        assert_eq!(model.personas[0].weight, 0.3);
        assert_eq!(model.personas[1].name, "users_casual_user");
        assert_eq!(model.personas[1].weight, 0.7);

        // Actor entity should have persona_distribution set
        let entity = &model.entities[0];
        assert!(entity.actor);
        assert_eq!(entity.persona_distribution, Some("personas".into()));
    }

    #[test]
    fn assemble_emits_actor_relationships() {
        use crate::learn::actor_graph::ActorRelationshipSpec;

        let mut table = TableAnalysis::new(
            "messages".into(),
            vec![
                ColumnAnalysis::new("sender_id".into(), 0.0, 0.9),
                ColumnAnalysis::new("receiver_id".into(), 0.0, 0.9),
            ],
            5000,
        );
        table.actor_relationships = vec![ActorRelationshipSpec {
            name: "messages_sender_id_receiver_id_network".into(),
            from_entity: "messages".into(),
            to_entity: "messages".into(),
            graph_type: crate::core::GraphType::SmallWorld,
            params: BTreeMap::from([("avg_degree".into(), 8.0), ("reciprocity".into(), 0.6)]),
            community_count: Some(3),
            hierarchy_depth: None,
        }];

        let model = assemble_data_model("test", &[table]);

        assert_eq!(model.actor_relationships.len(), 1);
        let rel = &model.actor_relationships[0];
        assert_eq!(rel.name, "messages_sender_id_receiver_id_network");
        assert_eq!(rel.from_entity, "messages");
        assert_eq!(rel.graph_type, crate::core::GraphType::SmallWorld);
        assert_eq!(rel.params.get("avg_degree"), Some(&8.0));
        assert_eq!(rel.community_count, Some(CountSpec::Fixed(3)));
        assert_eq!(rel.hierarchy_depth, None);
    }

    #[test]
    fn assemble_non_actor_table_no_persona_distribution() {
        let mut table = TableAnalysis::new(
            "orders".into(),
            vec![ColumnAnalysis::new("order_id".into(), 0.0, 0.95)],
            500,
        );
        // Personas on a non-actor table — should be skipped entirely
        table.personas = vec![PersonaSpec {
            name: "bulk_buyer".into(),
            weight: 1.0,
            traits: BTreeMap::new(),
        }];

        let model = assemble_data_model("test", &[table]);

        // Non-actor tables don't emit personas
        assert_eq!(model.personas.len(), 0);
        let entity = &model.entities[0];
        assert!(!entity.actor);
        assert_eq!(entity.persona_distribution, None);
    }
}
