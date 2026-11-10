//! [`Error`], the JavaScript exception that a call into Rust throws or that
//! its promise rejects with, and the check of every Node-API call's status.

use std::ffi::{CStr, CString};
use std::{fmt, ptr};

use bimetal_ir::typescript_names;
use napi_sys as sys;

/// Return `Ok` when a Node-API call succeeded. Otherwise make sure that an
/// exception is pending that says why, and return [`Error::Pending`].
pub(super) fn check(env: sys::napi_env, status: sys::napi_status) -> Result<(), Error> {
    if status == sys::Status::napi_ok {
        Ok(())
    } else {
        Err(fail(env, status))
    }
}

/// Throw an `Error` for the failed Node-API call that returned `status`,
/// unless the call left an exception pending.
#[cold]
fn fail(env: sys::napi_env, status: sys::napi_status) -> Error {
    let mut info = ptr::null();
    // SAFETY: the call that failed ran on this environment's thread just
    // before, and `info` is a local; Node-API keeps the record until the next
    // call.
    let reason = if unsafe { sys::napi_get_last_error_info(env, &mut info) } == sys::Status::napi_ok
        && !info.is_null()
        // SAFETY: Node-API returned a live record.
        && !unsafe { (*info).error_message }.is_null()
    {
        // SAFETY: the record's message is a static NUL-terminated string.
        unsafe { CStr::from_ptr((*info).error_message) }
            .to_string_lossy()
            .into_owned()
    } else {
        format!("status {status}")
    };
    let mut pending = false;
    // SAFETY: as above, with `pending` a local.
    unsafe { sys::napi_is_exception_pending(env, &mut pending) };
    if !pending {
        let message = CString::new(format!("a Node-API call failed: {reason}"))
            .unwrap_or_else(|_| c"a Node-API call failed".to_owned());
        // SAFETY: as above; `message` outlives the call, which copies it.
        unsafe { sys::napi_throw_error(env, ptr::null(), message.as_ptr()) };
    }
    Error::Pending
}

/// A JavaScript exception that a call into Rust throws, or that its promise
/// rejects with. The generated JavaScript tells them apart as it does the
/// Wasm module's.
#[must_use]
#[derive(Debug)]
pub enum Error {
    /// The exception that is pending in the environment, which a Node-API
    /// call or JavaScript code that Rust ran raised.
    Pending,
    /// A `TypeError`: an argument has the wrong type.
    Type(String),
    /// A `RangeError`: an argument breaks a field rule.
    Range(String),
    /// An `Error` with this message: the call failed.
    Message(String),
    /// A typed contract error, thrown as `{bimetalErrorCode, message}`
    /// ([`typescript_names::ERROR_CODE_PROPERTY`]), which the generated
    /// wrapper turns into the error's class.
    Contract {
        /// The error's code.
        code: &'static str,
        /// The error's message.
        message: String,
        /// The error's fields as JSON, when it declares them.
        details: Option<String>,
    },
}

impl Error {
    /// The `Error` for a Rust panic with `message`.
    #[cold]
    pub(crate) fn panic(message: String) -> Self {
        Self::Message(format!("{}{message}", typescript_names::PANIC_PREFIX))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => formatter.write_str("a JavaScript exception is pending"),
            Self::Type(message)
            | Self::Range(message)
            | Self::Message(message)
            | Self::Contract { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}
