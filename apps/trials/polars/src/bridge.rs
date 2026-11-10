//! Conversions between Polars' Arrow implementation and arrow-rs, which Bimetal
//! uses.
//!
//! Rust treats their types as different, though the memory is the same. Both
//! implement the Arrow C Data Interface, so a column crosses as its two
//! `#[repr(C)]` structs, and nothing is copied.

use std::mem;
use std::sync::Arc;

use arrow_array::ffi::{FFI_ArrowArray, FFI_ArrowSchema, from_ffi, to_ffi};
use arrow_array::{Array as _, ArrayRef, RecordBatch, make_array};
use bimetal::arrow::{Field, Schema, SchemaRef, Table};
use polars::prelude::{Column, CompatLevel, DataFrame, DataType, PlSmallStr, SchemaExt, Series};
use polars_arrow::ffi;

use crate::FrameError;

/// Return the arrow-rs schema of a frame.
pub fn schema(frame: &DataFrame) -> Result<SchemaRef, FrameError> {
    let fields = frame
        .schema()
        .to_arrow(CompatLevel::oldest())
        .iter_values()
        .map(|field| {
            let exported = ffi::export_field_to_c(field);
            // SAFETY: both implementations declare this struct as the C Data
            // Interface's `#[repr(C)]` layout.
            let exported: FFI_ArrowSchema = unsafe { mem::transmute(exported) };
            Ok(Field::try_from(&exported)?)
        })
        .collect::<Result<Vec<Field>, FrameError>>()?;
    Ok(Arc::new(Schema::new(fields)))
}

/// Convert every chunk of a frame to an arrow-rs record batch.
pub fn batches(frame: &DataFrame) -> Result<Vec<RecordBatch>, FrameError> {
    let schema = schema(frame)?;
    let mut batches = Vec::new();
    for chunk in frame.clone().iter_chunks(CompatLevel::oldest(), false) {
        let (chunk_schema, arrays) = chunk.into_schema_and_arrays();
        let mut columns: Vec<ArrayRef> = Vec::with_capacity(arrays.len());
        for (array, field) in arrays.into_iter().zip(chunk_schema.iter_values()) {
            let array = ffi::export_array_to_c(array);
            let field = ffi::export_field_to_c(field);
            // SAFETY: as in `schema`, for both structs.
            let (array, field): (FFI_ArrowArray, FFI_ArrowSchema) =
                unsafe { (mem::transmute(array), mem::transmute(field)) };
            // SAFETY: Polars exported a valid array with this schema.
            columns.push(make_array(unsafe { from_ffi(array, &field) }?));
        }
        batches.push(RecordBatch::try_new(Arc::clone(&schema), columns)?);
    }
    Ok(batches)
}

/// Convert a frame to an arrow-rs table.
pub fn table(frame: &DataFrame) -> Result<Table, FrameError> {
    Ok(Table::try_new(schema(frame)?, batches(frame)?)?)
}

/// Convert an arrow-rs table to a frame.
pub fn from_table(table: Table) -> Result<DataFrame, FrameError> {
    let schema = Arc::clone(table.schema());
    let batches = table.into_batches();
    let mut columns = Vec::with_capacity(schema.fields().len());
    for (index, field) in schema.fields().iter().enumerate() {
        let mut chunks = Vec::with_capacity(batches.len());
        let mut dtype = None;
        for batch in &batches {
            let (array, exported) = to_ffi(&batch.column(index).to_data())?;
            // SAFETY: as in `schema`, for both structs.
            let (array, exported): (ffi::ArrowArray, ffi::ArrowSchema) =
                unsafe { (mem::transmute(array), mem::transmute(exported)) };
            // SAFETY: arrow-rs exported a valid array with this schema.
            let imported = unsafe { ffi::import_field_from_c(&exported) }?;
            // SAFETY: as above.
            let chunk = unsafe { ffi::import_array_from_c(array, imported.dtype().clone()) }?;
            dtype.get_or_insert(imported);
            chunks.push(chunk);
        }
        let name = PlSmallStr::from_str(field.name());
        let series = if chunks.is_empty() {
            let exported = FFI_ArrowSchema::try_from(field.as_ref())?;
            // SAFETY: as in `schema`.
            let exported: ffi::ArrowSchema = unsafe { mem::transmute(exported) };
            // SAFETY: arrow-rs exported a valid schema.
            let imported = unsafe { ffi::import_field_from_c(&exported) }?;
            Series::new_empty(name, &DataType::from_arrow_field(&imported))
        } else {
            Series::try_from((name, chunks))?
        };
        // apache-arrow dictionary-encodes strings, which Polars reads as a
        // categorical column that does not join with a string column.
        let series = match series.dtype() {
            DataType::Categorical(..) | DataType::Enum(..) => series.cast(&DataType::String)?,
            _ => series,
        };
        columns.push(Column::from(series));
    }
    Ok(DataFrame::new_infer_height(columns)?)
}
