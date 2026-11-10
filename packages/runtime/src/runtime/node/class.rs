//! The addon's exports: its functions, the classes of its resources, and the
//! module initializer that defines them in each environment that loads it.
//!
//! A class constructor wraps the resource's Rust object in its JavaScript
//! object and tags the object with the class, so a method checks `this`
//! before it reads the object. A factory builds its object through the same
//! constructor: it leaves the Rust object in a thread-local slot that the
//! constructor takes. Each environment keeps the constructors in its own
//! state, so worker threads load the addon side by side.

use std::any::Any;
use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::ptr;

use napi_sys as sys;

use super::env::{self, CallInfo, Callback, DefinedClass, Env, napi};
use super::error::Error;
use super::value::{Kind, Value};
use crate::ir::discovery;
use crate::registry;

/// One function or class that the addon exports, which the generated
/// wrappers submit through `inventory`.
pub enum Registration {
    /// A free function.
    Function(Method),
    /// A resource class.
    Class(&'static ClassDescriptor),
}

inventory::collect!(Registration);

/// A function that JavaScript calls by its name: a free function, or a
/// class's factory or method.
pub struct Method {
    /// The function's JavaScript name.
    pub name: &'static str,
    /// What the function runs.
    pub call: Callback,
}

/// One resource class: its constructor, its factories, and its methods.
pub struct ClassDescriptor {
    /// The class's JavaScript name.
    name: &'static str,
    /// The constructor that Node.js calls.
    construct: unsafe extern "C" fn(sys::napi_env, sys::napi_callback_info) -> sys::napi_value,
    /// The resource's class constructor, which `new` runs, when it has one.
    constructor: Option<Callback>,
    /// The static factories.
    factories: &'static [Method],
    /// The instance methods, `close` among them.
    methods: &'static [Method],
}

impl ClassDescriptor {
    /// Describe the class of the Rust objects `C`.
    #[must_use]
    pub const fn new<C: Class>(
        name: &'static str,
        constructor: Option<Callback>,
        factories: &'static [Method],
        methods: &'static [Method],
    ) -> Self {
        Self {
            name,
            construct: construct::<C>,
            constructor,
            factories,
            methods,
        }
    }
}

/// The Rust object behind the JavaScript objects of one resource class,
/// which the generated wrapper implements.
pub trait Class: 'static {
    /// The class's descriptor, a static whose address identifies the class.
    fn descriptor() -> &'static ClassDescriptor;
}

thread_local! {
    /// The Rust object that a factory leaves for the class constructor.
    static PENDING: Cell<Option<Box<dyn Any>>> = const { Cell::new(None) };
}

/// The upper half of every class's type tag, which marks the tag as Bimetal's.
const TAG_MARK: u64 = u64::from_be_bytes(*b"bimetal!");

/// Return the type tag of the objects of class `C`.
fn tag<C: Class>() -> sys::napi_type_tag {
    sys::napi_type_tag {
        lower: ptr::from_ref(C::descriptor()).addr() as u64,
        upper: TAG_MARK,
    }
}

/// Make the JavaScript object of a new resource, the Rust object `value`.
///
/// # Errors
///
/// Returns the error of the class's constructor, which is pending.
pub fn instance<C: Class>(env: Env<'_>, value: C) -> Result<Value<'_>, Error> {
    let descriptor = ptr::from_ref(C::descriptor()).addr();
    let class = env
        .state()?
        .classes
        .iter()
        .find(|class| class.descriptor == descriptor)
        .ok_or_else(|| Error::Message("the class of the resource is not defined".to_owned()))?;
    let constructor = env.reference(class.constructor)?;
    PENDING.set(Some(Box::new(value)));
    let instance = constructor.construct([]);
    // The constructor took the object, unless it failed first.
    PENDING.take();
    instance
}

/// The constructor of class `C`, which Node.js calls for `new` and for
/// [`instance`].
unsafe extern "C" fn construct<C: Class>(
    env: sys::napi_env,
    info: sys::napi_callback_info,
) -> sys::napi_value {
    // SAFETY: Node.js calls a constructor on the environment's thread, with
    // a scope open until it returns.
    let env = unsafe { Env::new(env) };
    let call = CallInfo::new(env, info);
    env.run(|| {
        if let Some(value) = PENDING.take() {
            let this = call.receiver()?;
            let value = value.downcast::<C>().map_err(|_| {
                Error::Message("a resource of another class reached the constructor".to_owned())
            })?;
            wrap(env, this, value)?;
            return Ok(this);
        }
        // A class constructor makes its object through `instance`, which the
        // `new` expression returns instead of `this`.
        let descriptor = C::descriptor();
        match descriptor.constructor {
            Some(constructor) => constructor(call),
            None => Err(Error::Type(format!(
                "{} has no constructor; create one with its factories",
                descriptor.name
            ))),
        }
    })
}

/// Give `this` the Rust object `value`, and tag it with its class.
fn wrap<C: Class>(env: Env<'_>, this: Value<'_>, value: Box<C>) -> Result<(), Error> {
    let raw = Box::into_raw(value);
    let wrapped = napi!(
        env,
        napi_wrap(
            this.raw,
            raw.cast::<c_void>(),
            Some(finalize::<C>),
            ptr::null_mut(),
            ptr::null_mut()
        )
    );
    if let Err(error) = wrapped {
        // SAFETY: Node-API did not take the box, which `into_raw` made.
        drop(unsafe { Box::from_raw(raw) });
        return Err(error);
    }
    napi!(env, napi_type_tag_object(this.raw, &tag::<C>()))
}

/// Drop the Rust object of a JavaScript object that the garbage collector
/// collected, or that its environment tore down.
unsafe extern "C" fn finalize<C: Class>(
    _env: sys::napi_env,
    data: *mut c_void,
    _hint: *mut c_void,
) {
    // SAFETY: `wrap` gave Node.js this box, which Node.js returns once.
    env::discard(unsafe { Box::from_raw(data.cast::<C>()) });
}

/// The Rust object of `value`, or `None` when `value` is not an object of
/// class `C`. The object stays alive for as long as the call's arguments.
///
/// # Errors
///
/// Returns the error of Node-API, which is pending.
pub fn object_of<'env, C: Class>(value: Value<'env>) -> Result<Option<&'env C>, Error> {
    let env = value.env();
    let tagged = value.kind()? == Kind::Object
        && napi!(env, napi_check_object_type_tag(value.raw, &tag::<C>()) -> out)?;
    if !tagged {
        return Ok(None);
    }
    let data = napi!(env, napi_unwrap(value.raw) -> out)?;
    // SAFETY: the tag says that `wrap` gave the object a box of `C`, which
    // Node.js drops only once the object is unreachable, and the call's
    // arguments stay reachable until the call returns, at the end of `'env`.
    Ok(Some(unsafe { &*data.cast::<C>() }))
}

