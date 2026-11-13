# The contract

The *contract* is the public API that Bimetal exports: your models, functions, errors, resources and constants. There is no interface file. Your Rust types are the only definition, and Bimetal reads them from compiled code.

## What marks an item

`#[derive(Model)]` marks a type that crosses as data, `#[derive(bimetal::Error)]` an error callers can branch on, and `#[bimetal::export]` a function, constant or resource. Everything else stays private to Rust.

Each marked item registers a description of itself when your crate compiles. To read those descriptions, Bimetal builds your crate as a library for this machine, loads it, and asks it for the contract, as JSON. So the contract reflects what the compiler actually saw: every `cfg`, every macro expansion, every type a build script generated. A tool that parsed your source couldn't see those.

## What each thing becomes

| In Rust | In Python | In TypeScript |
| --- | --- | --- |
| A struct that derives `Model` | A frozen Pydantic model | A readonly interface |
| A unit-only enum | A `StrEnum` | A string union, with a frozen object of the same name |
| An internally tagged enum | A discriminated union | A discriminated union |
| An enum in Serde's default form | A model per variant, in a union picked by the variant's name | A union of each variant's name or one-key object |
| An exported function | A function | A function |
| An `async fn` | A coroutine | A function that returns a `Promise` |
| An error type | A `RuntimeError` subclass with a `code` | An `Error` subclass with a `code` |
| An exported `impl` block | A class | A class |
| A Rust module | A subpackage | A subpath of the package |
| A doc comment | A docstring | JSDoc |

The names follow each language: `count_tags` stays `count_tags` in Python, and is `countTags` in TypeScript. Model fields follow your `#[serde(rename_all)]`, since Serde decides what crosses. [Names](../reference/names.md) has the exact rules.

## Why compiled code, and not source

Reading compiled code costs a build, which is why every Bimetal application builds a small extra library for discovery, unoptimized and incremental. In exchange, three things hold:

- The generated code can't disagree with the Rust. Whatever Rust compiled is what the hosts see.
- Bimetal rejects what it can't represent while you build, and names the Rust item, instead of generating something subtly wrong.
- A macro that generates exports works, because discovery runs after expansion.

One gap remains. The contract comes from a build for this machine, and the Wasm module is built for `wasm32`. Bimetal compares the module's exports with the contract and stops if a function, resource or method is missing on either side. It can't see a difference inside an export, such as a `cfg_attr` that renames a field on one target, or a constant whose value depends on the target. Every package takes those from this machine's build.

## One signature, every host

A signature means the same thing on each host, so you describe it once. Where a host needs more than the Rust type says, an attribute says it: `#[bimetal(buffer)]` for a NumPy array, `#[bimetal(arrow)]` for an Arrow value, `#[bimetal(default = 10)]` for an optional argument. [Attributes](../reference/attributes.md) lists them all.

Everything else follows from the signature. `&mut self` means a method runs alone, `Result` means it can raise, `self` means it consumes the resource. You don't configure these.

## Checks when you build

`bimetal build` reports every problem it finds in one error, naming the Rust items involved: names that collide after renaming, names a host reserves, shapes it can't carry, and project files that don't match. It checks only the hosts you turned on, so a name that only JavaScript reserves doesn't bother a Python-only application.
