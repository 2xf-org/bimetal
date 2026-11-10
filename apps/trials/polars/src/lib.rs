//! Polars, the dataframe library, behind Bimetal: the same frames and queries from
//! Python, from Node.js, and from a browser.
//!
//! Expressions are SQL text, such as `price * size AS notional`, because a
//! string crosses to every host and Polars parses it.

mod bridge;

use std::collections::VecDeque;
use std::io::Cursor;

use bimetal::arrow::{ArrowError, RecordBatch, Table};
use bimetal::{Callback, CallbackError, Model};
use polars::prelude::{
    AnyValue, CsvReadOptions, CsvWriter, DataFrame, DataType, Expr, IntoLazy, JoinType, JsonFormat,
    JsonReader, JsonWriter, LazyFrame, NamedFrom, PlSmallStr, PolarsError, SerReader, SerWriter,
    Series, SortMultipleOptions, UnionArgs, col, concat,
};
use polars::sql::{SQLContext, sql_expr};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Why an operation on a frame failed.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum FrameError {
    /// A column that an operation named does not exist.
    #[error("{message}")]
    #[bimetal(details)]
    MissingColumn {
        /// What Polars reported.
        message: String,
    },
    /// An expression or a query does not parse or type-check.
    #[error("{message}")]
    #[bimetal(details)]
    InvalidQuery {
        /// What Polars reported.
        message: String,
    },
    /// Polars failed.
    #[error("{message}")]
    #[bimetal(details)]
    Polars {
        /// What Polars reported.
        message: String,
    },
    /// A host function failed.
    #[error("the function failed: {message}")]
    #[bimetal(details)]
    Function {
        /// What the host function raised.
        message: String,
    },
}

impl From<PolarsError> for FrameError {
    fn from(error: PolarsError) -> Self {
        let message = error.to_string();
        // Polars adds context around the error that says what went wrong.
        let mut root = &error;
        while let PolarsError::Context { error, .. } | PolarsError::ExprContext { error, .. } = root
        {
            root = error;
        }
        match root {
            PolarsError::ColumnNotFound(_) => Self::MissingColumn { message },
            PolarsError::SQLInterface(_)
            | PolarsError::SQLSyntax(_)
            | PolarsError::SchemaMismatch(_)
            | PolarsError::InvalidOperation(_) => Self::InvalidQuery { message },
            _ => Self::Polars { message },
        }
    }
}

impl From<ArrowError> for FrameError {
    fn from(error: ArrowError) -> Self {
        Self::Polars {
            message: error.to_string(),
        }
    }
}

impl From<CallbackError> for FrameError {
    fn from(error: CallbackError) -> Self {
        Self::Function {
            message: error.to_string(),
        }
    }
}

/// One column of a frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
pub struct ColumnInfo {
    /// The column's name.
    pub name: String,
    /// The column's type, as Polars writes it, such as `i64`.
    pub dtype: String,
}

fn expressions(texts: &[String]) -> Result<Vec<Expr>, FrameError> {
    Ok(texts
        .iter()
        .map(sql_expr)
        .collect::<Result<Vec<Expr>, PolarsError>>()?)
}

fn sort_options(descending: bool) -> SortMultipleOptions {
    SortMultipleOptions::default().with_order_descending(descending)
}

/// A table of columns in memory.
pub struct Frame {
    frame: DataFrame,
}

#[bimetal::export]
impl Frame {
    /// Make a frame from Arrow data, such as a pyarrow table or a Polars
    /// DataFrame in Python.
    #[bimetal(constructor)]
    pub fn new(#[bimetal(arrow)] table: Table) -> Result<Self, FrameError> {
        Ok(Self {
            frame: bridge::from_table(table)?,
        })
    }

    /// Read CSV text.
    #[bimetal(constructor)]
    pub fn read_csv(
        #[bimetal(bytes)] data: Vec<u8>,
        #[bimetal(default = true)] has_header: bool,
    ) -> Result<Self, FrameError> {
        let frame = CsvReadOptions::default()
            .with_has_header(has_header)
            .into_reader_with_file_handle(Cursor::new(data))
            .finish()?;
        Ok(Self { frame })
    }

