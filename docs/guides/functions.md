# Functions

Put `#[bimetal::export]` on a public function, and each host gets a function of the same name. The function can't be generic. [`apps/examples/showcase`](../../apps/examples/showcase) has everything here, and its tests run these samples.

```rust
/// Count the words in a text.
#[bimetal::export]
#[must_use]
pub fn phrase(text: &str) -> Phrase {
```

```python
self.assertEqual(phrase("one two").word_count, 2)
```

```ts
assert.equal(phrase("one two").wordCount, 2);
```

The doc comment becomes the Python docstring and the TypeScript JSDoc. Each paragraph is joined and wrapped again at the generated file's width, while lists, code and headings keep their lines. Parameters and return types can be anything in [Types](../reference/types.md). A function can return `Result` with an [exported error](errors.md), and it can be [async](async.md).

Python names stay in snake case, and TypeScript functions, methods and parameters use lower camel case. A parameter a host can't use by its Rust name is renamed there, so `class` is `class_` in TypeScript and `class_value` in Python. [Names](../reference/names.md) has the rules, and what Bimetal refuses.

## Constants

Put `#[bimetal::export]` on a `pub const` or `pub static`:

```rust
/// The version of this contract.
#[bimetal::export]
pub const CONTRACT_VERSION: u32 = 3;

/// The label that new records start with.
#[bimetal::export]
pub static DEFAULT_LABEL: &str = "untitled";
```

Bimetal reads the value during the build and writes it into each package. A constant that breaks a [field rule](models.md#field-rules), or a `serde_json::Value` constant that holds NaN or infinity, fails `bimetal build`.

## Default arguments

`#[bimetal(default = <expression>)]` on a parameter lets a caller omit it. Rust always receives a value:

```rust
pub fn offset(
    value: i32,
    #[bimetal(default = -1)] by: i32,
    #[bimetal(default = 0.5)] scale: f64,
) -> f64 {
```

```python
self.assertEqual(offset(4), 1.5)
self.assertEqual(offset(4, scale=1.0), 3.0)
```

```ts
assert.equal(offset(4), 1.5);
assert.equal(offset(4, undefined, 1), 3);
```

A default is an expression of the parameter's type, such as `Duration::from_secs(1)` or a variant of a string enum, and a string literal also fits a `String`. The build evaluates it when it reads the contract, so it must give the same value every time. It may be a boolean, a number, a string, a date, a duration, a string enum, or a variant without data of an externally tagged enum, which the hosts write in their signatures. An `Option` parameter defaults to `None` when every parameter after it may be omitted too. Every parameter after an omissible one must be omissible as well.

TypeScript has no keyword arguments, so skipping a parameter takes an explicit `undefined`.

## Brief calls

Python releases its interpreter lock while Rust runs a call, so other Python threads keep going, and a thread that the call waits for can take the lock. `#[bimetal(brief)]` keeps the lock instead. It suits a synchronous call that is short and never waits, where releasing the lock and taking it back costs more than the call itself:

```rust
/// The smallest and largest value, or nothing for an empty list.
///
/// A brief call keeps Python's interpreter lock, and this one, which takes
/// and returns plain values, is the native function itself in Python.
#[bimetal::export]
#[bimetal(brief)]
#[must_use]
pub fn bounds(values: Vec<i32>) -> Option<(i32, i32)> {
```

```python
self.assertEqual(type(bounds).__name__, "builtin_function_or_method")
```

A brief free function whose Python function would only pass its arguments on is the native function itself: the module binds it in its place, so a call costs what the native call costs. That happens when no parameter is renamed or may be omitted, and nothing in Python converts an argument or the result. The bound function keeps its name, module and docstring, but has no annotations at run time, so `typing.get_type_hints` finds none.

Other Python threads wait while a brief call runs, so a brief call that works long stalls them. An async export can't be brief, and neither can a call that takes a `Callback` or a `RecordBatchStream`, which may wait for a thread that needs the lock. A brief method on a busy resource still releases the lock while it waits for the resource. JavaScript has no such lock, so `brief` changes nothing in TypeScript.

## Namespaces

Each Rust module becomes a namespace. `text` in `apps/examples/showcase` is `showcase.text` in Python and `showcase/text` in TypeScript:

```python
from showcase.text import phrase
```

```ts
import { phrase } from "showcase/text";
```

An application can also link other packages of its workspace, each of which becomes a namespace, as [Workspaces](workspaces.md) shows.
