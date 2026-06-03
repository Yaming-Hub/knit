//! Incremental learning: Arrow-to-state bridge and finalize logic.
//!
//! This module connects Arrow `RecordBatch` data to the streaming state,
//! and provides finalization that converts accumulated state into schema
//! analysis suitable for `schema_assembly`.

use arrow::array::Array;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;

use crate::learn::fitting::fit_distribution;
use crate::learn::schema_assembly::{ColumnAnalysis, TableAnalysis};
use crate::learn::streaming::state::{ColumnDataType, ColumnState, LearnState, TableState};
use crate::learn::temporal::detect_temporal_pattern;
use crate::learn::type_inference::InferredType;

/// Map an Arrow DataType to our simplified ColumnDataType.
pub fn arrow_to_column_type(dt: &DataType) -> ColumnDataType {
    match dt {
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => ColumnDataType::Integer,

        DataType::Float16 | DataType::Float32 | DataType::Float64 => ColumnDataType::Float,

        DataType::Utf8 | DataType::LargeUtf8 => ColumnDataType::String,

        DataType::Boolean => ColumnDataType::Boolean,

        DataType::Date32
        | DataType::Date64
        | DataType::Timestamp(_, _)
        | DataType::Time32(_)
        | DataType::Time64(_)
        | DataType::Duration(_) => ColumnDataType::Temporal,

        // Dictionary-encoded strings
        DataType::Dictionary(_, value_type) if is_string_type(value_type) => ColumnDataType::String,

        _ => ColumnDataType::Other,
    }
}

fn is_string_type(dt: &DataType) -> bool {
    matches!(dt, DataType::Utf8 | DataType::LargeUtf8)
}

/// Format an Arrow DataType as a string hint for finalize fidelity.
pub fn arrow_type_hint(dt: &DataType) -> String {
    format!("{dt:?}")
}

/// Ingest a set of RecordBatches into the state for a given table/entity.
///
/// This processes all batches from a single source file, updating column states
/// with observed values. Call this once per source file.
pub fn ingest_batches_to_state(
    state: &mut LearnState,
    entity: &str,
    batches: &[RecordBatch],
    source_path: &str,
) -> bool {
    if batches.is_empty() {
        return false;
    }

    let schema = batches[0].schema();
    let table = state.get_or_create_table(entity);

    // Ensure columns exist in state with correct types
    for field in schema.fields() {
        let col_type = arrow_to_column_type(field.data_type());
        let col = table.get_or_create_column(field.name(), col_type);
        col.set_arrow_type_hint(&arrow_type_hint(field.data_type()));
        col.widen_type(col_type);
    }

    // Track which columns were touched for chunk presence marking
    let mut seen_columns: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Process each batch
    let mut total_rows: u64 = 0;
    for batch in batches {
        let num_rows = batch.num_rows();
        total_rows += num_rows as u64;

        for (col_idx, field) in batch.schema().fields().iter().enumerate() {
            let array = batch.column(col_idx);
            let col =
                table.get_or_create_column(field.name(), arrow_to_column_type(field.data_type()));
            update_column_from_array(col, array, field.data_type());
            seen_columns.insert(field.name().to_string());
        }
    }

    // Mark chunk presence for all columns that were processed
    for col in &mut table.columns {
        if seen_columns.contains(&col.name) {
            col.mark_chunk_present();
        }
    }

    table.add_rows(total_rows);

    // Record chunk (once per source file)

    state.record_chunk(source_path, total_rows)
}

