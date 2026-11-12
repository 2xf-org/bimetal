# Arrow

Apache Arrow values cross as Arrow values on every host. In Python, nothing is copied. Enable the `arrow` feature of `bimetal`:

```toml
bimetal = { version = "=202611.12.0", features = ["arrow"] }
```

Then annotate a top-level parameter with `arrow`, or the export with `returns(arrow)`:

```rust
use bimetal::arrow::{
    ArrayRef, ArrowError, DataType, Field, RecordBatch, RecordBatchStream, Schema, Table,
};

/// Split a batch into a table of batches with at most `size` rows.
#[bimetal::export]
#[bimetal(returns(arrow))]
#[must_use]
pub fn split(#[bimetal(arrow)] batch: &RecordBatch, size: u32) -> Table {
```

```python
self.assertEqual(split(batch, 1).num_rows, 2)
```

```ts
assert.equal(split(batch, 1).numRows, 2);
```

| Rust | Python parameter | Python return | TypeScript |
| --- | --- | --- | --- |
| `ArrayRef` | `ArrowArrayExportable` | `pyarrow.Array` | `Vector` |
| `RecordBatch` | `ArrowArrayExportable` | `pyarrow.RecordBatch` | `RecordBatch` |
| `bimetal::arrow::Table` | `ArrowStreamExportable` | `pyarrow.Table` | `Table` |
| `Schema`, `SchemaRef` | `ArrowSchemaExportable` | `pyarrow.Schema` | `Schema` |
| `bimetal::arrow::RecordBatchStream` | `ArrowStreamExportable` | `pyarrow.RecordBatchReader` | [`RecordBatchStream`, `RecordBatchSource`](#streams-in-typescript) |

`bimetal::arrow` re-exports `arrow_array` and `arrow_schema` from arrow-rs, which keeps your Arrow types the same as the runtime's. Use the re-exports rather than another `arrow` dependency.

A `Table` is a schema with any number of batches that all use it. Unlike a list of batches, it keeps its schema when it has no batches. A return can be an `Option` of an Arrow value, for a `next` method. Models can't contain Arrow fields.

## Streams

A `RecordBatchStream` is a schema and batches that cross one at a time, made as the reader asks for them. Return one, and Python receives a `pyarrow.RecordBatchReader`, and TypeScript a [`RecordBatchStream`](#streams-in-typescript) to read with `for await`:

```rust
pub fn count(batches: u32, size: u32, fail_at: Option<u32>) -> RecordBatchStream {
    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
    let batch_schema = schema.clone();
    let numbers = stream::iter(0..batches).map(move |batch| {
        if fail_at == Some(batch) {
            return Err(StreamError::Stopped { batch });
        }
        let start = i64::from(batch) * i64::from(size);
        let column = Int64Array::from_iter_values(start..start + i64::from(size));
        let numbers = RecordBatch::try_new(batch_schema.clone(), vec![Arc::new(column)])
            .expect("the column matches the schema");
        Ok(numbers)
    });
    RecordBatchStream::new(schema, numbers)
}
```

```python
reader = count(3, 4)
self.assertIsInstance(reader, pa.RecordBatchReader)
self.assertEqual(reader.read_next_batch().column(0).to_pylist(), [0, 1, 2, 3])
```

`RecordBatchStream::new` takes the schema and any `Stream` of `Result<RecordBatch, E>`, where `E` is an `ArrowError` or a typed error. `RecordBatchStream::from_reader` takes a blocking arrow-rs `RecordBatchReader` instead. pyarrow, DuckDB and Polars read the stream from any thread, with or without the interpreter lock, so a DuckDB query pulls it from its own threads:

```python
connection = duckdb.connect()
connection.execute("SET threads TO 4")
reader = count(100, 1000)
result = connection.sql("SELECT sum(n) FROM reader").fetchall()
```

Take a `#[bimetal(arrow)] RecordBatchStream` parameter, and it reads any object with `__arrow_c_stream__`, such as a pyarrow `RecordBatchReader`, a DuckDB relation or a Polars `DataFrame`, one batch at a time as Rust asks. Synchronous code iterates it:

```rust
pub fn total(
    #[bimetal(arrow)] rows: RecordBatchStream,
    limit: Option<u32>,
) -> Result<i64, StreamError> {
    let limit = limit.map_or(usize::MAX, |limit| limit as usize);
    let mut total = 0;
    for batch in rows.into_iter().take(limit) {
        total += sum(&batch?)?;
    }
    Ok(total)
}
```

Async code reads it as a `futures::Stream`:

```rust
pub async fn total_later(
    #[bimetal(arrow)] mut rows: RecordBatchStream,
) -> Result<i64, StreamError> {
    let mut total = 0;
    while let Some(batch) = rows.next().await {
        total += sum(&batch?)?;
    }
    Ok(total)
}
```

```python
self.assertEqual(total(source()), sum(range(1000)))  # type: ignore[arg-type]
self.assertEqual(
    asyncio.run(total_later(source())),  # type: ignore[arg-type]
    sum(range(1000)),
)
```

A Python source may need the interpreter lock to make a batch, as a reader over a generator does, so nothing holds the lock while Rust waits for one. A synchronous export reads on its calling thread, after it releases the lock. An async export that takes a stream starts on a runtime worker, not on the caller's thread, and reads each batch on a blocking thread, so the source runs while the event loop waits. A pull from Python releases the lock too, if the puller holds it, and an async `RecordBatchStream::new` stream runs on the puller's thread inside Bimetal's runtime.

Each batch must have the stream's schema, and one that doesn't fails the read with a `SchemaError`. Imported batches are validated, as other Arrow arguments are, and no batch is copied in either direction.

### When a stream fails

A failure reaches Python as the message of the Arrow C stream interface. pyarrow raises `ArrowInvalid` with it, and DuckDB and Polars raise errors whose messages hold it. A typed error keeps its code before its message, and its details stay behind:

```python
reader = count(3, 2, fail_at=1)
self.assertEqual(reader.read_next_batch().num_rows, 2)
for _ in range(2):
    with self.assertRaises(pa.ArrowInvalid) as caught:
        reader.read_next_batch()
    self.assertEqual(
        str(caught.exception), "External error: stopped: the count stopped at batch 1"
    )
```

`External error: ` is arrow-rs's prefix. An `ArrowError` keeps its own text, and a panic reads `panic: ` and its message. After the first failure, every read fails the same way.

On the way in, a source that fails gives Rust an `Err` for the batch that failed, an `ArrowError` that carries the source's message, such as a Python exception's.

### When a stream drops

Dropping a `RecordBatchStream` drops its source, which cancels the source's work. A returned stream drops as soon as it ends or fails, or when Python closes or deletes the `RecordBatchReader`. A parameter's source is released when Rust drops the stream, so a Python generator closes once Python drops its own reader too.

Once Bimetal's runtime has started, as it does for an async call or for the first read of an async stream, a child that `fork` makes can't read a stream that its parent returned: every read fails, and the child never drops it, because the source may hold tasks of the parent's runtime.

### Streams in TypeScript

A returned stream is a `RecordBatchStream`: an async iterator of apache-arrow `RecordBatch`es, with the stream's `schema`. Each `next()` asks Rust for one batch, and `close()`, a `break` out of `for await`, or `await using` drops the stream in Rust:

```ts
const stream: RecordBatchStream = batches(split(prices(5).batches[0]!, 2));
for await (const batch of stream) {
```

A parameter takes a `RecordBatchSource`: a `Table`, an apache-arrow `RecordBatchReader`, a `RecordBatchStream`, or any iterable or async iterable of `RecordBatch`es. The source writes each batch when Rust asks for it, and closes once Rust drops the stream:

```rust
pub async fn totals(
    #[bimetal(arrow)] rows: RecordBatchStream,
) -> Result<RecordBatchStream, ColumnError> {
    let mut fields = rows.schema().fields().to_vec();
    fields.push(Arc::new(Field::new("total", DataType::Float64, true)));
    let totals = rows.map(|batch| add_totals(&batch?));
```

```ts
assert.equal(await sumColumn(table, "price"), total);
assert.equal(await sumColumn(RecordBatchReader.from(tableToIPC(table)), "price"), total);
```

JavaScript makes a source's batches asynchronously, and a synchronous call on its thread could never wait for them, so only an async export takes a stream there. `bimetal build` refuses a synchronous one when the contract has a TypeScript package, and Rust code that blocks to read such a stream gets an `Err` instead of waiting. An iterable without a `schema`, such as an array of batches, gives its first batch's, which the wrapper reads before the call, so an empty one throws a `TypeError`. Each batch takes the source's schema, whose metadata a table's batches lack. If the call fails before Rust reads the stream, the wrapper closes the source.

A failure rejects that `next()` and every later one with an `Error`, whose message is the same as in Python but without arrow-rs's prefix, such as `missing_column: the batch has no such column`. A source that throws gives Rust an `Err` with its message. Each batch crosses as an IPC stream of its own, with the copies of other Arrow values. A source that waits for Rust's next request doesn't keep Node.js running.

## Choosing a version of arrow-rs

Rust treats two versions' types as different, so your Arrow values must be the types of your other Arrow libraries. The `arrow` feature uses arrow-rs 60, and `arrow-59` uses arrow-rs 59. DataFusion 55 needs `arrow-59`. Enabling both is a compile error.

A library with an Arrow implementation of its own can still cross, through the C Data Interface. [`apps/trials/polars/src/bridge.rs`](../../apps/trials/polars/src/bridge.rs) does this for Polars, without a copy.

## In Python

Parameters accept any object with the [Arrow PyCapsule interface](https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html), such as pyarrow, Polars or DuckDB values, and nothing is copied in either direction. Bimetal validates imported data before Rust reads it. Results are always pyarrow objects.

The project must depend on `pyarrow>=16`, the first release that allows NumPy 2, which `bimetal build` checks. pyarrow ships no type hints, so add `pyarrow-stubs` to the development dependencies to check a package with `mypy --strict` or strict pyright.

## In TypeScript

Values cross as an Arrow IPC stream, on both backends: the stream, and one copy into Rust's memory, each way. A chunked `Vector` is joined into one array. apache-arrow 21 can't read `ListView` or `RunEndEncoded` columns.

The npm package must list `apache-arrow` in `peerDependencies`, which `bimetal build` checks, so that it shares your application's copy: the apache-arrow classes recognize only their own instances. A value from another copy throws a `TypeError` that says so. Two copies usually come from a package linked with a `file:` dependency, which loads apache-arrow from its own `node_modules`, or from a bundler that doesn't deduplicate it. TypeScript checks the declarations of apache-arrow's `flatbuffers` dependency, which need TypeScript 5.7 or newer, unless `skipLibCheck` is on.
