//! The Rust values that an Arrow boundary carries, and the parts that each
//! crosses as.

use std::sync::Arc;

use arrow_array::{Array, ArrayRef, RecordBatch, RecordBatchIterator};
use arrow_schema::{ArrowError, Field, Schema, SchemaRef};

use super::record_batch_stream::RecordBatchStream;
use super::table::Table;
use crate::ir::ArrowKind;

mod sealed {
    pub trait Sealed {}
}

/// A Rust value that a `#[bimetal(arrow)]` parameter or `#[bimetal(returns(arrow))]`
/// export can carry: [`ArrayRef`], [`RecordBatch`], [`Table`], [`Schema`],
/// [`SchemaRef`], or [`RecordBatchStream`].
///
/// Every value converts to and from [`Parts`], which is what each host
/// boundary carries.
pub trait ArrowValue: sealed::Sealed + Sized {
    /// The host value that this Rust value becomes.
    #[doc(hidden)]
    const KIND: ArrowKind;

    /// Split the value into the parts that cross.
    #[doc(hidden)]
    fn into_parts(self) -> Parts;

    /// Rebuild the value from the parts that crossed. A value held in memory
    /// needs every batch: a host reads a stream first, with [`Parts::collected`].
    ///
    /// # Errors
    ///
    /// Returns an error when the parts do not form this value.
    #[doc(hidden)]
    fn from_parts(parts: Parts) -> Result<Self, ArrowError>;
}

/// The parts that an Arrow value crosses a host boundary as.
#[doc(hidden)]
pub enum Parts {
    /// A schema and every batch, in memory.
    Batches {
        /// The schema of every batch.
        schema: SchemaRef,
        /// The batches, in order.
        batches: Vec<RecordBatch>,
    },
    /// A stream, which makes its batches as the reader asks for them.
    Stream(RecordBatchStream),
}

impl Parts {
    /// Read a stream to its end on this thread, so that the parts hold every
    /// batch.
    ///
    /// # Errors
    ///
    /// Returns the error of the first batch that fails.
    pub fn collected(self) -> Result<Self, ArrowError> {
        let Self::Stream(stream) = self else {
            return Ok(self);
        };
        let schema = stream.schema().clone();
        let batches: Result<Vec<_>, _> = stream.into_iter().collect();
        Ok(Self::Batches {
            schema,
            batches: batches?,
        })
    }

    /// Return the schema and every batch. Reading a stream here would link
    /// its runtime into every host that never passes one.
    fn into_batches(self) -> Result<(SchemaRef, Vec<RecordBatch>), ArrowError> {
        match self {
            Self::Batches { schema, batches } => Ok((schema, batches)),
            Self::Stream(_) => Err(ArrowError::InvalidArgumentError(
                "a value in memory needs every batch, but a stream arrived".to_owned(),
            )),
        }
    }
}

/// The one column that carries an array: a batch has fields, an array has none.
const ARRAY_COLUMN: &str = "values";

impl sealed::Sealed for ArrayRef {}

impl ArrowValue for ArrayRef {
    const KIND: ArrowKind = ArrowKind::Array;

    fn into_parts(self) -> Parts {
        let field = Field::new(
            ARRAY_COLUMN,
            self.data_type().clone(),
            /* nullable */ true,
        );
        let schema = Arc::new(Schema::new(vec![field]));
        let batch = RecordBatch::try_new(schema.clone(), vec![self])
            .expect("a nullable field accepts any array of its own type");
        Parts::Batches {
            schema,
            batches: vec![batch],
        }
    }

    fn from_parts(parts: Parts) -> Result<Self, ArrowError> {
        let (schema, batches) = parts.into_batches()?;
        if schema.fields().len() != 1 {
            return Err(ArrowError::SchemaError(format!(
                "an array crosses as one column, but the schema has {}",
                schema.fields().len()
            )));
        }
        let mut columns: Vec<ArrayRef> = batches
            .iter()
            .map(|batch| batch.column(0).clone())
            .collect();
        match columns.len() {
            0 => Ok(arrow_array::new_empty_array(schema.field(0).data_type())),
            1 => Ok(columns.remove(0)),
            // A chunked host array arrives as one batch per chunk.
            _ => {
                let chunks: Vec<&dyn Array> = columns.iter().map(AsRef::as_ref).collect();
                arrow_select::concat::concat(&chunks)
            }
        }
    }
}

impl sealed::Sealed for RecordBatch {}

impl ArrowValue for RecordBatch {
    const KIND: ArrowKind = ArrowKind::RecordBatch;

    fn into_parts(self) -> Parts {
        Parts::Batches {
            schema: self.schema(),
            batches: vec![self],
        }
    }