/// Update relationship evidence in the state after ingestion.
///
/// This performs Stage 1 (candidate detection) and Stage 2 (HLL evidence update)
/// for cross-table relationship tracking. Call after all tables from a source
/// have been ingested.
pub fn update_relationship_evidence(state: &mut LearnState) {
    use crate::learn::streaming::relationships::{IncrementalRelColumn, detect_candidates};

    // Build column metadata for candidate detection
    let mut all_columns: Vec<IncrementalRelColumn> = Vec::new();
    for (table_name, table_state) in &state.tables {
        for col in &table_state.columns {
            let is_likely_pk = is_likely_pk_column(col, table_state.row_count);
            all_columns.push(IncrementalRelColumn {
                name: col.name.clone(),
                is_likely_pk,
                table_name: table_name.clone(),
            });
        }
    }

    // Stage 1: Detect new candidates
    let new_candidates = detect_candidates(&all_columns, &state.relationship_evidence);
    state.relationship_evidence.extend(new_candidates);

    // Stage 2: Update HLL sketches by merging column HLLs directly
    for ev in &mut state.relationship_evidence {
        if let Some(table) = state.tables.get(&ev.from_table)
            && let Some(col) = table.columns.iter().find(|c| c.name == ev.from_column)
        {
            ev.from_hll.merge(&col.hll);
        }
        if let Some(table) = state.tables.get(&ev.to_table)
            && let Some(col) = table.columns.iter().find(|c| c.name == ev.to_column)
        {
            ev.to_hll.merge(&col.hll);
        }
        ev.chunks_observed += 1;
    }
}

/// Update per-table pairwise numeric correlation evidence from Arrow batches.
///
/// For each numeric column pair in the table, extracts paired non-null values
/// and feeds them into a `PairwiseCorrelation` tracker stored on `TableState`.
/// Pair keys are canonicalized (lexicographic order) to avoid duplicates.
pub fn update_correlation_evidence(state: &mut LearnState, entity: &str, batches: &[RecordBatch]) {
    use crate::learn::streaming::relationships::PairwiseCorrelation;
    use arrow::array::{
        Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, UInt8Array,
        UInt16Array, UInt32Array, UInt64Array,
    };

    if batches.is_empty() {
        return;
    }

    let table = match state.tables.get_mut(entity) {
        Some(t) => t,
        None => return,
    };

    // Identify numeric columns by index
    let schema = batches[0].schema();
    let numeric_indices: Vec<(usize, String)> = schema
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            matches!(
                arrow_to_column_type(f.data_type()),
                ColumnDataType::Integer | ColumnDataType::Float
            )
        })
        .map(|(i, f)| (i, f.name().clone()))
        .collect();

    if numeric_indices.len() < 2 {
        return;
    }

    // For each pair, update PairwiseCorrelation
    for i in 0..numeric_indices.len() {
        for j in (i + 1)..numeric_indices.len() {
            let (idx_a, ref name_a) = numeric_indices[i];
            let (idx_b, ref name_b) = numeric_indices[j];

            // Canonicalize pair key (lexicographic order)
            let (canon_a, canon_b, swap) = if name_a <= name_b {
                (name_a.clone(), name_b.clone(), false)
            } else {
                (name_b.clone(), name_a.clone(), true)
            };

            // Find or create tracker
            let tracker_pos = table
                .correlations
                .iter()
                .position(|pc| pc.col_a == canon_a && pc.col_b == canon_b);
            let tracker_idx = match tracker_pos {
                Some(idx) => idx,
                None => {
                    table
                        .correlations
                        .push(PairwiseCorrelation::new(canon_a.clone(), canon_b.clone()));
                    table.correlations.len() - 1
                }
            };

            // Extract paired non-null values from all batches
            for batch in batches {
                let arr_a = batch.column(idx_a);
                let arr_b = batch.column(idx_b);
                let len = arr_a.len().min(arr_b.len());

                for row in 0..len {
                    if arr_a.is_null(row) || arr_b.is_null(row) {
                        continue;
                    }
                    let va = extract_f64(arr_a, row);
                    let vb = extract_f64(arr_b, row);
                    if let (Some(a), Some(b)) = (va, vb) {
                        // Skip NaN/infinity to avoid corrupting accumulators
                        // (matches batch mode's paired_finite() filtering)
                        if !a.is_finite() || !b.is_finite() {
                            continue;
                        }
                        let (fa, fb) = if swap { (b, a) } else { (a, b) };
                        table.correlations[tracker_idx].update(fa, fb);
                    }
                }
            }
        }
    }

    /// Extract a numeric value as f64 from an Arrow array at the given row.
    fn extract_f64(array: &dyn Array, row: usize) -> Option<f64> {
        if let Some(a) = array.as_any().downcast_ref::<Float64Array>() {
            return Some(a.value(row));
        }
        if let Some(a) = array.as_any().downcast_ref::<Float32Array>() {
            return Some(a.value(row) as f64);
        }
        if let Some(a) = array.as_any().downcast_ref::<Int64Array>() {
            return Some(a.value(row) as f64);
        }
        if let Some(a) = array.as_any().downcast_ref::<Int32Array>() {
            return Some(a.value(row) as f64);
        }
        if let Some(a) = array.as_any().downcast_ref::<Int16Array>() {
            return Some(a.value(row) as f64);
        }
        if let Some(a) = array.as_any().downcast_ref::<Int8Array>() {
            return Some(a.value(row) as f64);
        }
        if let Some(a) = array.as_any().downcast_ref::<UInt64Array>() {
            return Some(a.value(row) as f64);
        }
        if let Some(a) = array.as_any().downcast_ref::<UInt32Array>() {
            return Some(a.value(row) as f64);
        }
        if let Some(a) = array.as_any().downcast_ref::<UInt16Array>() {
            return Some(a.value(row) as f64);
        }
        if let Some(a) = array.as_any().downcast_ref::<UInt8Array>() {
            return Some(a.value(row) as f64);
        }
        None
    }
}
fn is_likely_pk_column(col: &ColumnState, table_row_count: u64) -> bool {
    if table_row_count == 0 || col.count == 0 {
        return false;
    }
    let cardinality = col.hll.cardinality();
    let uniqueness_ratio = cardinality / col.count as f64;
    // High uniqueness + low null rate + name heuristic
    let lower = col.name.to_lowercase();
    let has_pk_name = lower == "id"
        || lower.ends_with("_id")
        || col.name.ends_with("Id")  // camelCase: userId, orderId
        || col.name.ends_with("ID"); // ALL_CAPS: userID
    uniqueness_ratio > 0.95 && col.null_rate() < 0.01 && has_pk_name
}

