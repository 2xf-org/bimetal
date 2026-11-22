//! The handles of one Node.js environment: [`Env`], and the calls into Rust
//! that it makes.
//!
//! Node.js calls Rust on the environment's JavaScript thread, inside a handle
//! scope that it opens for the call, including the calls of thread-safe
//! functions. An [`Env`] and each [`Value`] carry that scope's lifetime and
//! never leave the thread, so safe code cannot keep a handle that Node.js
//! has released.

use std::array;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::fmt::Write as _;
use std::marker::PhantomData;
use std::sync::OnceLock;
use std::{mem, ptr};

use bimetal_ir::typescript_names;
use napi_sys as sys;

use super::courier::Station;
use super::error::Error;
use super::value::{Element, Value};
use crate::runtime::executor;

/// Call the Node-API function `$function` with the environment `$env`, an
/// [`Env`], and the remaining arguments, and check its status.
///
/// With `-> out`, the function also takes a pointer to a zeroed local last,
/// and the call returns what Node-API wrote there; with `-> value`, it
/// returns that handle as a [`Value`].
macro_rules! napi {
    ($env:expr, $function:ident($($argument:expr),* $(,)?)) => {{
        let env: $crate::runtime::node::Env<'_> = $env;
        $crate::runtime::node::error::check(
            env.raw,
            // SAFETY: an `Env` exists only on its JavaScript thread while
            // Node.js keeps it live, and every other argument is a handle of
            // its current scope, a flag, a callback, or a pointer that stays
            // valid for as long as the function uses it.
            unsafe { ::napi_sys::$function(env.raw, $($argument),*) },
        )
    }};
    ($env:expr, $function:ident($($argument:expr),* $(,)?) -> out) => {{
        let mut out = <_ as $crate::runtime::node::env::Out>::NONE;
        napi!($env, $function($($argument,)* &mut out)).map(|()| out)
    }};
    ($env:expr, $function:ident($($argument:expr),* $(,)?) -> value) => {{
        let env: $crate::runtime::node::Env<'_> = $env;
        napi!(env, $function($($argument),*) -> out).map(|raw| env.value(raw))
    }};
}
pub(super) use napi;

/// What a Node-API function writes through its last pointer, for the
/// `-> out` of [`napi!`].
pub(super) trait Out: Copy {
    /// The zero or NULL that the local holds before the call.
    const NONE: Self;
}

impl Out for bool {
    const NONE: Self = false;
}

impl Out for f64 {
    const NONE: Self = 0.0;
}

impl Out for i32 {
    const NONE: Self = 0;
}

impl Out for u32 {
    const NONE: Self = 0;
}

impl Out for usize {
    const NONE: Self = 0;
}

impl<T> Out for *mut T {
    const NONE: Self = ptr::null_mut();
}

/// Load the Node-API functions from the running process, once per process,
/// and return whether they loaded. napi-sys panics when it cannot find them,
/// which leaves no Node-API function to throw an exception with.
pub(super) fn load_functions() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        executor::catch(|| {
            // SAFETY: Node.js and Electron export the Node-API functions from
            // the process that loads the addon; no other thread calls them
            // before this, the first thing that the addon's module
            // initializer does.
            let library = unsafe { sys::setup() };
            // The functions stay loaded while the process runs.
            mem::forget(library);
        })
        .is_ok()
    })
}

/// Drop `value` without letting a panic in its destructor unwind into
/// Node.js.
pub(super) fn discard<T>(value: T) {
    let _ = executor::catch(|| drop(value));
}

/// One Node.js environment, during one call that Node.js makes into Rust.
#[derive(Clone, Copy)]
pub struct Env<'env> {
    pub(super) raw: sys::napi_env,
    scope: PhantomData<&'env ()>,
}

/// The values that conversions use in every call, which one environment
/// keeps from its module initializer on, and each class's constructor.
pub(crate) struct State {
    /// `Map`, `Map.prototype.set`, and `Symbol`.
    map: sys::napi_ref,
    map_set: sys::napi_ref,
    symbol: sys::napi_ref,
    /// The functions that make a frozen array of their arguments, and a
    /// frozen object of their arguments taken as keys and values in turn.
    array_of: sys::napi_ref,
    map_of: sys::napi_ref,
    /// For each list of keys, the function that makes a frozen object with
    /// those keys, in order, from as many arguments, compiled at its first use.
    objects: RefCell<HashMap<Box<[&'static str]>, sys::napi_ref>>,
    /// The constructor of each class.
    pub(super) classes: Vec<DefinedClass>,
    /// The thread-safe function that carries jobs from other threads here.
    pub(super) station: Station,
}

impl State {
    /// Return whether `value` is a `Map`.
    pub(crate) fn is_map<'env>(&self, env: Env<'env>, value: Value<'env>) -> Result<bool, Error> {
        value.instance_of(env.reference(self.map)?)
    }

