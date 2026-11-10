//! Panic-safe C ABI that the CLI uses to read a compiled contract.

use std::ffi::{CString, c_char};
use std::panic::{self, AssertUnwindSafe};
use std::{mem, ptr};

use crate::hash;

/// Discovery completed and `payload` contains contract JSON.
pub const SUCCESS: u32 = 0;
/// Discovery failed while constructing or validating the contract.
pub const ERROR: u32 = 1;
/// Discovery panicked, and `payload` contains the panic's message, or is null
/// for a panic without one.
pub const PANIC: u32 = 2;

/// The start of the symbol of the function that returns the contract of a
/// native build, which the name of the link crate's Cargo package ends.
pub const CONTRACT_SYMBOL_PREFIX: &str = "bimetal_discovery_v1_contract__";

/// The start of the symbol of the function that frees what the contract
/// function returned, which the package's name ends in the same way.
pub const FREE_SYMBOL_PREFIX: &str = "bimetal_discovery_v1_contract_free__";

/// The environment variable through which `bimetal build` gives the native
/// build the application's name, instead of its Cargo package's.
pub const APPLICATION_NAME_VARIABLE: &str = "BIMETAL_APPLICATION_NAME";

/// The environment variable through which `bimetal build` gives the native
/// build the application's version, instead of its Cargo package's.
pub const APPLICATION_VERSION_VARIABLE: &str = "BIMETAL_APPLICATION_VERSION";

/// Owned result transferred across the discovery dynamic-library boundary.
///
/// `payload` is either null or allocated by [`CString::into_raw`]. The
/// caller must release a non-null pointer with [`free`].
#[repr(C)]
pub struct DiscoveryResult {
    /// One of [`SUCCESS`], [`ERROR`], or [`PANIC`].
    pub status: u32,
    /// UTF-8 contract JSON or an error message, depending on `status`.
    pub payload: *mut c_char,
}

/// Run contract construction without allowing Rust unwinding across FFI.
pub fn contract(build: impl FnOnce() -> Result<String, String>) -> DiscoveryResult {
    // A panic must not unwind into the CLI through the C ABI, which would abort
    // the process: report it instead.
    match panic::catch_unwind(AssertUnwindSafe(build)) {
        Ok(Ok(payload)) => owned(SUCCESS, &payload),
        Ok(Err(error)) => owned(ERROR, &error),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            let result = match message {
                Some(message) => owned(PANIC, message),
                None => DiscoveryResult {
                    status: PANIC,
                    payload: ptr::null_mut(),
                },
            };
            // Dropping a panic payload runs its destructor, which could panic
            // again, so leak it.
            mem::forget(payload);
            result
        }
    }
}

/// The attribute of a native host module that holds the hash of the contract
/// it was built from; see [`contract_hash`].
pub const CONTRACT_HASH_ATTRIBUTE: &str = "__bimetal_contract__";

/// Return the hash that identifies a contract: 64-bit FNV-1a over the
/// contract JSON that discovery returns, in lower-case hexadecimal.
///
/// The CLI records it in the generated Python package and in a TypeScript
/// package that loads the Node.js addon, and the extension and the addon
/// compute it from their own contract, so the package can tell at import
/// time whether its native code came from the same Rust API.
#[must_use]
pub fn contract_hash(contract_json: &[u8]) -> String {
    format!("{:016x}", hash::fnv1a(contract_json))
}

/// Transfer a message to the caller as an owned C string.
fn owned(status: u32, payload: &str) -> DiscoveryResult {
    let payload = CString::new(payload.replace('\0', "\\0"))
        .expect("escaping removes every interior NUL byte");
    DiscoveryResult {
        status,
        payload: payload.into_raw(),
    }
}

/// Release a payload that [`contract`] returned.
///
/// # Safety
///
/// `pointer` must be null or a live pointer returned by [`contract`].
pub unsafe fn free(pointer: *mut c_char) {
    if !pointer.is_null() {
        // SAFETY: the caller follows this function's contract.
        drop(unsafe { CString::from_raw(pointer) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_hashes_are_in_hexadecimal() {
        assert_eq!(contract_hash(b""), "cbf29ce484222325");
    }

    #[test]
    fn a_panic_returns_its_message() {
        use std::ffi::CStr;

        let result = contract(|| panic!("the contract panics"));
        assert_eq!(result.status, PANIC);
        assert!(
            !result.payload.is_null(),
            "a panic with a message returns it"
        );
        // SAFETY: `contract` returned a live NUL-terminated string.
        let message = unsafe { CStr::from_ptr(result.payload) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: the payload came from `contract`, and is freed once.
        unsafe { free(result.payload) };
        assert_eq!(message, "the contract panics");
    }
}