    /// Make a frame from rows, each an object of column names and values.
    #[bimetal(constructor)]
    pub fn from_rows(rows: Vec<Value>) -> Result<Self, FrameError> {
        let text = serde_json::to_vec(&rows).map_err(|error| FrameError::Polars {
            message: error.to_string(),
        })?;
        let frame = JsonReader::new(Cursor::new(text))
            .with_json_format(JsonFormat::Json)
            .finish()?;
        Ok(Self { frame })
    }

    /// How many rows the frame has.
    #[bimetal(getter)]
    pub fn height(&self) -> u32 {
        self.frame.height() as u32
    }

    /// How many columns the frame has.
    #[bimetal(getter)]
    pub fn width(&self) -> u32 {
        self.frame.width() as u32
    }

    /// The names of the columns.
    #[bimetal(getter)]
    pub fn columns(&self) -> Vec<String> {
        self.frame
            .get_column_names()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// The name and the type of each column.
    #[bimetal(getter)]
    pub fn schema(&self) -> Vec<ColumnInfo> {
        self.frame
            .schema()
            .iter()
            .map(|(name, dtype)| ColumnInfo {
                name: name.to_string(),
                dtype: dtype.to_string(),
            })
            .collect()
    }

    /// How many rows the frame has.
    #[bimetal(len)]
    pub fn row_count(&self) -> u32 {
        self.height()
    }

    /// The frame as the table that Polars prints.
    #[bimetal(repr)]
    pub fn table(&self) -> String {
        self.frame.to_string()
    }

    /// The frame as the table that Polars prints.
    #[bimetal(display)]
    pub fn text(&self) -> String {
        self.frame.to_string()
    }

    /// Whether the frame has a column of this name.
    #[bimetal(contains)]
    pub fn has_column(&self, name: &str) -> bool {
        self.frame.column(name).is_ok()
    }

    /// The values of a column, if the frame has one of this name.
    #[bimetal(getitem)]
    pub fn column(&self, name: &str) -> Option<Vec<Value>> {
        let column = self.frame.column(name).ok()?;
        let series = column.as_materialized_series();
        Some(series.iter().map(json_value).collect())
    }

    /// Whether two frames hold the same values.
    #[bimetal(eq)]
    pub fn equals(&self, #[bimetal(resource)] other: &Frame) -> bool {
        self.frame.equals_missing(&other.frame)
    }

    /// The frame as Arrow data.
    #[bimetal(returns(arrow))]
    pub fn to_arrow(&self) -> Result<Table, FrameError> {
        bridge::table(&self.frame)
    }

    /// The rows, each an object of column names and values.
    pub fn to_rows(&self) -> Result<Vec<Value>, FrameError> {
        let mut buffer = Vec::new();
        let mut frame = self.frame.clone();
        JsonWriter::new(&mut buffer)
            .with_json_format(JsonFormat::Json)
            .finish(&mut frame)?;
        serde_json::from_slice(&buffer).map_err(|error| FrameError::Polars {
            message: error.to_string(),
        })
    }

    /// The frame as CSV text.
    pub fn to_csv(&self) -> Result<String, FrameError> {
        let mut buffer = Vec::new();
        let mut frame = self.frame.clone();
        CsvWriter::new(&mut buffer).finish(&mut frame)?;
        String::from_utf8(buffer).map_err(|error| FrameError::Polars {
            message: error.to_string(),
        })
    }

    /// The first rows.
    #[bimetal(returns(resource))]
    pub fn head(&self, #[bimetal(default = 5)] count: u32) -> Frame {
        Frame {
            frame: self.frame.head(Some(count as usize)),
        }
    }

    /// Start a query over the frame, which runs when it collects.
    #[bimetal(returns(resource))]
    pub fn lazy(&self) -> Query {
        Query {
            query: self.frame.clone().lazy(),
        }
    }

    /// Run SQL over the frame, which the query calls `self`.
    #[bimetal(returns(resource))]
    pub fn sql(&self, query: &str) -> Result<Frame, FrameError> {
        let mut context = SQLContext::new();
        context.register("self", self.frame.clone().lazy());
        Ok(Frame {
            frame: context.execute(query)?.collect()?,
        })
    }

    /// Join another frame on columns that both have.
    #[bimetal(returns(resource))]
    pub fn join(
        &self,
        #[bimetal(resource)] other: &Frame,
        on: Vec<String>,
        #[bimetal(default = "inner")] how: &str,
    ) -> Result<Frame, FrameError> {
        let how = match how {
            "inner" => JoinType::Inner,
            "left" => JoinType::Left,
            "full" => JoinType::Full,
            other => {
                return Err(FrameError::InvalidQuery {
                    message: format!("no join named {other}"),
                });
            }
        };
        let keys: Vec<Expr> = on.iter().map(|name| col(name.as_str())).collect();
        let frame = self
            .frame
            .clone()
            .lazy()
            .join(other.frame.clone().lazy(), keys.clone(), keys, how.into())
            .collect()?;
        Ok(Frame { frame })
    }

    /// Add a column of what `function` returns for each value of a float column.
    #[bimetal(returns(resource))]
    pub fn map_column(
        &self,
        column: &str,
        into: &str,
        function: Callback<(f64,), f64>,
    ) -> Result<Frame, FrameError> {
        let source = self.frame.column(column)?.cast(&DataType::Float64)?;
        let mut values: Vec<Option<f64>> = Vec::with_capacity(source.len());
        for value in source.f64()?.iter() {
            values.push(match value {
                Some(value) => Some(function.call((value,))?),
                None => None,
            });
        }
        let mapped = Series::new(PlSmallStr::from_str(into), values);
        let mut frame = self.frame.clone();
        frame.with_column(mapped.into())?;
        Ok(Frame { frame })
    }

    /// Walk the frame in Arrow batches of at most `size` rows.
    #[bimetal(returns(resource))]
    pub fn batches(&self, #[bimetal(default = 1000)] size: u32) -> Result<Batches, FrameError> {
        let mut pending = VecDeque::new();
        let height = self.frame.height();
        let size = size.max(1) as usize;
        let mut offset = 0;
        while offset < height {
            let slice = self.frame.slice(offset as i64, size);
            offset += slice.height();
            let mut slice = slice;
            slice.rechunk_mut();
            pending.extend(bridge::batches(&slice)?);
        }
        Ok(Batches { pending })
    }

    /// Stack frames of the same columns.
    #[bimetal(returns(resource))]
    pub fn stack(&self, #[bimetal(resource)] other: &Frame) -> Result<Frame, FrameError> {
        let frame = concat(
            [self.frame.clone().lazy(), other.frame.clone().lazy()],
            UnionArgs::default(),
        )?
        .collect()?;
        Ok(Frame { frame })
    }
}

/// Convert one value of a column to JSON.
fn json_value(value: AnyValue<'_>) -> Value {
    match value {
        AnyValue::Null => Value::Null,
        AnyValue::Boolean(value) => Value::from(value),
        AnyValue::String(value) => Value::from(value),
        AnyValue::StringOwned(value) => Value::from(value.as_str()),
        AnyValue::Int8(value) => Value::from(value),
        AnyValue::Int16(value) => Value::from(value),
        AnyValue::Int32(value) => Value::from(value),
        AnyValue::Int64(value) => Value::from(value),
        AnyValue::UInt8(value) => Value::from(value),
        AnyValue::UInt16(value) => Value::from(value),
        AnyValue::UInt32(value) => Value::from(value),
        AnyValue::UInt64(value) => Value::from(value),
        AnyValue::Float32(value) => Value::from(value),
        AnyValue::Float64(value) => Value::from(value),
        other => Value::from(other.str_value().as_ref()),
    }
}

/// A query, which Polars optimizes as a whole when it collects.
pub struct Query {
    query: LazyFrame,
}

#[bimetal::export]
impl Query {
    /// Keep the rows where a SQL expression is true.
    #[bimetal(returns(resource))]
    pub fn filter(&self, predicate: &str) -> Result<Query, FrameError> {
        Ok(Query {
            query: self.query.clone().filter(sql_expr(predicate)?),
        })
    }

