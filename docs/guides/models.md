# Models

A *model* is a type that crosses as data. Derive `Model` on a Serde type, with both `Serialize` and `Deserialize`, so that values cross in both directions with one wire shape. Serde controls the wire names. [`apps/examples/showcase/src/lib.rs`](../../apps/examples/showcase/src/lib.rs) has every kind.

`Model` supports structs with named fields, enums with only unit variants, internally tagged enums with named fields, enums in Serde's default form, and `#[serde(transparent)]` tuple structs with one field.

## Structs

```rust
/// Settings with defaults and a fixed value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Always `"v1"`.
    #[bimetal(literal = "v1")]
    pub version: String,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default)]
    pub prefix: String,
}
```

Python gets a frozen Pydantic model, and a `dict` passed where a model is expected is validated as that model. TypeScript gets a readonly interface, and values are plain frozen objects:

```python
self.assertFalse(echo_settings(Settings(version="v1")).verbose)
```

```ts
assert.equal(echoSettings({ version: "v1" }).verbose, false);
```

`#[serde(default)]` on a `bool`, integer or `String` field makes it optional. The host fills in its Rust `Default`, which must keep the field's rules. An `Option` field is `None` when absent, with or without `#[serde(default)]`.

## Field rules

`#[bimetal(...)]` on a field adds a rule that every host sees:

```rust
#[bimetal(min_length = 1, max_length = 40)]
pub label: String,
pub priority: Priority,
pub score: Score,
#[bimetal(ge = 1, le = 5)]
pub stars: u8,
/// A field that is always present, even though it may be empty.
#[bimetal(required)]
pub note: Option<String>,
```

| Attribute | Rule |
| --- | --- |
| `required` | The field must be present, also when its type is `Option<T>` |
| `literal = <literal>` | The field must have this boolean, integer or string value. It doesn't apply to an `Option` field |
| `min_length`, `max_length` | Limits for the length of a string or a list |
| `ge`, `le` | Limits for an integer field. They don't apply through an alias |

Rust checks `literal`, `min_length`, `max_length`, `ge` and `le` on every argument and result of an export, and on what a callback returns, on every host, including models inside lists and maps. A string's length counts Unicode code points, as Python's `len` does, so an emoji counts once, though JavaScript's `.length` counts two.

Pydantic also checks the rules where a Python model is built. Rust catches what Pydantic doesn't validate, such as a model changed with `model_copy(update=...)`. TypeScript's declarations carry the same names and types: a field with a default is optional, and a `literal` field has its value as its type.

Only Python checks two things at run time: `required`, because Serde reads a missing `Option` field as `None` (TypeScript's declarations make the field required), and unknown fields, which Pydantic rejects and Serde ignores.

The check is one more pass over each decoded value. It skips values whose types have no rules, except inside maps and recursive models, which it still walks.

[Failures](../concepts/failures.md) says what each broken rule raises.

## Enums

An enum with only unit variants becomes a Python `StrEnum` and a TypeScript string union, with a frozen object of the same name:

```rust
/// How urgent a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
```

```python
self.assertEqual(Priority.High, "high")
```

```ts
assert.equal(Priority.High, "high");
```

An internally tagged enum becomes a discriminated union:

```rust
/// Something that happened to a record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    /// The record was created.
    Created { record_id: RecordId },
```

An enum in Serde's default form, without a `tag`, becomes the union of its variants. A variant without data crosses as its name, and a variant with data as an object whose only key is that name. A variant can hold named fields or one value, and a `Box` crosses as the value inside it, so an enum can hold itself:

```rust
/// One step of a drawing, in Serde's default enum form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Stroke {
    /// Lift the pen.
    Lift,
    /// Move the pen to a point.
    MoveTo(Point),
```

In Python each variant is a model, such as `StrokeMoveTo`, which holds a one-value variant's data as `value`, and the union picks the model by the variant's name. TypeScript keeps the wire form: a variant without data is its name's string type, and one with data an interface with that one key:

```python
start = StrokeMoveTo(value=Point(x=1.0, y=2.0))
self.assertEqual(retrace([StrokeLift(), start]), [start, StrokeLift()])
```

```ts
assert.deepEqual(retrace(["lift", { move_to: { x: 1, y: 2 } }]), [{ move_to: { x: 1, y: 2 } }, "lift"]);
```

A tuple variant crosses only with one value, and a field of a struct variant can't have the variant's name. Untagged and adjacently tagged enums don't cross.

## Aliases

A `#[serde(transparent)]` tuple struct with one field becomes an alias of its field's type on each host:

```rust
/// A stable identifier that hosts see as a plain string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(transparent)]
pub struct RecordId(pub String);
```

## What a model can't hold

A model can't hold `bytes`, `buffer`, Arrow values, resources or callbacks. A `Vec<u8>` field is a list of integers, so send large payloads as separate parameters. A `Model` must be a type of your crate, so a type from another crate needs a mirror, as each [trial](large-projects.md) does.
