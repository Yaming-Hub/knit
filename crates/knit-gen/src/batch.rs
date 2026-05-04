//! Batch assembly — combine per-field arrays into an Arrow `RecordBatch`.
//!
//! Called once per batch after all field generators have run. The resulting
//! `RecordBatch` is handed to the output sink (Parquet writer, JSON serializer,
//! etc.) in downstream pipeline stages (`knit-bind`).

use arrow::array::ArrayRef;
use arrow::datatypes::{Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

use crate::error::GenError;

/// Assemble field arrays into a [`RecordBatch`].
///
/// Constructs a schema from the provided field names and the data types
/// inferred from each array. All fields are marked nullable.
///
/// # Errors
///
/// Returns [`GenError::Generation`] if `field_names` and `field_arrays` have
/// different lengths, or [`GenError::Arrow`] if Arrow rejects the batch
/// (e.g. arrays have inconsistent row counts).
pub fn assemble_batch(
    field_names: &[String],
    field_arrays: Vec<ArrayRef>,
) -> Result<RecordBatch, GenError> {
    if field_names.len() != field_arrays.len() {
        return Err(GenError::Generation(format!(
            "field_names len ({}) != field_arrays len ({})",
            field_names.len(),
            field_arrays.len(),
        )));
    }

    let fields: Vec<Field> = field_names
        .iter()
        .zip(field_arrays.iter())
        .map(|(name, arr)| Field::new(name, arr.data_type().clone(), true))
        .collect();

    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema, field_arrays)?;
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Float64Array, Int64Array, StringArray};

    #[test]
    fn assemble_basic() {
        let names = vec!["id".to_string(), "name".to_string()];
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![1, 2, 3])),
            Arc::new(StringArray::from(vec!["a", "b", "c"])),
        ];
        let batch = assemble_batch(&names, arrays).unwrap();
        assert_eq!(batch.num_rows(), 3);
        assert_eq!(batch.num_columns(), 2);
        assert!(batch.schema().field(0).is_nullable());
    }

    #[test]
    fn assemble_length_mismatch() {
        let names = vec!["id".to_string()];
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![1])),
            Arc::new(Float64Array::from(vec![2.0])),
        ];
        let err = assemble_batch(&names, arrays).unwrap_err();
        assert!(err.to_string().contains("field_names len"));
    }

    #[test]
    fn assemble_empty_columns_fails() {
        let err = assemble_batch(&[], vec![]).unwrap_err();
        // Arrow requires at least one column or an explicit row count
        assert!(matches!(err, GenError::Arrow(_)));
    }
}
