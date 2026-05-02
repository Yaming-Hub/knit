//! Batch assembly — combines per-field arrays into an Arrow `RecordBatch`.

use arrow::array::ArrayRef;
use arrow::datatypes::{Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;

use crate::error::GenError;

/// Assemble field arrays into a [`RecordBatch`].
///
/// The resulting batch has one column per field, using the supplied names
/// and the data types inferred from each array.
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
