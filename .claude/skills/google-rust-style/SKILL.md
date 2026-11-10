---
name: google-rust-style
description: Write, edit, review, and refactor Rust in the Bimetal repository the way Google's Rust style guide (https://google.github.io/styleguide/rustguide.html) asks, with Bimetal's recorded decisions about where each rule applies. Use this whenever you add or change Rust code here, including the macros, the runtime, the CLI, tests, fixtures, and the example applications, and whenever someone asks for a style review, a cleanup, a refactor, idiomatic Rust, or "does this follow the guide". Check new code against it before you call a change done, even when nobody mentions style.
---

# Google Rust style in Bimetal

Google's guide builds on the public Rust style (rustfmt and the Rust API
Guidelines) and adds rules aimed at readability, reviewability, and tooling
that can refactor many files at once. This skill condenses every rule that
applies to a Cargo workspace like Bimetal, says why it exists, and records the
decisions Bimetal has made where the guide leaves room. Read the guide itself
for its examples: https://google.github.io/styleguide/rustguide.html

## Where the rules apply

**Library code** follows the API-design rules strictly. **Application code**
treats them as strong recommendations and prefers simple, direct code over
generality.

| Code | Kind |
| --- | --- |
| `packages/runtime`, `packages/ir`, `packages/macros` | Library: applications depend on them |
| `apps/cli`, tests, fixtures, `apps/examples/*` | Application |

Every other rule applies to all of it.

**Rules that do not apply here.** The guide scopes these to Chromium, Fuchsia,
or Android: prefer single-file crates, keep a crate's files in one flat
directory, `chromium::import!`, no `async`/`.await`, no `std::thread`,
`fuchsia_sync::Mutex`, and per-codebase test frameworks. Bimetal uses the
standard Cargo layout, async exports, and `#[test]`.

**Enforced automatically.** `mise run lint` runs rustfmt and Clippy with the
workspace lints in the root `Cargo.toml`, which turn several rules below into
errors. Run it before finishing. The rules a lint cannot check are yours to
check.

## Code organization

- **Name a leaf module and its file after its primary item**: `Contract` lives
  in `contract.rs`.
- **Keep non-leaf modules thin.** A module with submodules (`mod.rs`, or a
  file with `mod child;`) holds module declarations and re-exports, plus at
  most a little code that ties the children together. Put real implementation
  in leaf modules. Inline `mod tests`, binding modules, macro-generated wrapper
  modules, and a sealed-trait `mod sealed { pub trait Sealed {} }` (the Rust
  API Guidelines idiom) do not make a module non-leaf. A proc-macro crate's
  `lib.rs` must define its macro entry points, so it keeps them, and nothing
  else.
- **No glob imports.** Allowed only for `use super::*` inside an *inline*
  module (tests, macro-generated wrapper modules), `use Enum::*` inside code
  that is mostly about that enum, `pub use child::*` re-exports in a non-leaf
  module, and the preludes that a crate asks you to use (`pyo3::prelude::*`).
  Write explicit imports everywhere else, and don't define preludes.
- **One module per `use`.** No `::` inside the braces: write
  `use foo::bar::{Bar, Baz};` and `use foo::Foo;`, not `use foo::{Foo, bar::Bar};`.
  `use std::{fs, io};` is fine; it imports two items of one module.
- **Import traits where they are used.** Put a trait `use` inside the function
  that calls its methods, unless the whole file uses it.
- **Call free functions through their module**: `use crate::contract;` then
  `contract::callable_refs(items)`. A function of the same module, or one whose
  name is already unambiguous across the codebase because it comes from
  generated or foreign bindings, can stay unqualified. Methods, macros, and
  constructors are unaffected.
- **Import types and traits, and use them unqualified**: `Path`, not
  `std::path::Path`. Qualify when a short name is ambiguous (`io::Error`,
  `fmt::Result`, `syn::Error` beside `anyhow::Error`), or rename on import.
  Macro output and generated code always use absolute paths (`::bimetal::...`),
  so that they work wherever they expand, and the import rules don't bind
  them.

## Formatting

- rustfmt with the default 100-column width. The guide's other rustfmt
  options (`hex_literal_case`, `format_macro_matchers`, `imports_granularity`)
  need nightly, so follow them by hand: lowercase hex digits, and the import
  rules above.
- `#[rustfmt::skip]` only for deliberate alignment or generated code, never for
  taste.
- Write invisible characters in literals as escapes (`\u{200D}`).

## Ownership and borrowing

- **Avoid `Rc` and `Arc`** unless unique ownership would be unreasonably
  complex. Cloning an `Arc` is fine; the rule is about choosing shared
  ownership.
- **Borrow instead of cloning.** Clone only when you need a deep copy or when
  the borrows would get truly tangled; consider the cost.

## Function inputs and outputs