    fn from_parts(parts: Parts) -> Result<Self, ArrowError> {
        let (_, batches) = parts.into_batches()?;
        let [batch] = <[RecordBatch; 1]>::try_from(batches).map_err(|batches| {
            ArrowError::InvalidArgumentError(format!(
                "a record batch crosses as one batch, but {} arrived",
                batches.len()
            ))
        })?;
        Ok(batch)
    }
}

impl sealed::Sealed for Table {}

impl ArrowValue for Table {
    const KIND: ArrowKind = ArrowKind::Table;

    fn into_parts(self) -> Parts {
        Parts::Batches {
            schema: self.schema().clone(),
            batches: self.into_batches(),
        }
    }

    fn from_parts(parts: Parts) -> Result<Self, ArrowError> {
        let (schema, batches) = parts.into_batches()?;
        Self::try_new(schema, batches)
    }
}

impl sealed::Sealed for SchemaRef {}

impl ArrowValue for SchemaRef {
    const KIND: ArrowKind = ArrowKind::Schema;

    fn into_parts(self) -> Parts {
        Parts::Batches {
            schema: self,
            batches: Vec::new(),
        }
    }

    fn from_parts(parts: Parts) -> Result<Self, ArrowError> {
        Ok(match parts {
            Parts::Batches { schema, .. } => schema,
            Parts::Stream(stream) => stream.schema().clone(),
        })
    }
}

impl sealed::Sealed for Schema {}

impl ArrowValue for Schema {
    const KIND: ArrowKind = ArrowKind::Schema;

    fn into_parts(self) -> Parts {
        Arc::new(self).into_parts()
    }

    fn from_parts(parts: Parts) -> Result<Self, ArrowError> {
        SchemaRef::from_parts(parts).map(Arc::unwrap_or_clone)
    }
}

impl sealed::Sealed for RecordBatchStream {}

impl ArrowValue for RecordBatchStream {
    const KIND: ArrowKind = ArrowKind::Stream;

    fn into_parts(self) -> Parts {
        Parts::Stream(self)
    }

    fn from_parts(parts: Parts) -> Result<Self, ArrowError> {
        Ok(match parts {
            Parts::Batches { schema, batches } => Self::from_reader(RecordBatchIterator::new(
                batches.into_iter().map(Ok),
                schema,
            )),
            Parts::Stream(stream) => stream,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{Int32Array, StringArray};
    use arrow_schema::DataType;

    fn batch(values: &[i32]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "n",
            DataType::Int32,
            /* nullable */ false,
        )]));
        RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(values.to_vec()))])
            .expect("valid batch")
    }

    fn round_trip<T: ArrowValue>(value: T) -> T {
        T::from_parts(value.into_parts()).expect("the parts form the value")
    }

    #[test]
    fn every_value_round_trips_through_its_parts() {
        let array: ArrayRef = Arc::new(StringArray::from(vec![Some("a"), None]));
        assert_eq!(round_trip(array.clone()).to_data(), array.to_data());

        let one = batch(&[1, 2]);
        assert_eq!(round_trip(one.clone()), one);

        let table = Table::try_new(one.schema(), vec![one.clone(), batch(&[3])]).expect("table");
        assert_eq!(table.num_rows(), 3);
        assert_eq!(round_trip(table.clone()), table);

        let schema = one.schema().as_ref().clone();
        assert_eq!(round_trip(schema.clone()), schema);
    }

    #[test]
    fn values_in_memory_take_a_collected_stream() {
        let first = batch(&[1, 2]);
        let schema = first.schema();
        let stream = |batches: Vec<RecordBatch>| {
            Parts::Stream(RecordBatchStream::from_reader(RecordBatchIterator::new(
                batches.into_iter().map(Ok),
                schema.clone(),
            )))
        };
        assert!(Table::from_parts(stream(vec![first.clone()])).is_err());
        let collected = |batches| stream(batches).collected().expect("the stream reads");
        let table = Table::from_parts(collected(vec![first.clone(), batch(&[3])])).expect("table");
        assert_eq!(table.num_rows(), 3);
        let joined =
            ArrayRef::from_parts(collected(vec![first.clone(), batch(&[3])])).expect("join");
        assert_eq!(joined.as_ref(), batch(&[1, 2, 3]).column(0).as_ref());
        assert_eq!(
            SchemaRef::from_parts(stream(Vec::new())).expect("schema"),
            schema
        );
        assert!(RecordBatch::from_parts(collected(Vec::new())).is_err());
    }

    #[test]
    fn mismatched_parts_fail() {
        let first = batch(&[1, 2]);
        let parts = Parts::Batches {
            schema: first.schema(),
            batches: Vec::new(),
        };
        assert!(RecordBatch::from_parts(parts).is_err());
        let other = Arc::new(Schema::new(vec![Field::new(
            "m",
            DataType::Utf8,
            /* nullable */ true,
        )]));
        assert!(Table::try_new(other, vec![first]).is_err());
    }
}