/// Update a ColumnState from an Arrow array.
fn update_column_from_array(col: &mut ColumnState, array: &dyn Array, dt: &DataType) {
    let len = array.len();

    match dt {
        DataType::Boolean => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::BooleanArray>()
                .expect("Boolean array must downcast to BooleanArray");
            for i in 0..len {
                if arr.is_null(i) {
                    col.update_null();
                } else {
                    let v = if arr.value(i) { "true" } else { "false" };
                    col.update_string(v);
                }
            }
        }

        DataType::Int8 => update_int_array_i64::<arrow::datatypes::Int8Type>(col, array),
        DataType::Int16 => update_int_array_i64::<arrow::datatypes::Int16Type>(col, array),
        DataType::Int32 => update_int_array_i64::<arrow::datatypes::Int32Type>(col, array),
        DataType::Int64 => update_int_array_i64::<arrow::datatypes::Int64Type>(col, array),
        DataType::UInt8 => update_int_array_i64::<arrow::datatypes::UInt8Type>(col, array),
        DataType::UInt16 => update_int_array_i64::<arrow::datatypes::UInt16Type>(col, array),
        DataType::UInt32 => update_int_array_i64::<arrow::datatypes::UInt32Type>(col, array),
        DataType::UInt64 => update_int_array_i64::<arrow::datatypes::UInt64Type>(col, array),

        DataType::Float16 | DataType::Float32 | DataType::Float64 => {
            update_float_array(col, array, dt);
        }

        DataType::Utf8 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .expect("Utf8 array must downcast to StringArray");
            for i in 0..len {
                if arr.is_null(i) {
                    col.update_null();
                } else {
                    col.update_string(arr.value(i));
                }
            }
        }

        DataType::LargeUtf8 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::LargeStringArray>()
                .expect("LargeUtf8 array must downcast to LargeStringArray");
            for i in 0..len {
                if arr.is_null(i) {
                    col.update_null();
                } else {
                    col.update_string(arr.value(i));
                }
            }
        }

        DataType::Date32 | DataType::Date64 | DataType::Timestamp(_, _) => {
            update_temporal_array(col, array, dt);
        }

        // Treat other types as string via display
        _ => {
            for i in 0..len {
                if array.is_null(i) {
                    col.update_null();
                } else {
                    let s =
                        arrow::util::display::array_value_to_string(array, i).unwrap_or_default();
                    col.update_string(&s);
                }
            }
        }
    }
}

