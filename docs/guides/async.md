# Async

An `async fn` becomes a Python coroutine and a TypeScript function that returns a `Promise`. [Async calls](../concepts/async.md) explains what runs them.

```rust
/// Count the words of a text asynchronously.
#[bimetal::export]
pub async fn count_words(text: String) -> u32 {
```

```python
self.assertEqual(await count_words("one two three"), 3)
```

```ts
assert.equal(await countWords("one two three"), 3);
```

A typed error rejects the awaitable with the error class, and a panic behaves as [Failures](../concepts/failures.md) describes.

## Async resources

Resource methods can be async too, with `&self`, `&mut self` or `self`. A Python `__init__` and a JavaScript `new` can't wait, so an async constructor becomes a factory:

```rust
#[bimetal::export]
impl Mailbox {
    /// Open a mailbox. Opening is async, so the class has no constructor.
    #[bimetal(constructor)]
    pub async fn open(recipient: String) -> Result<Self, ParseError> {
```

```python
async with await Mailbox.open("Ada") as mailbox:
    self.assertEqual(await mailbox.deliver("hello"), 1)
```

```ts
await using mailbox = await Mailbox.open("Ada");
assert.equal(await mailbox.deliver("hello"), 1);
```

An async `&mut self` method holds the resource until its result is ready. [Resources](../concepts/resources.md) says what other calls do meanwhile.

## What a future may hold

An exported future must be `Send` and `'static`. It can't hold an `Rc` or a `RefCell` borrow across an `.await`, and Bimetal checks this when the crate compiles. `&str` and `&[T]` parameters work, because the call owns its arguments, but an async export can't return a reference.

## Tokio

On Python and the addon, your code runs inside Bimetal's Tokio runtime, so `tokio::spawn`, timers and I/O work. The bank example's constructor spawns the task that settles its transfers, and a withdrawal waits on a timer:

```rust
pub async fn withdraw(&mut self, owner: String, amount: u64) -> Result<u64, TransferError> {
    self.locked().check(&owner, amount)?;
    tokio::time::sleep(SETTLEMENT).await;
    self.locked().withdraw(&owner, amount)
}
```

The Wasm module has no Tokio runtime, and the JavaScript event loop runs your futures. Runtime-independent code, such as `tokio::sync`, works there. Code that needs Tokio's I/O, timers or `tokio::spawn` has three options: choose an implementation per target with `#[cfg(target_arch = "wasm32")]`, use the [Node.js backend](node.md), or use a current-thread runtime of your own.

The third suits a library that spawns tasks internally, as DataFusion does, as long as nothing it awaits needs I/O or a timer:

```rust
#[cfg(target_arch = "wasm32")]
async fn run<F: Future>(future: F) -> F::Output {
    thread_local! {
        static RUNTIME: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime needs no I/O");
    }
    RUNTIME.with(|runtime| runtime.block_on(future))
}
```

On native hosts the same function just awaits. Calling a blocking function from a future stops a Tokio worker on Python and the addon, as it would in any Tokio program, and before the future's first wait it stops the caller's event loop. So use `spawn_blocking` for long work.