    /// Return `Symbol.iterator`.
    pub(crate) fn iterator_symbol<'env>(&self, env: Env<'env>) -> Result<Value<'env>, Error> {
        env.reference(self.symbol)?.named(c"iterator")
    }
}

/// A class that the module initializer defined in one environment.
pub(super) struct DefinedClass {
    /// The address of the class's descriptor, which identifies the class.
    pub(super) descriptor: usize,
    /// The class's constructor.
    pub(super) constructor: sys::napi_ref,
}

/// A function that Node.js calls through Bimetal: an exported function, or a
/// resource's constructor, factory, or method.
pub type Callback = for<'call, 'env> fn(&'call CallInfo<'call, 'env>) -> Result<Value<'env>, Error>;

/// The arguments, the receiver, and the data of one call into Rust.
pub struct CallInfo<'call, 'env> {
    env: Env<'env>,
    info: sys::napi_callback_info,
    this: sys::napi_value,
    arguments: &'call [sys::napi_value],
}

/// Return the Node-API callback for a free function or factory of `arity`
/// arguments, unpacking exact arguments and data in one Node-API call.
pub(super) const fn function_trampoline(
    arity: usize,
) -> unsafe extern "C" fn(sys::napi_env, sys::napi_callback_info) -> sys::napi_value {
    trampoline_for::<false>(arity)
}

/// Return the Node-API callback for a resource method of `arity` arguments,
/// unpacking `this`, exact arguments, and data in one Node-API call.
pub(super) const fn method_trampoline(
    arity: usize,
) -> unsafe extern "C" fn(sys::napi_env, sys::napi_callback_info) -> sys::napi_value {
    trampoline_for::<true>(arity)
}

const fn trampoline_for<const WITH_THIS: bool>(
    arity: usize,
) -> unsafe extern "C" fn(sys::napi_env, sys::napi_callback_info) -> sys::napi_value {
    match arity {
        0 => trampoline::<0, WITH_THIS>,
        1 => trampoline::<1, WITH_THIS>,
        2 => trampoline::<2, WITH_THIS>,
        3 => trampoline::<3, WITH_THIS>,
        4 => trampoline::<4, WITH_THIS>,
        5 => trampoline::<5, WITH_THIS>,
        6 => trampoline::<6, WITH_THIS>,
        7 => trampoline::<7, WITH_THIS>,
        8 => trampoline::<8, WITH_THIS>,
        _ => trampoline_many::<WITH_THIS>,
    }
}

/// The Node-API callback for [`Callback`]s with `ARGS` fixed arguments,
/// unpacking `data`, optional `this`, and exact `ARGS` in one call.
unsafe extern "C" fn trampoline<const ARGS: usize, const WITH_THIS: bool>(
    env: sys::napi_env,
    info: sys::napi_callback_info,
) -> sys::napi_value {
    // SAFETY: Node.js calls a callback on the environment's thread, with a
    // scope open until it returns.
    let env = unsafe { Env::new(env) };
    env.run(|| {
        let mut count = ARGS;
        let mut arguments = [ptr::null_mut(); ARGS];
        let mut this = ptr::null_mut();
        let mut data = ptr::null_mut();
        let (argc_ptr, argv_ptr) = if ARGS == 0 {
            (ptr::null_mut(), ptr::null_mut())
        } else {
            (&raw mut count, arguments.as_mut_ptr())
        };
        let this_ptr = if WITH_THIS {
            &raw mut this
        } else {
            ptr::null_mut()
        };
        napi!(
            env,
            napi_get_cb_info(info, argc_ptr, argv_ptr, this_ptr, &mut data)
        )?;
        let call = CallInfo {
            env,
            info,
            this,
            arguments: &arguments,
        };
        // SAFETY: Bimetal defines every function and method with a pointer to
        // a `Callback` in a static as its data.
        let callback = unsafe { *data.cast::<Callback>() };
        callback(&call)
    })
}

/// Fallback trampoline for exports with more than 8 arguments; `CallInfo::arguments`
/// reads the argument list directly.
unsafe extern "C" fn trampoline_many<const WITH_THIS: bool>(
    env: sys::napi_env,
    info: sys::napi_callback_info,
) -> sys::napi_value {
    // SAFETY: Node.js calls a callback on the environment's thread, with a
    // scope open until it returns.
    let env = unsafe { Env::new(env) };
    env.run(|| {
        let mut this = ptr::null_mut();
        let mut data = ptr::null_mut();
        let this_ptr = if WITH_THIS {
            &raw mut this
        } else {
            ptr::null_mut()
        };
        napi!(
            env,
            napi_get_cb_info(info, ptr::null_mut(), ptr::null_mut(), this_ptr, &mut data)
        )?;
        let call = CallInfo {
            env,
            info,
            this,
            arguments: &[],
        };
        // SAFETY: Bimetal defines every function and method with a pointer to
        // a `Callback` in a static as its data.
        let callback = unsafe { *data.cast::<Callback>() };
        callback(&call)
    })
}

impl<'call, 'env> CallInfo<'call, 'env> {
    /// Unpack only the receiver `this` of `info` (used by class constructors,
    /// whose argument count varies).
    pub(super) fn unpack_receiver(
        env: Env<'env>,
        info: sys::napi_callback_info,
    ) -> Result<CallInfo<'static, 'env>, Error> {
        let mut this = ptr::null_mut();
        napi!(
            env,
            napi_get_cb_info(
                info,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut this,
                ptr::null_mut()
            )
        )?;
        Ok(CallInfo {
            env,
            info,
            this,
            arguments: &[],
        })
    }

    /// The environment that makes the call.
    #[must_use]
    pub fn env(&self) -> Env<'env> {
        self.env
    }

    /// The first `N` arguments, with `undefined` for each that the caller
    /// left out.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Pending`] when Node-API cannot read them.
    pub fn arguments<const N: usize>(&self) -> Result<[Value<'env>; N], Error> {
        if N == 0 {
            return Ok(array::from_fn(|_| self.env.undefined()));
        }
        if self.arguments.len() == N {
            return Ok(array::from_fn(|index| {
                let raw = self.arguments[index];
                if raw.is_null() {
                    self.env.undefined()
                } else {
                    self.env.value(raw)
                }
            }));
        }
        let mut count = N;
        let mut raw = [ptr::null_mut(); N];
        napi!(
            self.env,
            napi_get_cb_info(
                self.info,
                &mut count,
                raw.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut()
            )
        )?;
        Ok(raw.map(|raw| {
            if raw.is_null() {
                self.env.undefined()
            } else {
                self.env.value(raw)
            }
        }))
    }

    /// The call's receiver, `this`.
    pub(super) fn receiver(&self) -> Result<Value<'env>, Error> {
        if !self.this.is_null() {
            return Ok(self.env.value(self.this));
        }
        let mut this = ptr::null_mut();
        napi!(
            self.env,
            napi_get_cb_info(
                self.info,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut this,
                ptr::null_mut()
            )
        )?;
        Ok(self.env.value(this))
    }
}