fn update_int_array_i64<T>(col: &mut ColumnState, array: &dyn Array)
where
    T: arrow::datatypes::ArrowPrimitiveType,
    T::Native: std::fmt::Display + Copy,
{
    let arr = array
        .as_any()
        .downcast_ref::<arrow::array::PrimitiveArray<T>>()
        .expect("primitive array must downcast to PrimitiveArray<T>");
    for i in 0..arr.len() {
        if arr.is_null(i) {
            col.update_null();
        } else {
            let s = arr.value(i).to_string();
            let f: f64 = s.parse().unwrap_or(0.0);
            col.update_numeric(f, &s);
        }
    }
}

fn update_float_array(col: &mut ColumnState, array: &dyn Array, dt: &DataType) {
    match dt {
        DataType::Float32 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::Float32Array>()
                .expect("Float32 array must downcast to Float32Array");
            for i in 0..arr.len() {
                if arr.is_null(i) {
                    col.update_null();
                } else {
                    let v = arr.value(i) as f64;
                    let s = format!("{}", arr.value(i));
                    col.update_numeric(v, &s);
                }
            }
        }
        DataType::Float64 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::Float64Array>()
                .expect("Float64 array must downcast to Float64Array");
            for i in 0..arr.len() {
                if arr.is_null(i) {
                    col.update_null();
                } else {
                    let v = arr.value(i);
                    let s = format!("{v}");
                    col.update_numeric(v, &s);
                }
            }
        }
        _ => {} // Float16 handled by generic path
    }
}

fn update_temporal_array(col: &mut ColumnState, array: &dyn Array, dt: &DataType) {
    // Convert temporal values to epoch seconds (f64) for numeric tracking
    // and ISO string for HLL/reservoir/topk
    match dt {
        DataType::Date32 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::Date32Array>()
                .expect("Date32 array must downcast to Date32Array");
            for i in 0..arr.len() {
                if arr.is_null(i) {
                    col.update_null();
                } else {
                    let days = arr.value(i) as f64;
                    let secs = days * 86400.0;
                    let s = format!("{}", arr.value(i));
                    col.update_numeric(secs, &s);
                }
            }
        }
        DataType::Date64 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::Date64Array>()
                .expect("Date64 array must downcast to Date64Array");
            for i in 0..arr.len() {
                if arr.is_null(i) {
                    col.update_null();
                } else {
                    let ms = arr.value(i) as f64;
                    let secs = ms / 1000.0;
                    let s = format!("{}", arr.value(i));
                    col.update_numeric(secs, &s);
                }
            }
        }
        DataType::Timestamp(_unit, _) => {
            // Try each timestamp type to get epoch seconds
            for i in 0..array.len() {
                if array.is_null(i) {
                    col.update_null();
                } else {
                    let s =
                        arrow::util::display::array_value_to_string(array, i).unwrap_or_default();
                    if let Some(ts_arr) = array.as_any().downcast_ref::<arrow::array::PrimitiveArray<arrow::datatypes::TimestampSecondType>>() {
                        let secs = ts_arr.value(i) as f64;
                        col.update_numeric(secs, &s);
                    } else if let Some(ts_arr) = array.as_any().downcast_ref::<arrow::array::PrimitiveArray<arrow::datatypes::TimestampMillisecondType>>() {
                        let secs = ts_arr.value(i) as f64 / 1000.0;
                        col.update_numeric(secs, &s);
                    } else if let Some(ts_arr) = array.as_any().downcast_ref::<arrow::array::PrimitiveArray<arrow::datatypes::TimestampMicrosecondType>>() {
                        let secs = ts_arr.value(i) as f64 / 1_000_000.0;
                        col.update_numeric(secs, &s);
                    } else if let Some(ts_arr) = array.as_any().downcast_ref::<arrow::array::PrimitiveArray<arrow::datatypes::TimestampNanosecondType>>() {
                        let secs = ts_arr.value(i) as f64 / 1_000_000_000.0;
                        col.update_numeric(secs, &s);
                    } else {
                        col.update_string(&s);
                    }
                }
            }
        }
        _ => {
            // Other temporal types: use display
            for i in 0..array.len() {
                if array.is_null(i) {
                    col.update_null();
                } else {
                    let s =
                        arrow::util::display::array_value_to_string(array, i).unwrap_or_default();
                    col.update_string(&s);
                }
            }
        }
    }
}

