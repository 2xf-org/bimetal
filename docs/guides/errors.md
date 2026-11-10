# Errors

Derive `bimetal::Error` on an enum that implements `Display`, and return it in a `Result`. Each host gets an error class: a `RuntimeError` subclass in Python, and an `Error` subclass in TypeScript.

```rust
/// Failures to parse text into a value.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum ParseError {
    /// The text was empty.
    #[error("the text is empty")]
    Empty,
    /// The text names no known value.
    #[error("unknown value `{text}`")]
    #[serde(rename = "unknown_value")]
    Unknown { text: String },
}
```

An instance has the Rust `Display` text as its message, and a stable `code`. The code is the variant's name in snake case, or its `#[serde(rename = "...")]` value:

```python
with self.assertRaises(ParseError) as caught:
    parse_priority("")
self.assertEqual(caught.exception.code, "empty")
```

```ts
assert.throws(
  () => parsePriority(""),
  (error: unknown) => error instanceof ParseError && error.code === "empty",
);
```

The type of `code` lists every code the error can report: `ParseErrorCode`, a `Literal[...]` in Python and a string union in TypeScript. Type checkers then catch a misspelled code, and TypeScript checks that a `switch` covers them all.

Branch on `code`, and not on the message. The message is for logs, so translate the code if you show the error to people.

A struct that derives `bimetal::Error` has one code, its own name in snake case:

```rust
/// A failure with a single code.
#[derive(Debug, thiserror::Error, bimetal::Error)]
#[error("the ledger is locked")]
pub struct LockedError;
```

## Details

`#[bimetal(details)]` on a variant, or on an error struct, gives its named fields to the hosts. Each becomes a generated model, named after the error and the variant, so the fields have the same types they have in Rust:

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
with self.assertRaises(ShelfError) as caught:
    full.add("c")
self.assertEqual(caught.exception.code, "full")
self.assertEqual(caught.exception.details, ShelfErrorFull(limit=1, title="c"))
```

```ts
assert.throws(
  () => full.add("c"),
  (error: unknown) => {
    assert.ok(error instanceof ShelfError);
    assert.equal(error.code, "full");
    assert.deepEqual(error.details, { limit: 1, title: "c" });
    return true;
  },
);
```

`details` is `None` or `null` for a code without details. The fields must derive `Serialize` and be contract types, and `#[serde(skip)]` leaves one out, such as a `#[source]`. They cross as JSON, which can't carry a 64-bit integer to TypeScript exactly, so `bimetal build` rejects those there: use a smaller integer or a `String`.

## Everything else

Bad arguments, panics and traps are not typed errors, and [Failures](../concepts/failures.md) says what each raises. An error declared in another package of the workspace works the same way, from that package's namespace.
