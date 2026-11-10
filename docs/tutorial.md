# Tutorial

This tutorial writes a small greeter in Rust: a function with typed input and output, an error callers can branch on, and a guestbook that keeps its state in Rust between calls. Then it calls all of them from Python and TypeScript. Everything here is in [`apps/examples/greeter`](../apps/examples/greeter), whose tests run these samples.

You need Rust 1.94 or newer with the Wasm target (`rustup target add wasm32-unknown-unknown`), Python 3.11 or newer, and Node.js 20.16 or newer. [Limits and requirements](reference/limits.md) has the details.

## Install the command

```bash
cargo install --locked bimetal-cli
```

Each [release](https://github.com/2xf-org/bimetal/releases) also has the command built for macOS, Linux and Windows. The command and the `bimetal` crate your application depends on must have the same version, and Bimetal stops with an error if they differ.

## Create the application

```bash
bimetal new greeter
```

That writes a Cargo package and a project for each host:

```text
greeter/
├── Cargo.toml
├── bimetal.toml           turns on each host
├── src/lib.rs           your API
├── python/              a Python project, with pyproject.toml
└── typescript/          an npm project, with package.json
```

All of these files are yours. Bimetal adds a package inside each project when it builds.

## Write a function

Replace `src/lib.rs`. A *model* is a type that crosses to the hosts. Derive `Model` beside Serde's traits:

```rust
use bimetal::Model;
use serde::{Deserialize, Serialize};

/// How warmly to greet someone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// "Hi, Ada."
    Casual,
    /// "Good day, Ada."
    Formal,
}

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
```

`#[bimetal(min_length = 1, max_length = 64)]` is a rule that every host enforces. Doc comments become Python docstrings and TypeScript JSDoc.

An error is an enum that derives `bimetal::Error`:

```rust
/// Failures that callers handle by their stable `code`.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum GreeterError {
    /// The name contained only whitespace.
    #[error("the name is blank")]
    BlankName,
    /// The guestbook already holds `GUESTBOOK_CAPACITY` names.
    #[error("the guestbook is full")]
    GuestbookFull,
}
```

Then export a function:

```rust
/// Greet one person.
#[bimetal::export]
pub fn greet(request: GreetingRequest) -> Result<Greeting, GreeterError> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(GreeterError::BlankName);
    }
```

The example has the rest, with `Greeting`, a model of the same kind, and `thiserror` in `Cargo.toml`.

## Build

```bash
bimetal build
```

The first build compiles every dependency for each host and takes a while. Later builds reuse them, and a build where nothing changed compiles nothing:

```text
Compiling greeter: contract discovery, WebAssembly module, Python extension
  Compiled the contract discovery in …s
  Compiled the WebAssembly module in …s
  Compiled the Python extension in …s
Built greeter 0.1.0.
  Python package greeter in python
  npm package greeter in typescript
```

Bimetal compiled your Rust once for each host, read the API from the compiled code, and wrote a package into each project. [Packages and projects](concepts/packages.md) shows what it wrote.

## Call it from Python

`python/` is an ordinary project, so `uv run --project python python` or `pip install ./python` both work:

```python
from greeter import GreeterError, GreetingRequest, Tone, greet

greeting = greet(GreetingRequest(name="Ada", tone=Tone.Formal, excited=True))
print(greeting.message)
```

`GreetingRequest` is a frozen Pydantic model and `Tone` is a `StrEnum`. The package ships `py.typed` and passes `mypy --strict`, so your editor knows every type. The `min_length` rule runs where the model is built:

```python
with self.assertRaises(ValidationError):
    GreetingRequest(name="", tone=Tone.Casual)
```

The error from Rust is a class with a stable `code`, which is what you branch on:

```python
with self.assertRaises(GreeterError) as caught:
    greet(GreetingRequest(name="   ", tone=Tone.Casual))
self.assertEqual(caught.exception.code, "blank_name")
```

## Call it from TypeScript

`typescript/` is an ordinary npm package. Models are plain objects with the types of your Rust:

```ts
import { GUESTBOOK_CAPACITY, GreeterError, Guestbook, Tone, greet } from "greeter";

assert.equal(greet({ name: "Ada", tone: Tone.Casual }).message, "Hi, Ada.");
```

By default the package runs your Rust as a WebAssembly module, so the same package works in Node.js and in a browser. A browser loads the module asynchronously, so a page first awaits `nativeReady`, as [Browsers](guides/browser.md) explains.

## Keep state in Rust

A *resource* is a Rust value that stays alive between calls. Put `#[bimetal::export]` on its `impl` block:

```rust
/// The names signed for one event, kept in Rust between host calls.
pub struct Guestbook {
    event: String,
    names: Vec<String>,
}

#[bimetal::export]
impl Guestbook {
    /// Open an empty guestbook.
    #[bimetal(constructor)]
    #[must_use]
    pub fn new(event: String) -> Self {
```

Run `bimetal build` again, and each host has a class. `with` in Python and `using` in TypeScript free the Rust value when the block ends:

```python
with Guestbook("launch") as guestbook:
    guestbook.sign("Ada")
    self.assertEqual(guestbook.names(), ["Ada"])
```

```ts
using guestbook = new Guestbook("launch");
for (const name of ["Ada", "Grace", "Linus"].slice(0, GUESTBOOK_CAPACITY)) {
  guestbook.sign(name);
}
```

## Keep the packages current

Commit the generated files with the Rust change that caused them. CI then needs one line, which fails if anyone changed the Rust without rebuilding:

```bash
bimetal check
```

Don't commit the compiled extension or addon, since each platform builds its own. In a fresh clone, `bimetal build` compiles only those. [Keeping packages current](guides/current.md) has the rest.

## Where to go next

Read the [concepts](README.md) in order, and the [guides](README.md#guides) for whatever your API needs. [`apps/examples/showcase`](../apps/examples/showcase) uses every feature, and [`apps/trials/`](../apps/trials) wraps five large projects.