/// Finalize a LearnState into a vector of TableAnalysis for schema assembly.
///
/// This is the incremental equivalent of the batch-mode per-table analysis:
/// it uses reservoir samples for distribution fitting and top-K for categorical
/// detection, producing the same `TableAnalysis` structures that
/// `assemble_data_model()` expects.
///
/// Also returns finalized relationships derived from HLL evidence.
pub fn finalize_state(
    state: &LearnState,
) -> (
    Vec<TableAnalysis>,
    Vec<crate::learn::streaming::FinalizedRelationship>,
) {
    use crate::learn::correlation::{Correlation, CorrelationMethod, pearson_p_value};

    let mut analyses = Vec::new();

    for (entity_name, table_state) in &state.tables {
        let col_analyses = finalize_columns(table_state);
        let mut analysis =
            TableAnalysis::new(entity_name.clone(), col_analyses, table_state.row_count);

        // Finalize per-table correlations from streaming PairwiseCorrelation trackers
        let mut corrs: Vec<Correlation> = table_state
            .correlations
            .iter()
            .filter_map(|pc| {
                let r = pc.pearson_r()?;
                if r.abs() < 0.3 {
                    return None;
                }
                let p = pearson_p_value(r, pc.count as usize);
                if p >= 0.05 {
                    return None;
                }
                Some(Correlation {
                    column_a: pc.col_a.clone(),
                    column_b: pc.col_b.clone(),
                    method: CorrelationMethod::Pearson,
                    coefficient: r,
                    p_value: p,
                })
            })
            .collect();
        corrs.truncate(500);
        analysis.correlations = corrs;

        analyses.push(analysis);
    }

    let relationships = crate::learn::streaming::relationships::finalize_relationships(
        &state.relationship_evidence,
    );

    (analyses, relationships)
}

/// Finalize all columns of a single table.
fn finalize_columns(table: &TableState) -> Vec<ColumnAnalysis> {
    table.columns.iter().map(finalize_column).collect()
}

