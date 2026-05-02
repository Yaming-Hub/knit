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