    /// Keep these columns or SQL expressions.
    #[bimetal(returns(resource))]
    pub fn select(&self, columns: Vec<String>) -> Result<Query, FrameError> {
        Ok(Query {
            query: self.query.clone().select(expressions(&columns)?),
        })
    }

    /// Add these SQL expressions as columns.
    #[bimetal(returns(resource))]
    pub fn with_columns(&self, columns: Vec<String>) -> Result<Query, FrameError> {
        Ok(Query {
            query: self.query.clone().with_columns(expressions(&columns)?),
        })
    }

    /// Group by `keys`, and compute `aggregates`, SQL expressions such as
    /// `sum(size) AS volume`.
    #[bimetal(returns(resource))]
    pub fn group_by(
        &self,
        keys: Vec<String>,
        aggregates: Vec<String>,
    ) -> Result<Query, FrameError> {
        let keys: Vec<Expr> = keys.iter().map(|name| col(name.as_str())).collect();
        Ok(Query {
            query: self
                .query
                .clone()
                .group_by_stable(keys)
                .agg(expressions(&aggregates)?),
        })
    }

    /// Sort by a column.
    #[bimetal(returns(resource))]
    pub fn sort(&self, by: &str, #[bimetal(default = false)] descending: bool) -> Query {
        Query {
            query: self.query.clone().sort([by], sort_options(descending)),
        }
    }

