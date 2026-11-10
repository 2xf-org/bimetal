//! Arrow values: columns, batches, tables, schemas, and streams that cross
//! without Serde. Hosts see this module as the `columns` namespace.

use std::sync::Arc;

use bimetal::arrow::array::cast::AsArray;
use bimetal::arrow::array::types::{Float64Type, Int64Type};
use bimetal::arrow::array::{Float64Array, RecordBatchIterator};
use bimetal::arrow::{
    ArrayRef, ArrowError, DataType, Field, RecordBatch, RecordBatchStream, Schema, Table,
};
use futures::StreamExt;

/// Failures of the column operations.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum ColumnError {
    /// A column has another type than the operation needs.
    #[error("the column has the wrong type")]
    WrongType,
    /// A batch lacks a column that the operation needs.
    #[error("the batch has no such column")]
    MissingColumn,
    /// Reading a stream failed.
    #[error("{message}")]
    Read {
        /// The stream's error.
        message: String,
    },
}

impl From<ArrowError> for ColumnError {
    fn from(error: ArrowError) -> Self {
        Self::Read {
            message: error.to_string(),
        }
    }
}

/// Multiply every value of a float column. Nulls stay null.
#[bimetal::export]
#[bimetal(returns(arrow))]
pub fn scale(#[bimetal(arrow)] values: &ArrayRef, factor: f64) -> Result<ArrayRef, ColumnError> {
    let values = values
        .as_primitive_opt::<Float64Type>()
        .ok_or(ColumnError::WrongType)?;
    let scaled: Float64Array = values
        .iter()
        .map(|value| value.map(|value| value * factor))
        .collect();
    Ok(Arc::new(scaled))
}

/// Add a `total` column: `price` times `quantity`.
#[bimetal::export]
#[bimetal(returns(arrow))]
pub fn add_totals(#[bimetal(arrow)] batch: &RecordBatch) -> Result<RecordBatch, ColumnError> {
    let price = batch
        .column_by_name("price")
        .ok_or(ColumnError::MissingColumn)?
        .as_primitive_opt::<Float64Type>()
        .ok_or(ColumnError::WrongType)?;
    let quantity = batch
        .column_by_name("quantity")
        .ok_or(ColumnError::MissingColumn)?
        .as_primitive_opt::<Int64Type>()
        .ok_or(ColumnError::WrongType)?;
    let total: Float64Array = price
        .iter()
        .zip(quantity.iter())
        .map(|(price, quantity)| Some(price? * quantity? as f64))
        .collect();
    let mut fields = batch.schema().fields().to_vec();
    fields.push(Arc::new(Field::new("total", DataType::Float64, true)));
    let mut columns = batch.columns().to_vec();
    columns.push(Arc::new(total));
    Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), columns)
        .expect("each column matches its field"))
}

/// Split a batch into a table of batches with at most `size` rows.
#[bimetal::export]
#[bimetal(returns(arrow))]
#[must_use]
pub fn split(#[bimetal(arrow)] batch: &RecordBatch, size: u32) -> Table {
    let size = (size as usize).max(1);
    let batches = (0..batch.num_rows())
        .step_by(size)
        .map(|offset| batch.slice(offset, size.min(batch.num_rows() - offset)))
        .collect();
    Table::try_new(batch.schema(), batches).expect("slices keep the batch schema")
}

/// Count the rows of a table asynchronously.
#[bimetal::export]
pub async fn count_rows(#[bimetal(arrow)] table: Table) -> u64 {
    table.num_rows() as u64
}

/// Name every field with its type, such as `price: Float64`.
#[bimetal::export]
#[must_use]
pub fn describe(#[bimetal(arrow)] schema: &Schema) -> Vec<String> {
    schema
        .fields()
        .iter()
        .map(|field| format!("{}: {}", field.name(), field.data_type()))
        .collect()
}

/// Attach a note to a schema's metadata.
#[bimetal::export]
#[bimetal(returns(arrow))]
#[must_use]
pub fn with_note(#[bimetal(arrow)] schema: Schema, note: String) -> Schema {
    let mut metadata = schema.metadata().clone();
    metadata.insert("note".to_owned(), note);
    schema.with_metadata(metadata)
}

/// Stream a table one batch at a time.
#[bimetal::export]
#[bimetal(returns(arrow))]
#[must_use]
pub fn batches(#[bimetal(arrow)] table: Table) -> RecordBatchStream {
    let schema = table.schema().clone();
    let reader = RecordBatchIterator::new(table.into_batches().into_iter().map(Ok), schema);
    RecordBatchStream::from_reader(reader)
}

/// Add a `total` column to every batch of a stream, as `add_totals` does to
/// one, as the reader asks for each.
#[bimetal::export]
#[bimetal(returns(arrow))]
pub async fn totals(
    #[bimetal(arrow)] rows: RecordBatchStream,
) -> Result<RecordBatchStream, ColumnError> {
    let mut fields = rows.schema().fields().to_vec();
    fields.push(Arc::new(Field::new("total", DataType::Float64, true)));
    let totals = rows.map(|batch| add_totals(&batch?));
    Ok(RecordBatchStream::new(
        Arc::new(Schema::new(fields)),
        totals,
    ))
}

/// Add up a float column across a stream, reading one batch at a time.
#[bimetal::export]
pub async fn sum_column(
    #[bimetal(arrow)] mut rows: RecordBatchStream,
    name: String,
) -> Result<f64, ColumnError> {
    let mut sum = 0.0;
    while let Some(batch) = rows.next().await {
        let batch = batch?;
        let values = batch
            .column_by_name(&name)
            .ok_or(ColumnError::MissingColumn)?
            .as_primitive_opt::<Float64Type>()
            .ok_or(ColumnError::WrongType)?;
        sum += values.iter().flatten().sum::<f64>();
    }
    Ok(sum)
}
