# Callbacks

A parameter of type `bimetal::Callback<A, R>` takes a Python callable or a JavaScript function, which Rust keeps and calls. `A` is a tuple of the arguments, of contract types that serialize, and `R` is the result, of a contract type that deserializes.

```rust
/// Call `visitor` with each title and its position until it returns
/// false, and return how many it saw.
pub fn visit(&self, visitor: Callback<(String, u32), bool>) -> Result<u32, ShelfError> {
    let titles = self.books.lock().expect(UNPOISONED).clone();
    let mut seen = 0;
    for (index, title) in titles.into_iter().enumerate() {
        seen += 1;
        if !visitor.call((title, index as u32))? {
            break;
        }
    }
    Ok(seen)
}
```

```python
self.assertEqual(value.visit(lambda title, index: index < 0), 1)
```

```ts
assert.equal(value.visit((_title, index) => index < 0), 1);
```

Arguments reach the function as an export's results reach its caller, models as models, and its result crosses as an export's arguments do.

## When the function fails

`call` returns `Err(CallbackError)` with the exception's text when the function raises, or when a value can't cross. Map it into your error type. The original exception doesn't reach the caller:

```rust
impl From<CallbackError> for ShelfError {
    fn from(error: CallbackError) -> Self {
        Self::Visitor {
            reason: error.to_string(),
        }
    }
}
```

## Keeping a callback

A callback can be cloned and kept after the call that received it, and it's `Send` and `Sync` on every host. Rust holds the function alive for as long as it holds the `Callback`:

```rust
/// Names shelves with a host function that it keeps.
pub struct Labeler {
    label: Callback<(String,), String>,
}
```

## Which thread it runs on

Calls may come from any thread:

- In Python, a call takes the interpreter lock.
- In the Wasm module, a call runs on the only thread there is.
- On the Node.js addon, a call on the JavaScript thread runs directly, and a call from another thread hands the work to the JavaScript thread and waits. A synchronous export blocks that thread, so it must not wait for a callback made on another thread. An async export leaves it free.

## Limits

The function can't be async, and models can't hold callbacks. `Option<Callback<A, R>>` isn't supported, so take a second method.

Rust doesn't check field rules on the arguments it passes to the function, though it checks what the function returns.

Inside the function, a synchronous method never waits for a resource's lock. If the lock is taken, as when the function calls a `&mut self` method of the resource whose call it's inside, the method fails with `the resource is busy`. [Resources](../concepts/resources.md#a-callback-that-uses-its-own-resource) explains why.
