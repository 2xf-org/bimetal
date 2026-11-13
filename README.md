# Bimetal

One Rust API, built into a Python package and a TypeScript package. Both call the same compiled code.

[Tutorial](docs/tutorial.md) · [Documentation](docs/README.md) · [White paper](docs/paper/bimetal.pdf) · [Examples](#examples)

## How it works

You mark the functions, types and `impl` blocks that callers may use. `bimetal build` compiles your library for each host, reads the API from the compiled code, and writes a typed package into a Python project and an npm project. The Python package is a native CPython extension. The TypeScript package runs your Rust as WebAssembly, in Node.js and browsers, or as a native Node.js addon. Field rules, error codes and documentation come from the Rust, so they are the same on every host. You commit the generated files, so an API change shows up in review, and `bimetal check` fails CI when they don't match the Rust.

## Getting started

Install the command with Cargo, or download it from a [release](https://github.com/2xf-org/bimetal/releases), which has it built for macOS, Linux and Windows:

```bash
cargo install --locked bimetal-cli
```

Create an application, which is a Cargo package with a project for each host:

```bash
bimetal new greeter
```

Build the packages, and check later that they still match the Rust:

```bash
bimetal build
```

```bash
bimetal check
```

Building needs Rust 1.94 with the Wasm target, Python 3.11 and Node.js 20.16. [The requirements](docs/reference/limits.md#requirements) have the details, and the [tutorial](docs/tutorial.md) goes through each step.

## A first API

Mark what crosses, in plain Rust:

```rust
/// A request to greet one person.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct GreetingRequest {
    /// The person to greet.
    #[bimetal(min_length = 1, max_length = 64)]
    pub name: String,
    /// How warmly to greet them.
    pub tone: Tone,
    /// End the greeting with an exclamation mark.
    #[serde(default)]
    pub excited: bool,
}

/// Greet one person.
#[bimetal::export]
pub fn greet(request: GreetingRequest) -> Result<Greeting, GreeterError> {
```

Python gets Pydantic models and docstrings:

```python
from greeter import GreeterError, GreetingRequest, Tone, greet

greeting = greet(GreetingRequest(name="Ada", tone=Tone.Formal, excited=True))
print(greeting.message)
```

TypeScript gets declarations and JSDoc:

```ts
import { GUESTBOOK_CAPACITY, GreeterError, Guestbook, Tone, greet } from "greeter";

assert.equal(greet({ name: "Ada", tone: Tone.Casual }).message, "Hi, Ada.");
```

A Rust error is an exception with a stable `code`:

```python
with self.assertRaises(GreeterError) as caught:
    greet(GreetingRequest(name="   ", tone=Tone.Casual))
self.assertEqual(caught.exception.code, "blank_name")
```

An `impl` block is a class, and its value stays in Rust between calls:

```rust
#[bimetal::export]
impl Guestbook {
    /// Open an empty guestbook.
    #[bimetal(constructor)]
    #[must_use]
    pub fn new(event: String) -> Self {
```

```python
with Guestbook("launch") as guestbook:
    guestbook.sign("Ada")
    self.assertEqual(guestbook.names(), ["Ada"])
```

```ts
using guestbook = new Guestbook("launch");
guestbook.sign("Ada");
assert.deepEqual(guestbook.names(), ["Ada"]);
```

## What each host gets

| Rust | Python | TypeScript |
| --- | --- | --- |
| A struct or enum with `Model` | A Pydantic model or `StrEnum` | An interface or string union |
| An error type | An exception with a `code` | An `Error` with a `code` |
| An `impl` block | A class, with `with` and `for` | A class, with `using` and `for...of` |
| An `async fn` | A coroutine | A `Promise` |
| A `bimetal::Callback` | Any callable | Any function |
| Bytes and numeric buffers | `bytes` and NumPy arrays | `Uint8Array` and typed arrays |
| Arrow values | pyarrow, Polars or DuckDB, without a copy | apache-arrow |

Each kind below is in [`apps/examples/showcase`](apps/examples/showcase), whose tests run these samples.

<details>
  <summary><code>async</code>: An <code>async fn</code> is a coroutine in Python and returns a <code>Promise</code> in TypeScript.</summary><br>

```rust
/// Count the words of a text asynchronously.
#[bimetal::export]
pub async fn count_words(text: String) -> u32 {
```

```python
self.assertEqual(await count_words("one two three"), 3)
async with await Mailbox.open("Ada") as mailbox:
    self.assertEqual(await mailbox.deliver("hello"), 1)
```

```ts
assert.equal(await countWords("one two three"), 3);
await using mailbox = await Mailbox.open("Ada");
assert.equal(await mailbox.deliver("hello"), 1);
```

</details>

<details>
  <summary><code>Callback</code>: Rust keeps a Python or JavaScript function and calls it.</summary><br>

```rust
pub fn visit(&self, visitor: Callback<(String, u32), bool>) -> Result<u32, ShelfError> {
```

```python
value = books("a", "b")
self.assertEqual(value.visit(lambda title, index: index < 0), 1)
```

```ts
const value = books(["a", "b"]);
assert.equal(value.visit((_title, index) => index < 0), 1);
```

</details>

<details>
  <summary><code>bytes</code> and <code>buffer</code>: Large payloads cross as one block of memory.</summary><br>

```rust
#[bimetal::export]
#[must_use]
pub fn reverse(bytes: &[u8]) -> Vec<u8> {
```

```rust
#[bimetal::export]
#[bimetal(returns(buffer))]
#[must_use]
pub fn running_total(#[bimetal(buffer)] values: &[i64]) -> Vec<i64> {
```

```python
self.assertEqual(reverse(b"abc"), b"cba")
self.assertEqual(running_total(np.array([1, 2, 3], dtype=np.int64)).tolist(), [1, 3, 6])
```

```ts
assert.deepEqual(reverse(new Uint8Array([1, 2, 3])), new Uint8Array([3, 2, 1]));
assert.deepEqual(runningTotal(new BigInt64Array([1n, 2n, 3n])), new BigInt64Array([1n, 3n, 6n]));
```

</details>

<details>
  <summary><code>arrow</code>: Record batches, tables and streams. Python shares their memory with Rust.</summary><br>

```rust
/// Split a batch into a table of batches with at most `size` rows.
#[bimetal::export]
#[bimetal(returns(arrow))]
#[must_use]
pub fn split(#[bimetal(arrow)] batch: &RecordBatch, size: u32) -> Table {
```

```python
batch = pa.record_batch({"price": [1.5, 2.5]})
self.assertEqual(split(batch, 1).num_rows, 2)
```

```ts
const batch = tableFromArrays({ price: new Float64Array([1.5, 2.5]) }).batches[0]!;
assert.equal(split(batch, 1).numRows, 2);
```

</details>

<details>
  <summary><code>details</code>: An error can carry typed fields beside its code.</summary><br>

```rust
/// The shelf is full.
#[error("the shelf holds at most {limit} books")]
#[bimetal(details)]
Full {
    /// How many books the shelf holds.
    limit: u32,
    /// The book that did not fit.
    title: String,
},
```

```python
self.assertEqual(caught.exception.code, "full")
self.assertEqual(caught.exception.details, ShelfErrorFull(limit=1, title="c"))
```

```ts
assert.equal(error.code, "full");
assert.deepEqual(error.details, { limit: 1, title: "c" });
```

</details>

## Documentation

- The [tutorial](docs/tutorial.md) builds the greeter above from an empty folder.
- The [concepts and guides](docs/README.md) cover each kind of export, each host and publishing.
- The [internals](docs/README.md#internals) are for anyone changing Bimetal itself, and [Testing](docs/internals/testing.md) has the commands to run before a pull request.

## Examples

- [`apps/examples/greeter`](apps/examples/greeter): one Cargo package, with the default projects beside it.
- [`apps/examples/showcase`](apps/examples/showcase): every kind of export, from two packages of one workspace.
- [`apps/examples/trading`](apps/examples/trading): a workspace laid out as a larger application is, with a Tokio engine on the Node.js backend.
- [`apps/examples/streams`](apps/examples/streams): streams of Arrow record batches, which pyarrow, DuckDB and Polars read and pass, in Python only.
- [`apps/trials`](apps/trials): DataFusion, Oxc, Polars, Tantivy and Typst behind Bimetal, each in a wrapper of a few hundred lines. [Their README](apps/trials/README.md) has the results.

## White paper

[Bimetal: One Rust API for Python, Node.js and the Browser](docs/paper/bimetal.pdf) describes the design, what each boundary promises, and what a call and a build cost. To cite it:

```bibtex
@techreport{faria2026bimetal,
  author = {Felipe Faria},
  title  = {Bimetal: One Rust API for Python, Node.js and the Browser},
  type   = {White paper},
  year   = {2026},
  month  = nov,
  url    = {https://github.com/2xf-org/bimetal/blob/main/docs/paper/bimetal.pdf}
}
```

## License

Bimetal is licensed under either the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option.