- **Take the most flexible borrow**: `&T`, `&[T]`, `&str`, `Option<&T>`, not
  `&Box<T>`, `&Vec<T>`, `&String`, or `&Option<T>`.
- **Returns and parameters in libraries**: prefer returning iterators and
  accepting `impl IntoIterator` when callers only iterate. Return a `Vec` when
  the function builds one anyway, when callers need one, or when it returns an
  existing one. Application code may use `Vec` and slices freely.
- **`impl Into<T>` parameters** only in library APIs where call-site
  ergonomics matter; almost never in application code.
- **Prefer an `enum` to a `bool`** when a call site such as `metadata(path, false)`
  would not say what the flag means. If a `bool` stays, name the argument at
  the call site: `export_options(attrs, /* method */ true)`.
- **Pick the error mechanism by contract**: `Option` when the cause is obvious,
  `Result` when the caller needs to know why, `panic!` for bugs and broken
  invariants. Tests and developer tools may panic.

## Types and traits

- **Newtype instead of a type alias** when the type means something different,
  needs its own API, may change representation, or is easy to mix up.
- **Named fields over tuples.** Avoid `.0`/`.1` on anything but a one-field
  newtype. A tuple that the caller destructures at once into named bindings
  (`let (schema, batches) = value.into_stream();`) is a local context and fine.
  A struct is better when a tuple travels through several functions.
- **Add trait impls when needed** (applications) or when users will clearly
  want them (libraries). Implement standard traits instead of look-alike
  methods (`FromStr`, not `fn from_str`), and derive instead of writing impls
  by hand.
- **Generics over `dyn Trait`** in library parameters, unless code size matters.
- **A trait only** for generic bounds, `impl Trait`/`dyn Trait`, or an extension
  trait (suffix `Ext`, owned by the type's owner). Use inherent impls to
  organize code.
- **Traits to implement**: `Copy` for error types and types that will stay
  `Copy`; `Clone` unless the type is an external resource; `Default` when there
  is a `new()` or a natural default state; `PartialEq`/`PartialOrd` before
  `Eq`/`Ord`, which add guarantees; `Hash` and `Eq` for likely map keys.
- An important `From` impl gets an inherent constructor beside it; an
  all-public "options" struct implements `Default`.

## Concurrency

- **Never reach into a poisoned mutex** (`PoisonError::into_inner`). Hold locks
  only around code that cannot panic, and `expect` the lock with that reason.

## Macros

- Prefer, in order: plain Rust, `macro_rules!`, other code generation, and
  procedural macros. Bimetal is a code generator, so its proc macros are the
  product, but inside them keep logic in ordinary functions and keep the
  emitted code as plain as possible.

## Naming

- Methods follow the standard library: `as_`/`to_`/`into_` with matching
  `self` conventions; getters are `foo()` (not `get_foo()`), plus `foo_mut()`,
  `set_foo()`, `take_foo()`, `replace_foo()`, and `with_foo(|foo| ..)` for
  scoped access.
- Constructors are `new()`, `new_with_foo()` for alternatives, and `try_new()`
  when they return `Result`; conversions are `from_foo()` with a `From` impl.
- Lifetimes get descriptive names, except `'_`, and one or two short ones that
  only relate a method's inputs to its output. Generic parameters are short and
  hint at their main bound.
- **No abbreviations** that an outsider wouldn't know, and no dropped letters
  (`decl`, `msg`, `cfg`). Common ones stay: `ir`, `ffi`, `ipc`, `io`, `json`,
  `wasm`, `py` for pyo3's `Python` token.

## Documentation

- Document every public item (`#![deny(missing_docs)]` in the library crates).
  Write sentences with capitals and full stops.
- Say what the code doesn't. Drop comments that restate a name; don't explain
  the language.
- Add `# Panics` when malformed input can make a public function panic, and
  `# Safety` with the invariants for every `unsafe fn` or trait.
- `//!` at the top of each file for the module, `///` for items, `//` for
  implementation notes and TODOs. Mixing `///` and `//` on one item is fine.
- Examples are welcome where they show how the crate works; not every function
  needs one.

## Other features

- **`unsafe` only when safe code can't do it.** Keep it in small, safe
  wrappers, give every block a `// SAFETY:` comment that argues correctness,
  and justify why a safe version won't do, such as an FFI boundary or a copy
  the hot path cannot afford.
- No unstable features.
- Implement expected API sets together (`len` with `is_empty`).
- Don't use another crate's `#[doc(hidden)]` items.
- Make a public function `const` only if it will stay `const`.
- Overload operators only for their mathematical meaning.
- `#[non_exhaustive]` on public enums, and on structs with public fields, that
  other teams use. See Bimetal's exception below.
- `#[must_use]` on functions whose result the caller must not drop, including
  pure functions, unless the return type is already `#[must_use]` (`Result`);
  and on types that are errors or lazy computations.
- No code before `main()`. See Bimetal's exception below.

## Implementation

- **Spell out types the reader can't infer**: annotate the results of
  `.into()`, `.parse()`, and long iterator chains. Put the type on the binding
  (`let names: Vec<_> = iter.collect();`) rather than a turbofish; keep a
  turbofish where there is no binding, such as an argument or a returned
  expression.
- **Match instead of check-then-unwrap**: `if let Some(value) = option`, not
  `if option.is_some() { option.unwrap() }`.
- **`expect` over `unwrap`** outside tests, with a message that says why the
  value must be there.
- On `#[non_exhaustive]` enums, list the variants you know; put the rest in the
  wildcard arm, or use `if let` when you care about only one.
- **Give assertions context** when the reason isn't obvious, when users may see
  the failure, or when a value helps: say why it should hold, or whether the
  caller or the code is at fault. Don't just restate the condition.
- **Keep iterator chains readable**: no long or stateful chains, no chain in a
  `for` header (bind it to a named variable first), and no `for_each`.
- **No `panic::catch_unwind`** except as defense in depth that nothing relies
  on.
- **`format!`/`write!` to join more than two strings** or to fill a template,
  not `+` chains or `str::replace` on a literal template.

## Bimetal's recorded exceptions

These deviate from the guide on purpose. Keep them, and keep their comments.

- **Code before `main()`.** In native builds, exports register through
  `inventory`, which uses linker constructors. That is how one macro, expanded
  in many crates, adds to one registry without a central list. The Wasm
  module registers nothing, and `bimetal_initialize` runs the constructors that
  dependencies declare once, at instantiation.
- **`catch_unwind` in the discovery ABI** (`bimetal-ir`). A panic must not cross
  the C ABI into the CLI, which would abort the process. The CLI reports it as
  an error instead; nothing relies on it for correctness.
- **`catch_unwind` at Python's async boundary** (`bimetal/src/runtime/executor.rs`
  and `python.rs`).
  pyo3 turns a panic in a synchronous call into `PanicException`, but a panic
  while a Tokio worker polls the future, or while the result converts, would
  otherwise reach Python as an opaque error. Bimetal raises the same
  `PanicException` with the panic's message instead.
