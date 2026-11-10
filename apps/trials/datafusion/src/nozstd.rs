//! Stand-ins for the zstd C functions in a browser.
//!
//! DataFusion turns on arrow-ipc's default features, which link zstd, a C
//! library that does not compile to Wasm without a C toolchain for it. Only IPC
//! compression calls it, and queries in memory never do, so each function here
//! stops the module with a message instead.

#![cfg(target_arch = "wasm32")]

use std::ffi::{c_char, c_int, c_uint, c_void};

/// Define each function, with the signature that zstd-sys declares.
macro_rules! absent {
    ($($name:ident($($parameter:ident: $ty:ty),*) -> $returns:ty;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub extern "C" fn $name($($parameter: $ty),*) -> $returns {
                $(let _ = $parameter;)*
                unreachable!("zstd is not available in the WebAssembly module")
            }
        )*
    };
}

absent! {
    ZSTD_createCCtx() -> *mut c_void;
    ZSTD_createDCtx() -> *mut c_void;
    ZSTD_freeCCtx(context: *mut c_void) -> usize;
    ZSTD_freeDCtx(context: *mut c_void) -> usize;
    ZSTD_isError(result: usize) -> c_uint;
    ZSTD_getErrorName(result: usize) -> *const c_char;
    ZSTD_compressBound(size: usize) -> usize;
    ZSTD_CCtx_setParameter(context: *mut c_void, parameter: c_uint, value: c_int) -> usize;
    ZSTD_CCtx_loadDictionary(context: *mut c_void, dictionary: *const c_void, size: usize) -> usize;
    ZSTD_DCtx_loadDictionary(context: *mut c_void, dictionary: *const c_void, size: usize) -> usize;
    ZSTD_DCtx_reset(context: *mut c_void, directive: c_uint) -> usize;
    ZSTD_compress2(
        context: *mut c_void,
        destination: *mut c_void,
        capacity: usize,
        source: *const c_void,
        size: usize
    ) -> usize;
    ZSTD_decompressDCtx(
        context: *mut c_void,
        destination: *mut c_void,
        capacity: usize,
        source: *const c_void,
        size: usize
    ) -> usize;
}
