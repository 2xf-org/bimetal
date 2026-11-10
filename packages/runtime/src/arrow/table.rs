//! A schema with every record batch that follows it, in memory.

use arrow_array::RecordBatch;
use arrow_schema::{ArrowError, SchemaRef};

/// A schema and the record batches that follow it, like a pyarrow or
/// apache-arrow `Table`. Unlike a bare list of batches, it keeps its schema
/// when it has no batches.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
}

impl Table {
    /// Create a table from batches that all use `schema`.
    ///
    /// # Errors
    ///
    /// Returns [`ArrowError::SchemaError`] when a batch has another schema.
    pub fn try_new(schema: SchemaRef, batches: Vec<RecordBatch>) -> Result<Self, ArrowError> {
        if let Some(batch) = batches.iter().find(|batch| *batch.schema_ref() != schema) {
            return Err(ArrowError::SchemaError(format!(
                "a table batch has the schema {} instead of {schema}",
                batch.schema_ref()
            )));
        }
        Ok(Self { schema, batches })
    }

    /// The schema of every batch.
    #[must_use]
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// The batches, in order.
    #[must_use]
    pub fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    /// Take the batches, in order.
    #[must_use]
    pub fn into_batches(self) -> Vec<RecordBatch> {
        self.batches
    }

    /// The number of rows in every batch together.
    #[must_use]
    pub fn num_rows(&self) -> usize {
        self.batches.iter().map(RecordBatch::num_rows).sum()
    }
}

impl From<RecordBatch> for Table {
    fn from(batch: RecordBatch) -> Self {
        Self {
            schema: batch.schema(),
            batches: vec![batch],
        }
    }
}
