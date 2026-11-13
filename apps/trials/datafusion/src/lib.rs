//! Apache DataFusion, the SQL query engine, behind Bimetal: query Arrow data with
//! SQL from Python, from Node.js, and from a browser.

mod nozstd;
mod scalar;

use std::future::Future;
use std::sync::{Arc, Mutex};

use bimetal::Callback;
use bimetal::arrow::{ArrowError, RecordBatch, SchemaRef, Table};
use datafusion::datasource::MemTable;
use datafusion::error::DataFusionError;
use datafusion::execution::SendableRecordBatchStream;
use datafusion::prelude::{DataFrame, SessionContext};
use futures::StreamExt;

/// Why a query failed.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum QueryError {
    /// The query does not parse or plan.
    #[error("{message}")]
    #[bimetal(details)]
    Plan {
        /// What DataFusion reported.
        message: String,
    },
    /// The query failed while running.
    #[error("{message}")]
    #[bimetal(details)]
    Execution {
        /// What DataFusion reported.
        message: String,
    },
}

impl From<DataFusionError> for QueryError {
    fn from(error: DataFusionError) -> Self {
        let message = error.to_string();
        match error.find_root() {
            DataFusionError::Plan(_)
            | DataFusionError::SQL(..)
            | DataFusionError::SchemaError(..) => Self::Plan { message },
            _ => Self::Execution { message },
        }
    }
}

impl From<ArrowError> for QueryError {
    fn from(error: ArrowError) -> Self {
        Self::Execution {
            message: error.to_string(),
        }
    }
}

/// Run a future that may spawn Tokio tasks, as DataFusion's plans do. Native
/// hosts run it on Bimetal's runtime, and a browser on a runtime of its own.
///
/// A native call runs on the caller's thread until it first waits, and
/// planning or running a query may not wait at all, so the yield moves the
/// work to a worker first.
#[cfg(not(target_arch = "wasm32"))]
async fn run<F: Future>(future: F) -> F::Output {
    tokio::task::yield_now().await;
    future.await
}

#[cfg(target_arch = "wasm32")]
async fn run<F: Future>(future: F) -> F::Output {
    thread_local! {
        static RUNTIME: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime needs no I/O");
    }
    RUNTIME.with(|runtime| runtime.block_on(future))
}

/// A set of tables and functions that queries run against.
pub struct Session {
    context: SessionContext,
}

#[bimetal::export]
impl Session {
    /// Create a session with no tables.
    #[bimetal(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            context: SessionContext::new(),
        }
    }

    /// Register a record batch as a table.
    pub fn add_batch(
        &self,
        name: &str,
        #[bimetal(arrow)] batch: RecordBatch,
    ) -> Result<(), QueryError> {
        self.context.register_batch(name, batch)?;
        Ok(())
    }

    /// Register a table of record batches.
    pub fn add_table(&self, name: &str, #[bimetal(arrow)] table: Table) -> Result<(), QueryError> {
        let schema = Arc::clone(table.schema());
        let provider = MemTable::try_new(schema, vec![table.into_batches()])?;
        self.context.register_table(name, Arc::new(provider))?;
        Ok(())
    }

    /// Register the result of a frame as a table.
    pub async fn add_frame(&self, name: String, frame: &Frame) -> Result<(), QueryError> {
        let provider = frame.frame.clone().into_view();
        self.context.register_table(name.as_str(), provider)?;
        Ok(())
    }

    /// Register a Python or JavaScript function of two floats as a SQL
    /// function.
    pub fn add_function(&self, name: &str, function: Callback<(f64, f64), f64>) {
        self.context.register_udf(scalar::udf(name, function));
    }

    /// Plan a query, without running it.
    pub async fn sql(&self, query: String) -> Result<Frame, QueryError> {
        Ok(Frame {
            frame: run(self.context.sql(&query)).await?,
        })
    }

    /// Run a query, and return its whole result.
    #[bimetal(returns(arrow))]
    pub async fn query(&self, query: String) -> Result<Table, QueryError> {
        let frame = run(self.context.sql(&query)).await?;
        collect(frame).await
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

async fn collect(frame: DataFrame) -> Result<Table, QueryError> {
    let schema: SchemaRef = Arc::clone(frame.schema().inner());
    let batches = run(frame.collect()).await?;
    Ok(Table::try_new(schema, batches)?)
}

/// A planned query, which more operations can refine.
pub struct Frame {
    frame: DataFrame,
}

#[bimetal::export]
impl Frame {
    /// The schema of the result.
    #[bimetal(getter)]
    #[bimetal(returns(arrow))]
    pub fn schema(&self) -> SchemaRef {
        Arc::clone(self.frame.schema().inner())
    }

    /// Keep the rows where a SQL expression is true.
    pub fn filter(&self, predicate: &str) -> Result<Frame, QueryError> {
        let predicate = self.frame.parse_sql_expr(predicate)?;
        Ok(Frame {
            frame: self.frame.clone().filter(predicate)?,
        })
    }

    /// Keep these columns, or SQL expressions.
    pub fn select(&self, columns: Vec<String>) -> Result<Frame, QueryError> {
        let columns: Vec<&str> = columns.iter().map(String::as_str).collect();
        Ok(Frame {
            frame: self.frame.clone().select_exprs(&columns)?,
        })
    }

    /// Keep at most `count` rows after skipping `skip` of them.
    pub fn limit(
        &self,
        count: u32,
        #[bimetal(default = 0)] skip: u32,
    ) -> Result<Frame, QueryError> {
        Ok(Frame {
            frame: self
                .frame
                .clone()
                .limit(skip as usize, Some(count as usize))?,
        })
    }

    /// Run the query, and return its whole result.
    #[bimetal(returns(arrow))]
    pub async fn collect(&self) -> Result<Table, QueryError> {
        collect(self.frame.clone()).await
    }

    /// Count the rows of the result.
    pub async fn count(&self) -> Result<u64, QueryError> {
        Ok(run(self.frame.clone().count()).await? as u64)
    }

    /// Run the query, and walk its result a batch at a time.
    pub async fn stream(&self) -> Result<Batches, QueryError> {
        Ok(Batches {
            stream: Mutex::new(run(self.frame.clone().execute_stream()).await?),
        })
    }
}

/// The batches of a running query.
pub struct Batches {
    /// A stream is `Send`, but not `Sync`, which a resource must be. A `Mutex`
    /// makes it `Sync`, and `&mut self` reaches it without locking.
    stream: Mutex<SendableRecordBatchStream>,
}

#[bimetal::export]
impl Batches {
    /// The next batch, or nothing at the end.
    #[bimetal(next)]
    #[bimetal(returns(arrow))]
    pub async fn next_batch(&mut self) -> Result<Option<RecordBatch>, QueryError> {
        let stream = self
            .stream
            .get_mut()
            .expect("nothing panics holding the stream");
        Ok(run(stream.next()).await.transpose()?)
    }
}