/// Convert a single ColumnState into a ColumnAnalysis for schema assembly.
fn finalize_column(col: &ColumnState) -> ColumnAnalysis {
    let null_rate = col.null_rate();
    let empty_string_rate = col.empty_string_rate();
    let mut ca = ColumnAnalysis::new(col.name.clone(), null_rate, 1.0);
    ca.empty_string_rate = empty_string_rate;

    // Restore Arrow type hint
    if let Some(ref hint) = col.arrow_type_hint {
        ca.source_arrow_type = parse_arrow_type_hint(hint);
    }

    match col.data_type {
        ColumnDataType::Boolean => {
            ca.inferred_type = Some(InferredType::Boolean);
            // Use top-K to get true/false ratio
            let items = col.top_k.top_items();
            let total: u64 = items.iter().map(|(_, c)| *c).sum();
            if total > 0 {
                let mut weights = Vec::new();
                for (val, count) in &items {
                    weights.push((val.clone(), *count as f64 / total as f64));
                }
                weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                ca.categorical_weights = Some(weights);
            }
        }

        ColumnDataType::Integer | ColumnDataType::Float => {
            ca.is_integer_valued = col.all_integer;
            ca.max_decimal_places = col.effective_precision();

            // Check if low-cardinality → categorical
            let estimated_distinct = col.hll.cardinality() as u64;
            if col.all_integer && estimated_distinct <= 20 {
                // Use top-K for categorical weights
                let items = col.top_k.top_items();
                let total: u64 = items.iter().map(|(_, c)| *c).sum();
                if total > 0 {
                    let mut weights: Vec<(String, f64)> = items
                        .iter()
                        .map(|(v, c)| (v.clone(), *c as f64 / total as f64))
                        .collect();
                    weights
                        .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    ca.categorical_weights = Some(weights);
                    ca.inferred_type = Some(InferredType::Categorical);
                }
            } else {
                // Use reservoir sample for distribution fitting
                let samples: Vec<f64> = col
                    .reservoir
                    .items()
                    .iter()
                    .filter_map(|s| s.parse::<f64>().ok())
                    .collect();
                if !samples.is_empty() {
                    // Detect zero-inflated columns: if >50% of values are exactly 0,
                    // fit the distribution on non-zero values only for better parameters.
                    let zero_count = samples.iter().filter(|&&v| v == 0.0).count();
                    let zero_rate = zero_count as f64 / samples.len() as f64;
                    if zero_rate > 0.5 {
                        let non_zero: Vec<f64> =
                            samples.iter().copied().filter(|&v| v != 0.0).collect();
                        if non_zero.len() >= 2 {
                            ca.zero_rate = Some(zero_rate);
                            ca.distribution = fit_distribution(&non_zero);
                        } else {
                            ca.distribution = fit_distribution(&samples);
                        }
                    } else {
                        ca.distribution = fit_distribution(&samples);
                    }
                    if let Some(ref fit) = ca.distribution {
                        ca.confidence = (1.0 - fit.best.ks_stat).max(0.0);
                    }
                }
            }
        }

        ColumnDataType::Temporal => {
            // Use reservoir sample for temporal pattern detection
            let samples: Vec<f64> = col
                .reservoir
                .items()
                .iter()
                .filter_map(|s| s.parse::<f64>().ok())
                .collect();
            if !samples.is_empty() {
                ca.temporal_pattern = detect_temporal_pattern(&samples);
            }
            // If no pattern detected, fall back to distribution
            if ca.temporal_pattern.is_none() && !samples.is_empty() {
                ca.distribution = fit_distribution(&samples);
            }
            // Mark as temporal from Arrow type hint
            ca.has_time_component = col
                .arrow_type_hint
                .as_deref()
                .is_some_and(|h| h.contains("Timestamp"));
        }

        ColumnDataType::String => {
            // Use top-K for categorical detection
            let estimated_distinct = col.hll.cardinality() as u64;
            let items = col.top_k.top_items();

            if estimated_distinct <= 50 && !items.is_empty() {
                // Likely categorical
                let total: u64 = items.iter().map(|(_, c)| *c).sum();
                if total > 0 {
                    let mut weights: Vec<(String, f64)> = items
                        .iter()
                        .map(|(v, c)| (v.clone(), *c as f64 / total as f64))
                        .collect();
                    weights
                        .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    ca.categorical_weights = Some(weights);
                    ca.inferred_type = Some(InferredType::Categorical);
                }
            } else {
                // High cardinality string — try type inference on reservoir samples
                let samples = col.reservoir.items();
                if !samples.is_empty() {
                    let sample_opts: Vec<Option<&str>> =
                        samples.iter().map(|s| Some(s.as_str())).collect();
                    let inference = crate::learn::type_inference::infer_type(&sample_opts, 0.3);
                    ca.inferred_type = Some(inference.inferred_type);
                    ca.confidence = inference.confidence;
                    // Copy detected patterns
                    ca.string_patterns = inference.patterns.into_iter().collect();
                }
            }
        }

        ColumnDataType::Other => {
            // Treat as categorical from top-K
            let items = col.top_k.top_items();
            if !items.is_empty() {
                let total: u64 = items.iter().map(|(_, c)| *c).sum();
                let mut weights: Vec<(String, f64)> = items
                    .iter()
                    .map(|(v, c)| (v.clone(), *c as f64 / total as f64))
                    .collect();
                weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                ca.categorical_weights = Some(weights);
            }
        }
    }

    ca
}

