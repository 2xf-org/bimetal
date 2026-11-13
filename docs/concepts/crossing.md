# How values cross

Every argument and result crosses a boundary between two memory managers, and how it crosses decides what the call costs. Bimetal has three ways, and you choose by how you write the signature.

| Way | For | Cost |
| --- | --- | --- |
| Converted | Models, strings, numbers, lists, maps | Walks the value, field by field |
| Copied whole | Bytes and buffers | One copy each way |
| Shared | Arrow, in Python | No copy |

## Converted values

A model or a list crosses as a tree of host objects, and Serde reads or writes it field by field. This is the most convenient way and the slowest per byte, and the cost grows with the number of fields, not the number of bytes.

Python reads a model instance as it is, and validates a dictionary passed for a model with Pydantic. A result comes back as an instance of the model's class, which the extension makes as Pydantic would validate it. Rust checks the field rules on arguments and results, so a model changed with `model_copy(update=...)`, which skips Pydantic, meets them too. TypeScript checks the outermost type of each argument before it enters Rust, and Rust reports anything deeper.

A very large JSON result crosses faster as text. Returning 4.7 MB as a `serde_json::Value` took about 190 ms in the Wasm module, and returning it as a `String` for `JSON.parse` took about 30 ms.

## Bytes and buffers

Bytes and `#[bimetal(buffer)]` cross as one block of memory: a `bytes` object or a `Uint8Array`, and a NumPy array or a typed array. There is no per-element cost. [Bytes and buffers](../guides/bytes.md) shows them.

A `Vec<u8>` parameter or return value is bytes, but a `Vec<u8>` field of a model is a list of integers, and a model can't hold a buffer, so send large payloads as separate parameters.

## Arrow

Arrow arrays, batches, tables and schemas cross as Arrow, and this is where the hosts differ most.

| Host | Into Rust | Out of Rust | How |
| --- | --- | --- | --- |
| Python | No copy | No copy | The Arrow PyCapsule interface, around the C Data Interface |
| Wasm | A stream and one copy | A stream and one copy | An Arrow IPC stream |
| Node.js addon | A stream and one copy | A stream and one copy | An Arrow IPC stream |

A [`RecordBatchStream`](../guides/arrow.md#streams) crosses one batch at a time, each batch as one of these values: without a copy in Python, and as an IPC stream with one copy in JavaScript.

JavaScript can't share here. It can't keep a view into Wasm memory, since growing it detaches views, and apache-arrow has no C Data Interface for the addon. [Arrow](../guides/arrow.md) has the details. On a laptop, passing a batch of two fixed-width columns in and out took about 5 µs in Python, from 1,000 to 4,000,000 rows, and about 2 ms in JavaScript at a million rows, which grows with the batch.

## Counting copies

The rule for working on Bimetal is that each value decodes once and moves. Bytes make one copy in and one out on every host, and a Rust function that builds a new `Vec` adds its own. `mise run bench` checks these counts against what each case holds at its peak.

## What can't cross

Serde can't carry a resource or a function, so both cross as host objects on paths of their own: a resource by its type, and a function as a `bimetal::Callback`. Neither can sit inside a model, and neither can a buffer or Arrow.

A few shapes can't round-trip, so Bimetal rejects them while you build: directional Serde renames such as `rename(serialize = "...")`, Serde aliases, and `Option<Option<T>>`. [Types](../reference/types.md) has the whole list.
