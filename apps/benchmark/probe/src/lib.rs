//! The probe that `apps/benchmark/lab.py` builds and times: one export for
//! each kind of value and call that crosses a host boundary, each doing as
//! little Rust work as it can, so that its time is the crossing's.

use std::collections::BTreeMap;
use std::future;
use std::sync::Arc;
use std::task::Poll;

use bimetal::arrow::array::{Float64Array, Int64Array, RecordBatchIterator};
use bimetal::arrow::{ArrayRef, DataType, Field, RecordBatch, RecordBatchStream, Schema};
use bimetal::{Callback, CallbackError, Model};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// A point, the smallest model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Model)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A row with the fields that a typical model has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub id: u64,
    pub name: String,
    pub score: f64,
    pub active: bool,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// Why a probe call failed.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum ProbeError {
    /// The call failed on purpose.
    #[error("the probe failed on purpose")]
    Requested,
    /// A callback failed.
    #[error("the callback failed: {0}")]
    Callback(String),
}

impl From<CallbackError> for ProbeError {
    fn from(error: CallbackError) -> Self {
        Self::Callback(error.to_string())
    }
}

/// Do nothing: the floor of a synchronous call.
#[bimetal::export]
pub fn noop() {}

/// Do nothing while Python keeps its interpreter lock: the floor of a brief
/// call.
#[bimetal::export]
#[bimetal(brief)]
pub fn noop_brief() {}

/// Add two integers: a scalar in each direction.
#[bimetal::export]
#[bimetal(brief)]
#[must_use]
pub fn add(a: i64, b: i64) -> i64 {
    a.wrapping_add(b)
}

#[bimetal::export]
#[bimetal(brief)]
#[must_use]
pub fn echo(text: String) -> String {
    text
}

/// Return the integers from zero below `count`: a list of scalars out.
#[bimetal::export]
#[must_use]
pub fn numbers(count: u32) -> Vec<i64> {
    (0..i64::from(count)).collect()
}

/// Add up a list of integers: a list of scalars in.
#[bimetal::export]
#[must_use]
pub fn total(values: Vec<i64>) -> i64 {
    values.iter().fold(0, |sum, value| sum.wrapping_add(*value))
}

/// Count each word: a list of texts in, and a map of scalars out.
#[bimetal::export]
#[must_use]
pub fn tally(words: Vec<String>) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for word in words {
        *counts.entry(word).or_insert(0) += 1;
    }
    counts
}

/// Return the smallest and the largest value: a tuple of scalars out, or
/// nothing.
#[bimetal::export]
#[must_use]
pub fn bounds(values: Vec<i64>) -> Option<(i64, i64)> {
    Some((*values.iter().min()?, *values.iter().max()?))
}

/// Make a point: a small model out.
#[bimetal::export]
#[bimetal(brief)]
#[must_use]
pub fn point(x: f64, y: f64) -> Point {
    Point { x, y }
}

/// Return a point's distance from the origin: a small model in.
#[bimetal::export]
#[bimetal(brief)]
#[must_use]
pub fn length(point: Point) -> f64 {
    point.x.hypot(point.y)
}

/// Make `count` rows: a list of models out.
#[bimetal::export]
#[must_use]
pub fn rows(count: u32) -> Vec<Row> {
    (0..count).map(row).collect()
}

/// Count the tags of some rows: a list of models in.
#[bimetal::export]
#[must_use]
pub fn count_tags(rows: Vec<Row>) -> u64 {
    rows.iter().map(|row| row.tags.len() as u64).sum()
}

/// Return the bytes reversed: bytes in each direction.
#[bimetal::export]
#[must_use]
pub fn reverse(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().rev().copied().collect()
}

/// Fail with a typed error: the cost of an error crossing.
///
/// # Errors
///
/// Always returns [`ProbeError::Requested`].
#[bimetal::export]
pub fn fail() -> Result<(), ProbeError> {
    Err(ProbeError::Requested)
}

/// Call `callback` with `value` and return its result: one call back into the
/// host.
///
/// # Errors
///
/// Returns [`ProbeError::Callback`] when the callback fails.
#[bimetal::export]
pub fn call(callback: Callback<(i64,), i64>, value: i64) -> Result<i64, ProbeError> {
    Ok(callback.call((value,))?)
}

