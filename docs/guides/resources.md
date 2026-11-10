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

## Returning resources

`#[bimetal(returns(resource))]` returns a resource from a function or a method, on its own or in an `Option` or a `Vec`. `Self` doesn't work here, so write the resource's name:

```rust
/// Open a session, whose books join the shelf when it commits.
#[bimetal(returns(resource))]
#[must_use]
pub fn session(&self) -> Session {
```

```rust
/// Open a session, unless the shelf is full.
#[bimetal(returns(resource))]
#[must_use]
pub fn try_session(&self) -> Option<Session> {
```

```rust
/// Divide the books among shelves of at most `size` books.
#[bimetal(returns(resource))]
#[must_use]
pub fn split(&self, size: u32) -> Vec<Shelf> {
```

## Passing resources

`#[bimetal(resource)]` takes one as a parameter. The signature says how the call holds it:

| Parameter | The call | Other calls to that resource |
| --- | --- | --- |
| `other: &T` | Reads it | Other readers run, and a `&mut self` method waits |
| `other: &mut T` | Holds it alone | Wait |
| `other: Option<&T>` | Reads it, when one is passed | As for `&T` |
| `other: T` | Takes it, and closes the resource | Fail: the resource is closed |

```rust
/// Copy the books of another shelf, which is only read.
pub fn copy_from(&mut self, #[bimetal(resource)] other: &Shelf) -> Result<(), ShelfError> {
```

```rust
/// Take over another shelf, which is closed afterwards.
pub fn absorb(&mut self, #[bimetal(resource)] other: Shelf) {
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