/// Best-effort parse of an Arrow type hint string back to DataType.
/// Only covers common cases needed for fidelity.
fn parse_arrow_type_hint(hint: &str) -> Option<DataType> {
    // Match common patterns from Debug output
    match hint {
        "Int8" => Some(DataType::Int8),
        "Int16" => Some(DataType::Int16),
        "Int32" => Some(DataType::Int32),
        "Int64" => Some(DataType::Int64),
        "UInt8" => Some(DataType::UInt8),
        "UInt16" => Some(DataType::UInt16),
        "UInt32" => Some(DataType::UInt32),
        "UInt64" => Some(DataType::UInt64),
        "Float32" => Some(DataType::Float32),
        "Float64" => Some(DataType::Float64),
        "Utf8" => Some(DataType::Utf8),
        "LargeUtf8" => Some(DataType::LargeUtf8),
        "Boolean" => Some(DataType::Boolean),
        "Date32" => Some(DataType::Date32),
        "Date64" => Some(DataType::Date64),
        _ if hint.starts_with("Timestamp") => {
            // e.g. "Timestamp(Nanosecond, None)"
            use arrow::datatypes::TimeUnit;
            let unit = if hint.contains("Nanosecond") {
                TimeUnit::Nanosecond
            } else if hint.contains("Microsecond") {
                TimeUnit::Microsecond
            } else if hint.contains("Millisecond") {
                TimeUnit::Millisecond
            } else {
                TimeUnit::Second
            };
            // Extract timezone if present
            let tz = if hint.contains("Some(") {
                hint.split("Some(\"")
                    .nth(1)
                    .and_then(|s| s.split('"').next())
                    .map(|s| s.into())
            } else {
                None
            };
            Some(DataType::Timestamp(unit, tz))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Float64Array, Int32Array, StringArray};
    use arrow::datatypes::{Field, Schema};
    use std::sync::Arc;

    fn make_test_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("name", DataType::Utf8, true),
            Field::new("score", DataType::Float64, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3, 4, 5])),
                Arc::new(StringArray::from(vec![
                    Some("alice"),
                    Some("bob"),
                    None,
                    Some("alice"),
                    Some("carol"),
                ])),
                Arc::new(Float64Array::from(vec![
                    Some(95.5),
                    Some(87.3),
                    Some(92.0),
                    None,
                    Some(88.1),
                ])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn test_ingest_batches_creates_columns() {
        let mut state = LearnState::new(42);
        let batch = make_test_batch();
        let dup = ingest_batches_to_state(&mut state, "users", &[batch], "test.csv");

        assert!(!dup);
        let table = state.tables.get("users").unwrap();
        assert_eq!(table.columns.len(), 3);
        assert_eq!(table.row_count, 5);

        let id_col = &table.columns[0];
        assert_eq!(id_col.name, "id");
        assert_eq!(id_col.data_type, ColumnDataType::Integer);
        assert_eq!(id_col.count, 5);
        assert_eq!(id_col.null_count, 0);
        assert!(id_col.all_integer);

        let name_col = &table.columns[1];
        assert_eq!(name_col.name, "name");
        assert_eq!(name_col.data_type, ColumnDataType::String);
        assert_eq!(name_col.count, 4);
        assert_eq!(name_col.null_count, 1);

        let score_col = &table.columns[2];
        assert_eq!(score_col.name, "score");
        assert_eq!(score_col.data_type, ColumnDataType::Float);
        assert_eq!(score_col.count, 4);
        assert_eq!(score_col.null_count, 1);
        assert!(!score_col.all_integer); // has fractional values
    }

    #[test]
    fn test_ingest_detects_duplicate() {
        let mut state = LearnState::new(42);
        let batch = make_test_batch();
        let dup1 = ingest_batches_to_state(
            &mut state,
            "users",
            std::slice::from_ref(&batch),
            "test.csv",
        );
        let dup2 = ingest_batches_to_state(&mut state, "users", &[batch], "test.csv");

        assert!(!dup1);
        assert!(dup2);
    }

    #[test]
    fn test_ingest_multiple_batches() {
        let mut state = LearnState::new(42);
        let batch = make_test_batch();
        ingest_batches_to_state(&mut state, "users", &[batch.clone(), batch], "big.csv");

        let table = state.tables.get("users").unwrap();
        assert_eq!(table.row_count, 10);
        assert_eq!(table.columns[0].count, 10); // id
        assert_eq!(table.columns[0].chunks_present, 1); // one source file
    }

    #[test]
    fn test_finalize_produces_analyses() {
        let mut state = LearnState::new(42);
        let batch = make_test_batch();
        ingest_batches_to_state(&mut state, "users", &[batch], "test.csv");

        let (analyses, _rels) = finalize_state(&state);
        assert_eq!(analyses.len(), 1);
        assert_eq!(analyses[0].name, "users");
        assert_eq!(analyses[0].columns.len(), 3);
        assert_eq!(analyses[0].row_count, 5);
    }

    #[test]
    fn test_finalize_categorical_detection() {
        // Create a batch with low-cardinality integer column
        let schema = Arc::new(Schema::new(vec![Field::new(
            "status",
            DataType::Int32,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![1, 2, 1, 1, 2, 3, 1, 2]))],
        )
        .unwrap();

        let mut state = LearnState::new(42);
        ingest_batches_to_state(&mut state, "orders", &[batch], "orders.csv");

        let (analyses, _rels) = finalize_state(&state);
        let col = &analyses[0].columns[0];
        assert_eq!(col.inferred_type, Some(InferredType::Categorical));
        assert!(col.categorical_weights.is_some());
    }

    #[test]
    fn test_arrow_to_column_type() {
        assert_eq!(
            arrow_to_column_type(&DataType::Int32),
            ColumnDataType::Integer
        );
        assert_eq!(
            arrow_to_column_type(&DataType::Float64),
            ColumnDataType::Float
        );
        assert_eq!(
            arrow_to_column_type(&DataType::Utf8),
            ColumnDataType::String
        );
        assert_eq!(
            arrow_to_column_type(&DataType::Boolean),
            ColumnDataType::Boolean
        );
        assert_eq!(
            arrow_to_column_type(&DataType::Timestamp(
                arrow::datatypes::TimeUnit::Second,
                None
            )),
            ColumnDataType::Temporal
        );
    }

    #[test]
    fn test_parse_arrow_type_hint() {
        assert_eq!(parse_arrow_type_hint("Int32"), Some(DataType::Int32));
        assert_eq!(parse_arrow_type_hint("Float64"), Some(DataType::Float64));
        assert_eq!(parse_arrow_type_hint("Utf8"), Some(DataType::Utf8));
        assert_eq!(
            parse_arrow_type_hint("Timestamp(Nanosecond, None)"),
            Some(DataType::Timestamp(
                arrow::datatypes::TimeUnit::Nanosecond,
                None
            ))
        );
        assert_eq!(parse_arrow_type_hint("Unknown"), None);
    }
}