/// Mix `rounds` times, a fixed cost of work per round on one core.
#[bimetal::export]
#[must_use]
pub fn spin(rounds: u32) -> u64 {
    mix(rounds)
}

/// Return `value` from an async call that does not wait.
#[bimetal::export]
pub async fn ready(value: i64) -> i64 {
    value
}

/// Return the integers below `count` from an async call that does not wait.
#[bimetal::export]
pub async fn ready_numbers(count: u32) -> Vec<i64> {
    numbers(count)
}

/// Return `value` after waiting once, as a call does that waits for I/O.
#[bimetal::export]
pub async fn wait(value: i64) -> i64 {
    yield_once().await;
    value
}

/// Mix `rounds` times in an async call that never waits, as a call does that
/// searches what it holds in memory.
#[bimetal::export]
pub async fn spin_async(rounds: u32) -> u64 {
    mix(rounds)
}

/// Return a batch of one row with a float and an integer column.
#[bimetal::export]
#[bimetal(returns(arrow))]
#[must_use]
pub fn one_row() -> RecordBatch {
    batch(1, 0)
}

/// Count the rows of a batch: an Arrow batch in.
#[bimetal::export]
#[must_use]
pub fn batch_rows(#[bimetal(arrow)] batch: &RecordBatch) -> u64 {
    batch.num_rows() as u64
}

/// Stream `batches` batches of `rows` rows each: one `next()` per batch.
#[bimetal::export]
#[bimetal(returns(arrow))]
#[must_use]
pub fn stream(batches: u32, rows: u32) -> RecordBatchStream {
    let parts: Vec<_> = (0..batches)
        .map(|index| Ok(batch(rows, i64::from(index) * i64::from(rows))))
        .collect();
    RecordBatchStream::from_reader(RecordBatchIterator::new(parts, schema()))
}

/// A counter, the smallest resource.
pub struct Counter {
    value: i64,
}

#[bimetal::export]
impl Counter {
    #[bimetal(constructor)]
    #[must_use]
    pub fn new(start: i64) -> Self {
        Self { value: start }
    }

    /// Add `by`, and return the new value: a method that writes.
    #[bimetal(brief)]
    pub fn add(&mut self, by: i64) -> i64 {
        self.value = self.value.wrapping_add(by);
        self.value
    }

    /// Return the value: a method that reads.
    #[bimetal(brief)]
    #[must_use]
    pub fn get(&self) -> i64 {
        self.value
    }

    /// Add `by` after waiting once: an async method that writes.
    pub async fn add_later(&mut self, by: i64) -> i64 {
        yield_once().await;
        self.add(by)
    }
}

/// Return to the executor once, without any one runtime, so the same code
/// waits on Tokio and on the JavaScript event loop.
async fn yield_once() {
    let mut yielded = false;
    future::poll_fn(|context| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            context.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

/// Mix a value `rounds` times with xorshift, which the optimizer cannot fold.
fn mix(rounds: u32) -> u64 {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    for _ in 0..rounds {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
    }
    state
}

fn row(id: u32) -> Row {
    Row {
        id: u64::from(id),
        name: format!("row {id}"),
        score: f64::from(id) * 0.5,
        active: id.is_multiple_of(2),
        tags: vec!["a".to_owned(), "b".to_owned()],
        created_at: Utc
            .timestamp_opt(1_784_000_000 + i64::from(id), 0)
            .single()
            .expect("the timestamp is in range"),
    }
}

/// The schema of every batch: a float and an integer column.
fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("price", DataType::Float64, /* nullable */ false),
        Field::new("quantity", DataType::Int64, /* nullable */ false),
    ]))
}

/// A batch of `rows` rows whose quantities start at `first`.
fn batch(rows: u32, first: i64) -> RecordBatch {
    let quantity: Int64Array = (first..first + i64::from(rows)).collect();
    #[allow(
        clippy::cast_precision_loss,
        reason = "the probe's prices need not be exact"
    )]
    let price: Float64Array = quantity
        .values()
        .iter()
        .map(|value| *value as f64 * 0.5)
        .collect();
    let columns: Vec<ArrayRef> = vec![Arc::new(price), Arc::new(quantity)];
    RecordBatch::try_new(schema(), columns).expect("the columns match the schema")
}
