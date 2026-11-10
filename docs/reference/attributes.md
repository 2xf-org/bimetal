# Attributes

Every `#[bimetal(...)]`, by where it goes.

## Derives and exports

| Item | Goes on | Does |
| --- | --- | --- |
| `#[derive(Model)]` | A Serde type | Makes it a [model](../guides/models.md) |
| `#[derive(bimetal::Error)]` | An enum or struct that implements `Display` | Makes it an [error](../guides/errors.md) |
| `#[bimetal::export]` | A public function, `const`, `static`, or one inherent `impl` block | Exports it |

## On a model's field

| Attribute | Does |
| --- | --- |
| `required` | The field must be present, also when its type is `Option<T>` |
| `literal = <literal>` | The field must have this boolean, integer or string value |
| `min_length = <n>`, `max_length = <n>` | Limits a string's or a list's length |
| `ge = <n>`, `le = <n>` | Limits an integer |

## On an error

| Attribute | Goes on | Does |
| --- | --- | --- |
| `details` | A variant, or an error struct | Gives its named fields to the hosts as a [typed model](../guides/errors.md#details) |

## On a function or method

| Attribute | Does |
| --- | --- |
| `returns(bytes)` | Returns `Vec<u8>` or `[u8; N]` as bytes, also in an `Option` |
| `returns(buffer)` | Returns a numeric `Vec<T>` as a NumPy array or typed array, also in an `Option` |
| `returns(arrow)` | Returns an Arrow value, also in an `Option`. Needs the `arrow` or `arrow-59` feature |
| `returns(resource)` | Returns a resource, also in an `Option` or a `Vec` |

## On a parameter

| Attribute | Does |
| --- | --- |
| `bytes` | Takes `&[u8]`, `Vec<u8>` or `[u8; N]` as bytes |
| `buffer` | Takes a numeric slice or `Vec<T>` as a NumPy array or typed array |
| `arrow` | Takes an Arrow value by value or by reference, or a `RecordBatchStream` by value |
| `resource` | Takes a resource as `&T`, `&mut T`, `Option<&T>` or `T` |
| `default = <literal>` | Lets a caller omit it |

## On a resource's method

| Attribute | Does |
| --- | --- |
| `constructor` | Makes it the class constructor, or a factory. [Resources](../guides/resources.md#constructors) says which |
| `skip` | Keeps it in Rust |
| `enter`, `exit` | Implements `with` and `using` |
| `next` | Implements `for`, and `async for` when async |
| `getter`, `setter` | Implements a property |
| `len`, `repr`, `display` | Implements `len()` and `length`, `repr()`, and `str()` and `toString()` |
| `getitem`, `setitem`, `delitem` | Implements `value[key]`, in Python |
| `contains`, `eq`, `hash`, `bool`, `call` | Implements `in`, `==`, `hash()`, `bool()` and `value(...)`, in Python |

[Protocols](../guides/protocols.md) has the signature each one needs.

## Serde attributes that matter

Serde decides what crosses, so its attributes shape the contract:

| Attribute | Effect |
| --- | --- |
| `rename_all`, `rename_all_fields`, `rename` | Change wire names, which is what TypeScript sees and what a Python model reads |
| `tag = "..."` on an enum | Makes an internally tagged enum, a discriminated union. Without it, an enum with a variant that carries data is in Serde's default form |
| `transparent` on a tuple struct | Makes an alias of its field's type |
| `default` on a `bool`, integer or `String` field | Makes it optional |
| `skip` on a field of an error's details | Leaves it out |

Bimetal rejects directional renames, such as `rename(serialize = "...")`, Serde aliases, and `tag` on a struct.
