//! Host functions that Rust calls.

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::ContractType;
use crate::ir::TypeRef;

thread_local! {
    /// How many host functions this thread is running through a [`Callback`].
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Whether this thread is running a host function through a [`Callback`].
///
/// A synchronous resource call must not wait for a lock here. The call that
/// made the callback may hold that lock, and it is waiting for this function
/// to return, so neither would ever finish.
#[doc(hidden)]
#[must_use]
pub fn inside_callback() -> bool {
    DEPTH.get() > 0
}

/// Marks this thread as running a host function, until it drops.
struct Inside;

impl Inside {
    fn enter() -> Self {
        DEPTH.set(DEPTH.get() + 1);
        Self
    }
}

impl Drop for Inside {
    fn drop(&mut self) {
        DEPTH.set(DEPTH.get() - 1);
    }
}

/// A host function, a Python callable or a JavaScript function, that takes
/// the items of the tuple `A` as its arguments and returns `R`.
///
/// Take one as a parameter of an export to let the host pass a function, and
/// keep it for as long as it is needed. Calls can come from any thread. Each
/// call converts its arguments and its result as an export's values cross.
///
/// In Python the call takes the interpreter lock, so it waits for any Python
/// code that holds it. On Node.js, a call from another thread waits for the
/// JavaScript thread, so a synchronous export must not wait for such a call:
/// run the work that makes it in an async export.
pub struct Callback<A, R> {
    function: Arc<dyn Fn(A) -> Result<R, CallbackError> + Send + Sync>,
    signature: PhantomData<fn(A) -> R>,
}

impl<A, R> Callback<A, R> {
    /// Wrap the conversion and call that a host's boundary built.
    #[doc(hidden)]
    pub fn new(function: impl Fn(A) -> Result<R, CallbackError> + Send + Sync + 'static) -> Self {
        Self {
            function: Arc::new(function),
            signature: PhantomData,
        }
    }

    /// Call the host function.
    ///
    /// # Errors
    ///
    /// Returns [`CallbackError`] with the exception's text when the function
    /// raises one, and when its arguments or its result cannot cross.
    pub fn call(&self, arguments: A) -> Result<R, CallbackError> {
        let _inside = Inside::enter();
        (self.function)(arguments)
    }
}

impl<A, R> Clone for Callback<A, R> {
    fn clone(&self) -> Self {
        Self {
            function: Arc::clone(&self.function),
            signature: PhantomData,
        }
    }
}

impl<A, R> fmt::Debug for Callback<A, R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Callback")
    }
}

/// Why a host function did not return a value.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CallbackError {
    message: String,
}

impl CallbackError {
    /// Report that a call failed with the exception's text, `message`.
    #[doc(hidden)]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The argument tuples of a [`Callback`].
#[doc(hidden)]
pub trait CallbackArguments {
    /// Return the contract types of the items.
    fn type_refs() -> Vec<TypeRef>;
}

macro_rules! arguments {
    ($($name:ident),*) => {
        impl<$($name: ContractType),*> CallbackArguments for ($($name,)*) {
            fn type_refs() -> Vec<TypeRef> {
                vec![$($name::type_ref()),*]
            }
        }
    };
}

arguments!();
arguments!(A);
arguments!(A, B);
arguments!(A, B, C);
arguments!(A, B, C, D);
arguments!(A, B, C, D, E);
arguments!(A, B, C, D, E, F);
arguments!(A, B, C, D, E, F, G);
arguments!(A, B, C, D, E, F, G, H);

impl<A: CallbackArguments, R: ContractType> ContractType for Callback<A, R> {
    fn type_ref() -> TypeRef {
        let mut signature = A::type_refs();
        signature.push(R::type_ref());
        TypeRef::Callback { signature }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_callback_describes_its_signature_and_runs() {
        let callback: Callback<(u32, String), bool> =
            Callback::new(|(number, text): (u32, String)| Ok(number as usize == text.len()));

        let TypeRef::Callback { signature } = Callback::<(u32, String), bool>::type_ref() else {
            panic!("a callback's contract type names its signature");
        };
        assert_eq!(
            signature,
            [u32::type_ref(), String::type_ref(), bool::type_ref()]
        );
        assert_eq!(callback.call((2, "no".to_owned())), Ok(true));
        assert_eq!(callback.clone().call((2, "yes".to_owned())), Ok(false));
    }

    #[test]
    fn a_callback_marks_its_thread_while_it_runs() {
        let callback: Callback<(), bool> = Callback::new(|()| Ok(inside_callback()));
        let nested = callback.clone();
        let outer: Callback<(), bool> = Callback::new(move |()| {
            nested.call(())?;
            Ok(inside_callback())
        });

        assert!(!inside_callback());
        assert_eq!(callback.call(()), Ok(true));
        assert_eq!(outer.call(()), Ok(true), "a nested call leaves the mark");
        assert!(!inside_callback());
    }

    #[test]
    fn a_callback_reports_what_the_host_raised() {
        let callback: Callback<(), u8> = Callback::new(|()| Err(CallbackError::new("boom")));

        assert_eq!(
            callback.call(()).expect_err("the host raised").to_string(),
            "boom"
        );
    }
}