impl<'env> Env<'env> {
    /// Wrap a live environment.
    ///
    /// # Safety
    ///
    /// `raw` must be an environment that Node.js keeps live, on this thread,
    /// for all of `'env`, inside a handle scope that lasts as long.
    pub(super) unsafe fn new(raw: sys::napi_env) -> Self {
        Self {
            raw,
            scope: PhantomData,
        }
    }

    /// Return a handle of this environment's scope.
    pub(super) fn value(self, raw: sys::napi_value) -> Value<'env> {
        Value { env: self, raw }
    }

    /// Run `call` as the body of a call from Node.js, and return its value,
    /// or `NULL` after throwing its error or its panic. An exception that is
    /// pending already stays, and nothing is thrown over it.
    pub(super) fn run(self, call: impl FnOnce() -> Result<Value<'env>, Error>) -> sys::napi_value {
        match executor::catch_error(call, Error::panic) {
            Ok(value) if !self.pending() => value.raw,
            Ok(_) => ptr::null_mut(),
            Err(error) => {
                self.throw(error);
                ptr::null_mut()
            }
        }
    }

    /// Throw `error`, unless an exception is pending already.
    pub(super) fn throw(self, error: Error) {
        if self.pending() {
            return;
        }
        if let Ok(exception) = self.exception(error) {
            // A failure leaves its own exception pending.
            let _ = napi!(self, napi_throw(exception.raw));
        }
    }

    /// Return the JavaScript value of `error`, taking the pending exception
    /// for [`Error::Pending`].
    pub(super) fn exception(self, error: Error) -> Result<Value<'env>, Error> {
        match error {
            Error::Pending => napi!(self, napi_get_and_clear_last_exception() -> value),
            Error::Type(message) => {
                let message = self.string(&message)?;
                napi!(self, napi_create_type_error(ptr::null_mut(), message.raw) -> value)
            }
            Error::Range(message) => {
                let message = self.string(&message)?;
                napi!(self, napi_create_range_error(ptr::null_mut(), message.raw) -> value)
            }
            Error::Message(message) => {
                let message = self.string(&message)?;
                napi!(self, napi_create_error(ptr::null_mut(), message.raw) -> value)
            }
            Error::Contract {
                code,
                message,
                details,
            } => {
                let payload = self.object()?;
                payload.set_property(
                    self.string(typescript_names::ERROR_CODE_PROPERTY)?,
                    self.string(code)?,
                )?;
                payload.set_named(c"message", self.string(&message)?)?;
                if let Some(details) = details {
                    payload.set_property(
                        self.string(typescript_names::ERROR_DETAILS_PROPERTY)?,
                        self.string(&details)?,
                    )?;
                }
                Ok(payload)
            }
        }
    }

    /// Take the pending exception, if one is pending.
    pub(crate) fn take_exception(self) -> Option<Value<'env>> {
        if !self.pending() {
            return None;
        }
        self.exception(Error::Pending).ok()
    }

    /// Return whether an exception is pending.
    pub(crate) fn pending(self) -> bool {
        napi!(self, napi_is_exception_pending() -> out).is_ok_and(|pending| pending)
    }

    /// Return `undefined`.
    ///
    /// # Panics
    ///
    /// Never: Node-API returns `undefined` for any live environment.
    #[must_use]
    pub fn undefined(self) -> Value<'env> {
        napi!(self, napi_get_undefined() -> value)
            .expect("Node-API returns undefined for a live environment")
    }

    pub(crate) fn null(self) -> Result<Value<'env>, Error> {
        napi!(self, napi_get_null() -> value)
    }

    pub(crate) fn global(self) -> Result<Value<'env>, Error> {
        napi!(self, napi_get_global() -> value)
    }

    /// Return a JavaScript boolean.
    ///
    /// # Errors
    ///
    /// Returns the error of Node-API, which is pending.
    pub fn boolean(self, value: bool) -> Result<Value<'env>, Error> {
        napi!(self, napi_get_boolean(value) -> value)
    }

    pub(crate) fn number(self, value: f64) -> Result<Value<'env>, Error> {
        napi!(self, napi_create_double(value) -> value)
    }

    pub(crate) fn int32(self, value: i32) -> Result<Value<'env>, Error> {
        napi!(self, napi_create_int32(value) -> value)
    }

    pub(crate) fn uint32(self, value: u32) -> Result<Value<'env>, Error> {
        napi!(self, napi_create_uint32(value) -> value)
    }

    pub(crate) fn string(self, value: &str) -> Result<Value<'env>, Error> {
        napi!(
            self,
            napi_create_string_utf8(value.as_ptr().cast::<c_char>(), length(value)) -> value
        )
    }

    pub(crate) fn bigint_i64(self, value: i64) -> Result<Value<'env>, Error> {
        napi!(self, napi_create_bigint_int64(value) -> value)
    }

    pub(crate) fn bigint_u64(self, value: u64) -> Result<Value<'env>, Error> {
        napi!(self, napi_create_bigint_uint64(value) -> value)
    }

    /// Return the bigint with this sign and these 64-bit words, the least
    /// significant first.
    pub(crate) fn bigint_words(self, negative: bool, words: &[u64]) -> Result<Value<'env>, Error> {
        napi!(
            self,
            napi_create_bigint_words(c_int::from(negative), words.len(), words.as_ptr()) -> value
        )
    }

    /// Return a new plain object.
    pub(crate) fn object(self) -> Result<Value<'env>, Error> {
        napi!(self, napi_create_object() -> value)
    }

    pub(crate) fn array(self) -> Result<Value<'env>, Error> {
        napi!(self, napi_create_array() -> value)
    }

    pub(crate) fn array_with_length(self, length: usize) -> Result<Value<'env>, Error> {
        napi!(self, napi_create_array_with_length(length) -> value)
    }

    /// Return a new `Map`, and its `set` method.
    pub(crate) fn map(self) -> Result<(Value<'env>, Value<'env>), Error> {
        let state = self.state()?;
        let map = self.reference(state.map)?.construct([])?;
        Ok((map, self.reference(state.map_set)?))
    }

    /// Return the function that makes a frozen array of its arguments.
    pub(crate) fn array_of(self) -> Result<Value<'env>, Error> {
        self.reference(self.state()?.array_of)
    }

    /// Return the function that makes a frozen object of its arguments, taken
    /// as keys and values in turn. `Object.fromEntries` defines an own
    /// property for every key, also `__proto__`, which assigning would treat
    /// as the prototype.
    pub(crate) fn map_of(self) -> Result<Value<'env>, Error> {
        self.reference(self.state()?.map_of)
    }

    /// Return the function that makes a frozen object with `keys`, in order,
    /// from as many arguments. A literal of fixed keys gives every object one
    /// shape, which costs less than adding its properties one at a time.
    pub(crate) fn object_of(self, keys: &[&'static str]) -> Result<Value<'env>, Error> {
        let state = self.state()?;
        if let Some(&function) = state.objects.borrow().get(keys) {
            return self.reference(function);
        }
        let mut source = String::from("(function (");
        for index in 0..keys.len() {
            let _ = write!(source, "{}value{index}", if index == 0 { "" } else { ", " });
        }
        source.push_str(") { return Object.freeze({");
        for (index, key) in keys.iter().enumerate() {
            let quoted =
                serde_json::to_string(key).map_err(|error| Error::Message(error.to_string()))?;
            // A literal's `__proto__` key sets the prototype, and a computed
            // one defines the property.
            let key = if *key == "__proto__" {
                format!("[{quoted}]")
            } else {
                quoted
            };
            let _ = write!(
                source,
                "{}{key}: value{index}",
                if index == 0 { " " } else { ", " }
            );
        }
        source.push_str(" }); })");
        let function = self.compile(&source)?;
        state
            .objects
            .borrow_mut()
            .insert(keys.into(), self.keep(function)?);
        Ok(function)
    }

    /// Compile the script `source`, which this module writes, and return its
    /// value.
    fn compile(self, source: &str) -> Result<Value<'env>, Error> {
        let source = self.string(source)?;
        napi!(self, napi_run_script(source.raw) -> value)
    }

    /// Return a new plain typed array of `T`, never a Node.js `Buffer`, with
    /// a copy of `values`.
    pub(crate) fn typed_array<T: Element>(self, values: &[T]) -> Result<Value<'env>, Error> {
        let bytes = mem::size_of_val(values);
        let mut data = ptr::null_mut();
        let mut buffer = ptr::null_mut();
        napi!(self, napi_create_arraybuffer(bytes, &mut data, &mut buffer))?;
        if bytes > 0 {
            // SAFETY: Node-API allocated `bytes` writable bytes at `data` for
            // the new buffer, which nothing else references yet, and every
            // `Element` is a numeric primitive whose bytes are all
            // initialized.
            unsafe {
                ptr::copy_nonoverlapping(values.as_ptr().cast::<u8>(), data.cast::<u8>(), bytes);
            }
        }
        napi!(
            self,
            napi_create_typedarray(T::TYPE.0, values.len(), buffer, 0) -> value
        )
    }

    /// Return the value that `reference` holds.
    pub(super) fn reference(self, reference: sys::napi_ref) -> Result<Value<'env>, Error> {
        napi!(self, napi_get_reference_value(reference) -> value)
    }

    /// Keep `value` for as long as the environment lives.
    pub(super) fn keep(self, value: Value<'env>) -> Result<sys::napi_ref, Error> {
        napi!(self, napi_create_reference(value.raw, 1) -> out)
    }

    /// Start the state of this environment, which holds `classes`, with the
    /// values that conversions use.
    pub(super) fn start(self, classes: Vec<DefinedClass>) -> Result<(), Error> {
        let mut kept: Vec<sys::napi_ref> = Vec::with_capacity(5);
        let init = (|| {
            let global = self.global()?;
            let map = global.named(c"Map")?;
            let map_set = map.named(c"prototype")?.named(c"set")?;
            let symbol = global.named(c"Symbol")?;
            let array_of =
                self.compile("(function (...values) { return Object.freeze(values); })")?;
            let map_of = self.compile(
                "(function (...entries) {\n\
                 \x20 const object = {};\n\
                 \x20 for (let index = 0; index < entries.length; index += 2) {\n\
                 \x20   const key = entries[index];\n\
                 \x20   if (key === \"__proto__\") {\n\
                 \x20     Object.defineProperty(object, key, { value: entries[index + 1], writable: true, enumerable: true, configurable: true });\n\
                 \x20   } else {\n\
                 \x20     object[key] = entries[index + 1];\n\
                 \x20   }\n\
                 \x20 }\n\
                 \x20 return Object.freeze(object);\n\
                 })",
            )?;
            let mut keep = |value: Value<'env>| -> Result<sys::napi_ref, Error> {
                let reference = self.keep(value)?;
                kept.push(reference);
                Ok(reference)
            };
            let map = keep(map)?;
            let map_set = keep(map_set)?;
            let symbol = keep(symbol)?;
            let array_of = keep(array_of)?;
            let map_of = keep(map_of)?;
            let station = Station::start(self)?;
            Ok(State {
                map,
                map_set,
                symbol,
                array_of,
                map_of,
                objects: RefCell::default(),
                classes: Vec::new(),
                station,
            })
        })();
        let mut state = match init {
            Ok(state) => state,
            Err(error) => {
                // SAFETY: every reference in `kept` and `classes` belongs to
                // this live environment on this thread and is deleted once.
                unsafe {
                    for reference in kept {
                        sys::napi_delete_reference(self.raw, reference);
                    }
                    for class in classes {
                        sys::napi_delete_reference(self.raw, class.constructor);
                    }
                }
                return Err(error);
            }
        };
        state.classes = classes;
        let state = Box::into_raw(Box::new(state));
        let status = napi!(
            self,
            napi_set_instance_data(state.cast::<c_void>(), Some(drop_state), ptr::null_mut())
        );
        if status.is_err() {
            // SAFETY: Node-API did not take the box, which `into_raw` made.
            let state = unsafe { Box::from_raw(state) };
            // SAFETY: `self.raw` is a live environment on this thread.
            unsafe { release_state(self.raw, *state) };
        }
        status
    }

    /// Return the state of this environment.
    pub(crate) fn state(self) -> Result<&'env State, Error> {
        let data = napi!(self, napi_get_instance_data() -> out)?;
        if data.is_null() {
            return Err(Error::Message(
                "the Node.js addon was not initialized in this environment".to_owned(),
            ));
        }
        // SAFETY: `start` set the data to a `State`, which Node.js drops only
        // when the environment, and so `'env`, ends.
        Ok(unsafe { &*data.cast::<State>() })
    }
}

