# Protocols

One attribute on a method makes it implement a feature of the host languages. The method stays callable by its name. [`apps/examples/showcase/src/shelf.rs`](../../apps/examples/showcase/src/shelf.rs) uses every one.

| Attribute | Signature | Python | TypeScript |
| --- | --- | --- | --- |
| `enter` | `&mut self` or `&self`, sync or async | `with` and `async with` run it first, and a value it returns is what `as` binds | A method |
| `exit` | One `bool`, returns `()`, sync or async | `with` and `async with` run it last | `using` and `await using` run it last |
| `next` | Returns `Option<T>`, sync or async | `for`, or `async for` when async | `for...of`, or `for await...of` when async |
| `getter` | Returns a value | A property | `get` |
| `setter` | `set_<name>` with one parameter, after the getter `<name>` | A property | `set` |
| `len` | Returns an integer | `len(value)` | `length` |
| `repr` | Returns `String` | `repr(value)` | What Node.js prints |
| `display` | Returns `String` | `str(value)` | `toString()` |
| `getitem` | One parameter, returns a value | `value[key]`, and `None` raises `KeyError` | A method |
| `setitem` | Two parameters, returns `()` | `value[key] = item` | A method |
| `delitem` | One parameter, returns `()` | `del value[key]` | A method |
| `contains` | One parameter, returns `bool` | `item in value` | A method |
| `eq` | One `&T` parameter of the resource's type, returns `bool` | `value == other`, which is `True` for the same object | A method |
| `hash` | Returns an integer | `hash(value)` | A method |
| `bool` | Returns `bool` | `bool(value)` | A method |
| `call` | Any | `value(...)` | A method |

Only `enter`, `exit` and `next` may be async. `bimetal build` rejects a method whose signature doesn't fit. A method implements one protocol, so `repr` and `display` take a method each.

## Properties and size

```rust
/// The shelf's name.
#[bimetal(getter)]
#[must_use]
pub fn name(&self) -> String {
    self.name.clone()
}

/// Rename the shelf.
#[bimetal(setter)]
pub fn set_name(&mut self, name: String) {
    self.name = name;
}

/// How many books the shelf holds.
#[bimetal(len)]
pub fn size(&self) -> usize {
```

```python
value.name = "history"
self.assertEqual(value.name, "history")
self.assertEqual(len(value), 0)
```

```ts
value.name = "history";
assert.equal(value.name, "history");
assert.equal(value.length, 0);
```

## `with` and `using`

Python's `with` and `async with`, and TypeScript's `using` and `await using`, all close the resource when the block ends, whatever happens in it. An `exit` method runs Rust code first, and is told whether the block failed. This is how a writer commits when a block finishes and rolls back when it raises:

```rust
/// End a `with` block: commit if it finished, and forget the books if it
/// failed.
#[bimetal(exit)]
pub fn finish(&mut self, failed: bool) -> Result<(), ShelfError> {
    if failed {
        self.pending.clear();
        return Ok(());
    }
    self.commit()
}
```

```python
with value.session() as session:
    session.add("a")
self.assertEqual(len(value), 1)
```

An error from `exit` raises from the block, and Python chains the block's own exception to it. `exit` doesn't run on a resource that's already closed, such as after `close()` or a `self` method.

JavaScript doesn't tell `[Symbol.dispose]` whether its block threw, so TypeScript always passes `failed = true`. Commit explicitly there, and let disposal roll back what's left:

```ts
using session = value.session();
session.add("a");
session.commit();
```

When `enter` or `exit` is async, a plain `with` raises `TypeError` and a plain `using` throws one, which says to use `async with` or `await using`.

## Iteration

A `next` method returns the next value, or `None` at the end. Its class then works in a loop, once:

```rust
/// The next title, or nothing at the end.
#[bimetal(next)]
pub fn next_title(&mut self) -> Option<String> {
    self.titles.next()
}
```

```python
self.assertEqual(list(value.titles()), ["a", "b"])
```

```ts
assert.deepEqual([...value.titles()], ["a", "b"]);
```

The async form works the same way, as [`apps/trials/datafusion`](../../apps/trials/datafusion/src/lib.rs) shows. To stream Arrow batches, return a [`RecordBatchStream`](arrow.md#streams) instead, which query engines read too:

```rust
/// The next title, or nothing at the end.
#[bimetal(next)]
pub async fn next_title(&mut self) -> Result<Option<String>, ShelfError> {
```

```python
self.assertEqual([title async for title in value.title_stream()], ["a", "b"])
```

```ts
for await (const title of stream) titles.push(title);
```
