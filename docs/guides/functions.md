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

`#[bimetal(default = <literal>)]` on a parameter lets a caller omit it. Rust always receives a value:

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

A default is a boolean, number or string literal that fits the parameter. A string enum, or a variant without data of an externally tagged enum, takes its wire name, and a `Duration` takes its nanoseconds. An `Option` parameter defaults to `None` when every parameter after it may be omitted too. Every parameter after an omissible one must be omissible as well.

TypeScript has no keyword arguments, so skipping a parameter takes an explicit `undefined`.

## Namespaces

Each Rust module becomes a namespace. `text` in `apps/examples/showcase` is `showcase.text` in Python and `showcase/text` in TypeScript:

```python
from showcase.text import phrase
```

```ts
import { phrase } from "showcase/text";
```

An application can also link other packages of its workspace, each of which becomes a namespace, as [Workspaces](workspaces.md) shows.
