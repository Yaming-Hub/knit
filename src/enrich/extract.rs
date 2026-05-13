//! Statistical knowledge extraction from reference data.

use arrow::array::{Array, AsArray};
use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

use crate::learn::fitting::{fit_categorical, fit_distribution, CategoricalFit, FitResult};
use crate::learn::profile::ColumnProfile;

/// Extracted enrichment data for a single field.
#[derive(Debug, Clone)]
pub struct FieldEnrichment {
    /// Numeric distribution fit (if field is numeric).
    pub distribution: Option<FitResult>,
    /// Categorical frequency fit (if field is categorical string).
    pub categorical: Option<CategoricalFit>,
    /// Observed null rate in the reference data.
    pub null_rate: f64,
    /// Number of non-null values in the reference sample.
    pub sample_size: u64,
}

/// Extract enrichment information from a profiled reference column.
pub fn extract_field_enrichment(
    profile: &ColumnProfile,
    batches: &[RecordBatch],
    _schema: &Arc<Schema>,
    col_index: usize,
) -> FieldEnrichment {
    let null_rate = profile.null_rate;
    let sample_size = profile.count - profile.null_count;

    // Try numeric distribution fit
    let distribution = if profile.numeric.is_some() {
        let values = extract_numeric_values(batches, col_index);
        if values.len() >= 5 {
            fit_distribution(&values)
        } else {
            None
        }
    } else {
        None
    };

    // Try categorical fit
    let categorical = if profile.string.is_some() && profile.cardinality_ratio.unwrap_or(1.0) < 0.5
    {
        let values = extract_string_values(batches, col_index);
        if values.len() >= 2 {
            Some(fit_categorical(&values))
        } else {
            None
        }
    } else {
        None
    };

    FieldEnrichment {
        distribution,
        categorical,
        null_rate,
        sample_size,
    }
}

/// Extract all non-null numeric values from a column across batches.
fn extract_numeric_values(batches: &[RecordBatch], col_index: usize) -> Vec<f64> {
    let mut values = Vec::new();
    for batch in batches {
        let col = batch.column(col_index);
        if let Some(arr) = col.as_primitive_opt::<arrow::datatypes::Float64Type>() {
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    values.push(arr.value(i));
                }
            }
        } else if let Some(arr) = col.as_primitive_opt::<arrow::datatypes::Int64Type>() {
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    values.push(arr.value(i) as f64);
                }
            }
        } else if let Some(arr) = col.as_primitive_opt::<arrow::datatypes::Float32Type>() {
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    values.push(arr.value(i) as f64);
                }
            }
        } else if let Some(arr) = col.as_primitive_opt::<arrow::datatypes::Int32Type>() {
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    values.push(arr.value(i) as f64);
                }
            }
        } else if let Some(arr) = col.as_primitive_opt::<arrow::datatypes::Int16Type>() {
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    values.push(arr.value(i) as f64);
                }
            }
        } else if let Some(arr) = col.as_primitive_opt::<arrow::datatypes::Int8Type>() {
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    values.push(arr.value(i) as f64);
                }
            }
        }
    }
    values
}

/// Extract all non-null string values from a column across batches.
fn extract_string_values(batches: &[RecordBatch], col_index: usize) -> Vec<String> {
    let mut values = Vec::new();
    for batch in batches {
        let col = batch.column(col_index);
        if let Some(arr) = col.as_string_opt::<i32>() {
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    values.push(arr.value(i).to_string());
                }
            }
        } else if let Some(arr) = col.as_string_opt::<i64>() {
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    values.push(arr.value(i).to_string());
                }
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Float64Array, StringArray};
    use arrow::datatypes::{DataType, Field as ArrowField};
    use arrow::record_batch::RecordBatch;

    #[test]
    fn test_extract_numeric_values() {
        let schema = Arc::new(Schema::new(vec![ArrowField::new(
            "score",
            DataType::Float64,
            true,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Float64Array::from(vec![
                Some(1.0),
                Some(2.0),
                None,
                Some(3.0),
            ]))],
        )
        .unwrap();

        let values = extract_numeric_values(&[batch], 0);
        assert_eq!(values, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_extract_string_values() {
        let schema = Arc::new(Schema::new(vec![ArrowField::new(
            "name",
            DataType::Utf8,
            true,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(vec![
                Some("Alice"),
                Some("Bob"),
                None,
                Some("Carol"),
            ]))],
        )
        .unwrap();

        let values = extract_string_values(&[batch], 0);
        assert_eq!(values, vec!["Alice", "Bob", "Carol"]);
    }
}
