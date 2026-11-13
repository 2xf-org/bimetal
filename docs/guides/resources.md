# Resources

A resource is a Rust value that keeps state between calls, and [Resources](../concepts/resources.md) explains how calls share it. Put `#[bimetal::export]` on one inherent `impl`, and mark its constructors with `#[bimetal(constructor)]`. [`apps/examples/showcase`](../../apps/examples/showcase) has everything here.

```rust
/// A balance kept in Rust between host calls.
pub struct Ledger {
    owner: String,
    entries: Vec<Entry>,
    locked: bool,
}

#[bimetal::export]
impl Ledger {
    /// Open an empty ledger.
    #[bimetal(constructor)]
    pub fn new(owner: String) -> Result<Self, ParseError> {
```

```python
with Ledger("Ada") as ledger:
    self.assertEqual(ledger.deposit(5), 5)
```

```ts
using ledger = new Ledger("Ada");
assert.equal(ledger.deposit(5n), 5n);
```

Document a resource on its `impl` block, which is what `#[bimetal::export]` sees. A doc comment on the struct doesn't reach the generated class. One resource has one exported `impl` block.

## Constructors

The synchronous constructor named `new`, or else the first synchronous one, becomes the class constructor. The others become a Python `@classmethod` and a TypeScript `static` method:

```rust
/// Open a ledger with a starting balance.
#[bimetal(constructor)]
#[must_use]
pub fn with_balance(owner: String, balance: i64) -> Self {
```

```python
self.assertEqual(Ledger.with_balance("Ada", 5).balance(), 5)
```

```ts
assert.equal(Ledger.withBalance("Ada", 5n).balance(), 5n);
```

Async constructors are always factories, so a resource whose constructors are all async has no class constructor. A resource needs no constructor at all when other exports return it, and its class then refuses `Session()` and `new Session()`.

## Methods

Every public method is exported, unless `#[bimetal(skip)]` keeps it in Rust. Methods can take `&self`, `&mut self` or `self`, can be async, and can return `Result` with an exported error.

```rust
/// Clear the history. Rust code can call this; hosts cannot.
#[bimetal(skip)]
pub fn clear(&mut self) {
```

## Features

A `#[cfg]` that tests a feature can gate a method, because every host of one build compiles the same features. The method is then in every package or in none, and so is a type that only the feature declares:

```rust
/// Sum up the ledger, with the `statements` feature.
#[cfg(feature = "statements")]
#[must_use]
pub fn statement(&self) -> Statement {
```

```python
self.assertEqual(ledger.statement(), Statement(owner="Ada", balance=7, entries=2))
```

```ts
assert.deepEqual(ledger.statement(), { owner: "Ada", balance: 7n, entries: 2 });
```

Each test in the `#[cfg]` must be `feature = "..."`, and `all`, `any` and `not` can join them. A test of the target, such as `target_arch = "wasm32"`, `unix` or `windows`, is a compile error, because the hosts share one contract but not one target. Put such a `#[cfg]` inside the method's body, and return an error where the method can't run. Write the gate as a `#[cfg]` of its own: one inside a `#[cfg_attr(...)]` is a compile error, and so is an empty `any()`, which holds in no build.

A gated constructor is always a factory, so the class has the same constructor in every build. When every constructor is gated, the class has no constructor, and only its factories create it.

`bimetal build` compiles each package with its default features only, and has no option to choose others. A gate on a feature that isn't in `default` is therefore off in every generated package. Add the feature to `default` to export what it gates.

## Returning resources

A function or a method whose result is a resource's type returns a new object of its class, on its own or in an `Option` or a `Vec`:

```rust
/// Open a session, whose books join the shelf when it commits.
#[must_use]
pub fn session(&self) -> Session {
```

```rust
/// Open a session, unless the shelf is full.
#[must_use]
pub fn try_session(&self) -> Option<Session> {
```

```rust
/// Divide the books among shelves of at most `size` books.
#[must_use]
pub fn split(&self, size: u32) -> Vec<Shelf> {
```

## Passing resources

A parameter of a resource's type takes one, and the signature says how the call holds it:

| Parameter | The call | Other calls to that resource |
| --- | --- | --- |
| `other: &T` | Reads it | Other readers run, and a `&mut self` method waits |
| `other: &mut T` | Holds it alone | Wait |
| `other: Option<&T>` | Reads it, when one is passed | As for `&T` |
| `other: T` | Takes it, and closes the resource | Fail: the resource is closed |

```rust
/// Copy the books of another shelf, which is only read.
pub fn copy_from(&mut self, other: &Shelf) -> Result<(), ShelfError> {
```

```rust
/// Take over another shelf, which is closed afterwards.
pub fn absorb(&mut self, other: Shelf) {
```

An argument of another class raises `TypeError`, and a call that receives one resource twice fails.

## Consuming a resource

A method that takes `self` moves the value out and closes the resource, which suits builders and writers that finish once. It fails with an error saying the resource is busy while another call holds the value:

```rust
/// End the shelf, and return its books.
pub fn seal(self) -> Vec<String> {
```

```python
self.assertEqual(value.seal(), ["a", "b"])
```

```ts
assert.deepEqual(value.seal(), ["a", "b"]);
```

## Thread safety

The type must be `Send` and `Sync`. [Resources](../concepts/resources.md#bound-to-send-and-sync) says what to do with a field that isn't.

Host features such as `with`, `for` and `len()` come next, in [Protocols](protocols.md).