- **`catch_unwind` at the Node.js boundary** (`bimetal/src/runtime/node`). Every
  call from Node.js into Rust catches a panic: the callbacks, the module
  initializer, the conversion of an async result, and the finalizers. One
  that unwound into Node.js would abort the process; the addon throws or
  rejects with the panic's message instead, and stays usable, as the Python
  extension does.
- **`catch_unwind` in an exported Arrow stream** (`bimetal/src/convert/python_arrow.rs`).
  Consumers call its `get_next` and `release` from C, where an unwind would
  abort the process, so a panic in the stream or in its drop ends the stream
  with the panic's message instead.
- Every native host catches through `executor::catch`, which drops the caught
  payload in turn, and leaks it only if its own drop panics.
- **Leaked Tokio runtimes after `fork`.** A forked child starts its own
  runtime. Dropping the parent's would wait for worker threads that the child
  does not have, so Bimetal never drops a runtime that it published. For the
  same reason, a forked child leaks the Arrow streams that the parent
  exported instead of dropping their sources.
- **No `#[non_exhaustive]` on the IR.** Generated code in application crates
  builds the IR structs with struct literals, and the CLI matches the enums
  exhaustively at the same version, so a new field or variant is a breaking
  change made, and fixed, in one commit.
- **`unsafe impl Send` for a callback's JavaScript function in the Wasm module**
  (`bimetal/src/convert/wasm.rs`). `Callback` is `Send + Sync` on every host, so
  that a resource that keeps one has one signature, and `wasm32` has one thread.
- **`Arc` in `bimetal::runtime::Resource` and in Arrow values.** Async calls keep
  a resource alive after the host closes it, and arrow-rs shares its buffers
  with `Arc` by design.
- **`unsafe` at FFI boundaries.** The discovery ABI, the Arrow C Data
  Interface, the zero-copy NumPy buffer, and Node-API exist to cross a language
  boundary or to avoid a copy. Node-API's
  calls stay in `bimetal/src/runtime/node/{env,value,error,class,promise,callback}.rs`,
  behind safe handles.

## Reviewing or auditing code

1. Run `mise run lint`, and fix everything it reports.
2. For the rules no lint checks, search the changed files:
   - glob imports: `rg -n '^\s*(pub(\(\w+\))? )?use .*\*;'`, outside inline modules;
   - nested `use` paths: a `::` inside `use` braces;
   - turbofish bindings: `rg -n 'let .*collect::<'`;
   - free-function imports: `use crate::module::{some_function}` then a bare call;
   - `bool` arguments at call sites: `, true)` and `, false)`;
   - `.0`/`.1` on tuples, `.clone()` that a borrow would replace, `+` string
     joins, and comments that only restate a name.
3. Report every deliberate deviation in the change description, citing the
   rule, as the exceptions above do.
