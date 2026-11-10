//! Value conversion at the Python, Node.js, and WebAssembly boundaries.
//!
//! Ordinary contract values use Serde-shaped conversion. Annotated byte,
//! numeric-buffer, and Arrow boundaries use specialized host representations
//! so large contiguous values avoid object-by-object serialization. Every
//! host's argument errors name a `parameter`, which both JavaScript hosts
//! name alike, and both pass Arrow values as IPC streams (`arrow_ipc`), which
//! `wasm_arrow` and `node::arrow` give their hosts.

#[cfg(all(feature = "python", feature = "arrow-api", not(target_arch = "wasm32")))]
mod arrow_ffi;
#[cfg(all(
    feature = "arrow-api",
    any(
        target_arch = "wasm32",
        all(feature = "node", not(target_arch = "wasm32"))
    )
))]
mod arrow_ipc;
#[cfg(all(feature = "node", not(target_arch = "wasm32")))]
pub mod node;
#[cfg(any(
    target_arch = "wasm32",
    all(any(feature = "python", feature = "node"), not(target_arch = "wasm32"))
))]
pub(crate) mod parameter;
#[cfg(all(feature = "python", not(target_arch = "wasm32")))]
pub mod python;
#[cfg(all(feature = "python", feature = "arrow-api", not(target_arch = "wasm32")))]
mod python_arrow;
#[cfg(target_arch = "wasm32")]
pub mod wasm;
#[cfg(all(target_arch = "wasm32", feature = "arrow-api"))]
mod wasm_arrow;