impl<'env> CallInfo<'env> {
    /// The Rust object of the call's receiver, `this`.
    ///
    /// # Errors
    ///
    /// Returns a `TypeError` when the receiver is not an object of class `C`.
    pub fn this<C: Class>(&self) -> Result<&'env C, Error> {
        let env = self.env();
        let this = self.receiver()?;
        let tagged = this.kind()? == Kind::Object
            && napi!(env, napi_check_object_type_tag(this.raw, &tag::<C>()) -> out)?;
        if !tagged {
            return Err(Error::Type(format!(
                "the receiver is not a {}",
                C::descriptor().name
            )));
        }
        let data = napi!(env, napi_unwrap(this.raw) -> out)?;
        // SAFETY: the tag says that `wrap` gave the object a box of `C`,
        // which Node.js drops only once the object is unreachable, and the
        // call's receiver stays reachable until the call returns, at the end
        // of `'env`.
        Ok(unsafe { &*data.cast::<C>() })
    }
}

/// Define every linked export on the addon's `exports`, with the hash of the
/// application's contract, which the generated runtime compares with its
/// own, as `__bimetal_contract__`.
///
/// # Safety
///
/// Node.js must call this as the addon's module initializer, on the
/// environment's thread, with the environment and its `exports` object.
pub unsafe fn register(
    env: sys::napi_env,
    exports: sys::napi_value,
    package_name: &str,
    package_version: &str,
) -> sys::napi_value {
    // Node.js then keeps `exports` empty, which fails the package's contract
    // check at import.
    if !env::load_functions() {
        return ptr::null_mut();
    }
    // SAFETY: the caller passes a live environment, on this thread, during
    // this call.
    let env = unsafe { Env::new(env) };
    env.run(|| {
        let exports = env.value(exports);
        let contract_hash = registry::contract_hash(package_name, package_version)
            .map_err(|error| Error::Message(error.to_string()))?;
        exports.set_property(
            env.string(discovery::CONTRACT_HASH_ATTRIBUTE)?,
            env.string(&contract_hash)?,
        )?;
        let mut classes = Vec::new();
        for registration in inventory::iter::<Registration> {
            match registration {
                Registration::Function(function) => {
                    let defined = define_function(env, function)?;
                    exports.set_property(env.string(function.name)?, defined)?;
                }
                Registration::Class(class) => {
                    let defined = define_class(env, class)?;
                    exports.set_property(env.string(class.name)?, defined)?;
                    classes.push(DefinedClass {
                        descriptor: ptr::from_ref(*class).addr(),
                        constructor: env.keep(defined)?,
                    });
                }
            }
        }
        env.start(classes)?;
        Ok(exports)
    })
}

/// Return the data of a JavaScript function that runs `callback`.
fn data(callback: &'static Callback) -> *mut c_void {
    ptr::from_ref(callback).cast_mut().cast::<c_void>()
}

/// Define the JavaScript function of `function`.
fn define_function<'env>(env: Env<'env>, function: &'static Method) -> Result<Value<'env>, Error> {
    napi!(
        env,
        napi_create_function(
            function.name.as_ptr().cast::<c_char>(),
            env::length(function.name),
            Some(env::trampoline),
            data(&function.call)
        ) -> value
    )
}

/// Define the JavaScript class of `class`.
fn define_class<'env>(
    env: Env<'env>,
    class: &'static ClassDescriptor,
) -> Result<Value<'env>, Error> {
    use sys::PropertyAttributes::{configurable, static_, writable};

    let factories = class.factories.iter().map(|method| (method, static_));
    let members = factories.chain(class.methods.iter().map(|method| (method, 0)));
    let mut properties = Vec::new();
    for (method, placement) in members {
        properties.push(sys::napi_property_descriptor {
            utf8name: ptr::null(),
            name: env.string(method.name)?.raw,
            method: Some(env::trampoline),
            getter: None,
            setter: None,
            value: ptr::null_mut(),
            // As a JavaScript class defines its methods.
            attributes: writable | configurable | placement,
            data: data(&method.call),
        });
    }
    napi!(
        env,
        napi_define_class(
            class.name.as_ptr().cast::<c_char>(),
            env::length(class.name),
            Some(class.construct),
            ptr::null_mut(),
            properties.len(),
            properties.as_ptr()
        ) -> value
    )
}
