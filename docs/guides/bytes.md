# Bytes and buffers

A plain `Vec<T>` becomes a host list, with one host object per element. If the representation matters, annotate a top-level parameter with `bytes` or `buffer`, or the export with `returns(bytes)` or `returns(buffer)`. [How values cross](../concepts/crossing.md) says what this saves.

## Bytes

Bytes become Python `bytes` and a `Uint8Array`:

```rust
/// Reverse bytes. Large payloads use the `bytes` boundary instead of a list.
///
/// A parameter that shares its name with a Python builtin, such as `bytes`,
/// gains a trailing underscore in Python: `bytes_`.
#[bimetal::export]
#[bimetal(returns(bytes))]
#[must_use]
pub fn reverse(#[bimetal(bytes)] bytes: &[u8]) -> Vec<u8> {
```

```python
self.assertEqual(reverse(b"abc"), b"cba")
```

```ts
assert.deepEqual(reverse(new Uint8Array([1, 2, 3])), new Uint8Array([3, 2, 1]));
```

A fixed-size `[u8; N]` also works, and an argument of the wrong length is a `ValueError` in Python and a `TypeError` in TypeScript. Without `bytes`, an array crosses as a tuple of its items, as Serde writes it, just as a `Vec<u8>` crosses as a list of integers. Serde writes and reads arrays of up to 32 items. Python refuses a tuple of the wrong length; TypeScript refuses one that is too short and leaves out extra items.

## Buffers

Buffers become writable NumPy arrays and JavaScript typed arrays of the matching element type, such as `Float64Array` or `BigInt64Array`:

```rust
/// A running total. Buffers become NumPy arrays and typed arrays.
#[bimetal::export]
#[bimetal(returns(buffer))]
#[must_use]
pub fn running_total(#[bimetal(buffer)] values: &[i64]) -> Vec<i64> {
```

```python
self.assertEqual(running_total(np.array([1, 2, 3], dtype=np.int64)).tolist(), [1, 3, 6])
```

```ts
assert.deepEqual(runningTotal(new BigInt64Array([1n, 2n, 3n])), new BigInt64Array([1n, 3n, 6n]));
```

A Python project whose contract uses buffers must depend on `numpy`, which `bimetal build` checks.

Python rejects a NumPy array of another element type, or in the other byte order, such as `dtype=">i8"`, with a `BufferError` that names the dtype and says how to convert the array. A value that isn't a buffer, such as a list, is a `TypeError`. TypeScript accepts only the parameter's own typed array, a `Uint8Array` for bytes, and throws a `TypeError` for anything else, such as a plain array or another typed array.

## Options, in returns

A return can be an `Option` of bytes or a buffer, for a method that may have nothing to give, or for a `next` method:

```rust
/// The first title as bytes, if there is one.
#[bimetal(returns(bytes))]
pub fn first_bytes(&self) -> Option<Vec<u8>> {
```

## In models

Models can't contain `bytes` or `buffer` fields. A `Vec<u8>` field is a list of integers, so send large payloads as separate parameters.
