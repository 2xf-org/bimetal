# Types

Every Rust type that can cross, and what each host sees.

| Rust | Python | TypeScript |
| --- | --- | --- |
| `()` | `None` | `void` |
| `bool` | `bool` | `boolean` |
| `u8` to `u32`, `i8` to `i32` | `int` | `number` |
| `u64`, `i64`, `usize`, `isize` | `int` | `bigint` |
| `f32`, `f64` | `float` | `number` |
| `String`, `&str` | `str` | `string` |
| `Option<T>` | `T \| None` | `T \| null` |
| `Vec<T>`, `&[T]` | `list[T]` | `ReadonlyArray<T>` |
| `HashMap<String, T>`, `BTreeMap<String, T>` | `dict[str, T]` | `{ readonly [key: string]: T }` |
| Tuples of 2 to 8 items | `tuple[...]` | Readonly tuple |
| `[T; N]` other than bytes, up to 32 items | `tuple[...]` of N items | Readonly tuple |
| `chrono::DateTime<Utc>` or `<FixedOffset>` | `datetime` with a time zone | ISO 8601 `string` |
| `chrono::NaiveDate`, in years 1 to 9999 | `date` | `YYYY-MM-DD` `string` |
| `std::time::Duration` | `int` nanoseconds | `bigint` nanoseconds |
| `PathBuf`, `&Path` | `str` | `string` |
| alloy's `Address`, `FixedBytes<N>` such as `B256`, `Uint` such as `U256`, `Signed` such as `I256`, and `Bytes`, with Bimetal's `alloy` feature and alloy's `serde` feature | `str`, as alloy's Serde writes it | `string` |
| `serde_json::Value` | `Any` | JSON value, where wide integers use `bigint` |
| A `Model` type | Generated model | Generated interface, union or alias |
| `&[u8]`, `Vec<u8>`, `[u8; N]`, as a parameter or a return value, also in an `Option` there | `bytes` | `Uint8Array` |
| A numeric `&[T]` or `Vec<T>`, with `buffer` | `numpy.ndarray` | The matching typed array |
| `ArrayRef`, `RecordBatch`, `Table`, `Schema`, with `arrow` | [Arrow](../guides/arrow.md) | [Arrow](../guides/arrow.md) |
| `RecordBatchStream`, with `arrow` | [`RecordBatchReader`](../guides/arrow.md#streams) | [`RecordBatchStream`](../guides/arrow.md#streams-in-typescript) |
| A resource | Its class | Its class |
| `bimetal::Callback<(A, B), R>` | `Callable[[A, B], R]` | `(arg0: A, arg1: B) => R` |

## Rules at the edges

Python rejects a `datetime` without a time zone, also inside a `serde_json::Value`, because Rust can't tell which instant it means. A `dict` passed where Python expects a model is validated as that model, and a `str` passed for a list or a tuple is a `TypeError`, not a list of characters. JSON has no NaN or infinity, so inside a `serde_json::Value` they become `null`, and a constant that holds one fails `bimetal build`. A `Duration` is a non-negative `int` of nanoseconds, so a `timedelta` or a `bool` is refused. Python also reads Serde's `{"secs": ..., "nanos": ...}`, and a model dumps a duration in that form, which is what crosses.

A date outside years 1 to 9999 fails Python's `date` on its way back, so a sentinel such as `NaiveDate::MAX` can't cross. Pass a date parameter a `date`, not a `datetime`: type checkers accept a `datetime`, which is a `date` in Python, but Pydantic refuses one that has a time of day. alloy's values come back as alloy writes them, which may not be as they were sent: an address in lower case, without its checksum, and a `Uint` as hex without leading zeros, though it also reads decimal text.

TypeScript passes every finite number where Rust expects a float. Inside a `serde_json::Value`, a number that isn't a safe integer, such as `2 ** 53` or `1e300`, crosses as a float, so pass a `bigint` for a wider integer that must stay exact. Models and maps cross as plain objects, and a `Map` or a `Set` where one is expected is a `TypeError`. Returned maps arrive as plain objects, where a `__proto__` key stays data, and returned values are frozen. A `Duration` is a `bigint` of nanoseconds, or a `number` that is a safe integer.

## Rejected at build time

- Directional Serde renames, such as `rename(serialize = "...")`.
- Serde aliases, and `#[serde(tag = "...")]` on a struct.
- `Option` types that can't round-trip, such as `Option<Option<T>>`.
- Generic exports.
- 64-bit integers and durations in an error's details, when TypeScript is on.
- A `RecordBatchStream` parameter of a synchronous call, when TypeScript is on.

[Limits and requirements](limits.md#what-bimetal-doesnt-do-yet) lists what isn't supported yet.