/// Drop the state of an environment that Node.js tears down.
unsafe extern "C" fn drop_state(env: sys::napi_env, data: *mut c_void, _hint: *mut c_void) {
    // SAFETY: `Env::start` gave Node.js this box, which Node.js returns once.
    let state = unsafe { Box::from_raw(data.cast::<State>()) };
    // SAFETY: Node.js runs the finalizer on the environment's thread while
    // `env` can still release its references.
    unsafe { release_state(env, *state) };
}

/// Release every Node-API reference held by `state`.
///
/// # Safety
///
/// Must run on `env`'s JavaScript thread while `env` is still valid for
/// `napi_delete_reference`.
unsafe fn release_state(env: sys::napi_env, state: State) {
    // SAFETY: every reference below belongs to `env` and is deleted once.
    unsafe {
        sys::napi_delete_reference(env, state.map);
        sys::napi_delete_reference(env, state.map_set);
        sys::napi_delete_reference(env, state.symbol);
        sys::napi_delete_reference(env, state.array_of);
        sys::napi_delete_reference(env, state.map_of);
        for reference in state.objects.into_inner().into_values() {
            sys::napi_delete_reference(env, reference);
        }
        for class in state.classes {
            sys::napi_delete_reference(env, class.constructor);
        }
    }
}

/// The length of a string as Node-API takes it.
pub(super) fn length(value: &str) -> isize {
    isize::try_from(value.len()).expect("a str holds at most isize::MAX bytes")
}