    /// Keep at most `count` rows.
    #[bimetal(returns(resource))]
    pub fn limit(&self, count: u32) -> Query {
        Query {
            query: self.query.clone().limit(count),
        }
    }

    /// The plan that Polars would run, after optimizing it.
    pub fn explain(&self) -> Result<String, FrameError> {
        Ok(self.query.clone().explain(true)?)
    }

    /// Run the query.
    #[bimetal(returns(resource))]
    pub fn collect(&self) -> Result<Frame, FrameError> {
        Ok(Frame {
            frame: self.query.clone().collect()?,
        })
    }

    /// Run the query without blocking the host's event loop on native hosts.
    #[bimetal(returns(resource))]
    pub async fn collect_async(&self) -> Result<Frame, FrameError> {
        self.collect()
    }
}

/// Arrow batches of a frame, one call at a time.
pub struct Batches {
    pending: VecDeque<RecordBatch>,
}

#[bimetal::export]
impl Batches {
    /// The next batch, or nothing at the end.
    #[bimetal(next)]
    #[bimetal(returns(arrow))]
    pub fn next_batch(&mut self) -> Option<RecordBatch> {
        self.pending.pop_front()
    }
}

/// Named frames that SQL queries run against.
pub struct Session {
    context: SQLContext,
}

#[bimetal::export]
impl Session {
    /// Make a session with no tables.
    #[bimetal(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            context: SQLContext::new(),
        }
    }

    /// Make a frame available to queries under a name.
    pub fn register(&mut self, name: &str, #[bimetal(resource)] frame: &Frame) {
        self.context.register(name, frame.frame.clone().lazy());
    }

    /// Run a SQL query.
    #[bimetal(returns(resource))]
    pub fn sql(&mut self, query: &str) -> Result<Frame, FrameError> {
        Ok(Frame {
            frame: self.context.execute(query)?.collect()?,
        })
    }

    /// The names of the tables.
    #[bimetal(getter)]
    pub fn tables(&self) -> Vec<String> {
        self.context.get_tables()
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
