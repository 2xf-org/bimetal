//! How the extension makes the Python value of a Rust result: plans that
//! follow the contract's types, so that a model arrives as an instance of its
//! generated class, a date-time as a `datetime`, and a string enum as its
//! member, with no validation in Python, since Rust checked the value.
//!
//! The generated package registers its classes in its runtime module's
//! `NATIVE_CLASSES`. An extension without that module, as in the runtime's
//! own tests, makes plain values, as `pythonize` does.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PySet, PyString, PyType};
use pyo3::{ffi, intern};

use crate::ContractType;
use crate::ir::{FieldDef, TypeDef, TypeRef, TypeShape, VariantPayload};
use crate::registry::TypeRegistration;
use crate::runtime::CallbackArguments;
use crate::runtime::ContractError;

/// The plan of one argument or result, kept in a static at its call site so
/// that it is built once.
#[doc(hidden)]
#[derive(Debug)]
pub struct ValuePlan(OnceLock<Planned>);

/// The plan of one contract type, and whether the extension reads its
/// arguments by it.
#[derive(Debug)]
struct Planned {
    /// `None` when the value reaches a numeric buffer, which the generated
    /// code restores and validates itself.
    plan: Option<Plan>,
    /// Whether an argument is read as Python holds it after validation, rather
    /// than from the dump that the generated code passes: true for a value that
    /// is not plain and reaches no buffer and no enum with data.
    reads: bool,
}

impl ValuePlan {
    /// A plan that is built at its first use.
    #[must_use]
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn planned(&self, reference: impl FnOnce() -> TypeRef) -> &Planned {
        self.0.get_or_init(|| {
            let reference = reference();
            let plan = (!reaches(&reference, &mut Vec::new(), |item| {
                matches!(item, Reached::Buffer)
            }))
            .then(|| Plan::of(&reference));
            let reads = plan
                .as_ref()
                .is_some_and(|plan| !matches!(plan, Plan::Plain))
                && !reaches(&reference, &mut Vec::new(), |item| {
                    matches!(item, Reached::EnumWithData)
                });
            Planned { plan, reads }
        })
    }

    /// The plan by which the extension makes a result of `T`.
    pub(crate) fn of<T: ContractType + ?Sized>(&self) -> Option<&Plan> {
        self.planned(T::type_ref).plan.as_ref()
    }

    /// The plan by which the extension makes the arguments of a callback,
    /// a tuple of `A`'s items.
    pub(crate) fn of_arguments<A: CallbackArguments>(&self) -> Option<&Plan> {
        self.planned(|| TypeRef::Tuple {
            items: A::type_refs(),
        })
        .plan
        .as_ref()
    }

    /// The plan by which the extension reads an argument of `T` as Python
    /// holds it, or `None` when it reads the argument's dump.
    pub(crate) fn reads<T: ContractType + ?Sized>(&self) -> Option<&Plan> {
        let planned = self.planned(T::type_ref);
        planned.plan.as_ref().filter(|_| planned.reads)
    }
}

impl Default for ValuePlan {
    fn default() -> Self {
        Self::new()
    }
}

/// What the extension makes of one contract type.
#[derive(Debug)]
pub(crate) enum Plan {
    /// A value that Python holds as `pythonize` makes it: a scalar, JSON, or
    /// a container of them.
    Plain,
    /// A `datetime` with Pydantic's `TzInfo`, from RFC 3339 text.
    DateTime,
    /// A `date`, from `YYYY-MM-DD` text.
    Date,
    /// An `int` of nanoseconds, from Serde's `{secs, nanos}`.
    Duration,
    /// `bytes`.
    Bytes,
    Option(Box<Plan>),
    List(Box<Plan>),
    Tuple(Vec<Plan>),
    Map(Box<Plan>),
    /// A model, a string enum, or an enum with data.
    Named(Arc<Named>),
}

impl Plan {
    /// The plan of `reference`, which reaches no buffer.
    fn of(reference: &TypeRef) -> Self {
        match reference {
            TypeRef::DateTime => Self::DateTime,
            TypeRef::Date => Self::Date,
            TypeRef::Duration => Self::Duration,
            TypeRef::Bytes | TypeRef::FixedBytes { .. } => Self::Bytes,
            TypeRef::Option { item } => Self::of(item).wrapped(Self::Option),
            TypeRef::List { item } => Self::of(item).wrapped(Self::List),
            TypeRef::Map { value } => Self::of(value).wrapped(Self::Map),
            TypeRef::Tuple { items } => {
                let items: Vec<Self> = items.iter().map(Self::of).collect();
                if items.iter().all(|item| matches!(item, Self::Plain)) {
                    Self::Plain
                } else {
                    Self::Tuple(items)
                }
            }
            TypeRef::Named { identity } => match named(&identity.to_string()) {
                Some(named) => match &named.definition.shape {
                    TypeShape::Alias { target } => Self::of(target),
                    _ => Self::Named(Arc::clone(named)),
                },
                // A type that no linked crate derives `Model` for cannot
                // reach a host: the registry refuses the contract.
                None => Self::Plain,
            },
            TypeRef::Unit
            | TypeRef::Bool
            | TypeRef::Int { .. }
            | TypeRef::Float { .. }
            | TypeRef::String
            | TypeRef::Json
            | TypeRef::Buffer { .. }
            | TypeRef::Arrow { .. }
            | TypeRef::Resource { .. }
            | TypeRef::Callback { .. } => Self::Plain,
        }
    }

    /// `wrap` around this plan, unless `pythonize` makes the whole value.
    fn wrapped(self, wrap: fn(Box<Self>) -> Self) -> Self {
        match self {
            Self::Plain => Self::Plain,
            item => wrap(Box::new(item)),
        }
    }
}

/// Whether `reference` reaches what `found` looks for, through the types it
/// names, each looked at once.
fn reaches(reference: &TypeRef, seen: &mut Vec<String>, found: fn(Reached) -> bool) -> bool {
    match reference {
        TypeRef::Buffer { .. } => found(Reached::Buffer),
        TypeRef::Named { identity } => {
            let key = identity.to_string();
            if seen.contains(&key) {
                return false;
            }
            let Some(named) = named(&key) else {
                return false;
            };
            seen.push(key);
            if matches!(
                named.definition.shape,
                TypeShape::TaggedEnum { .. } | TypeShape::ExternalEnum { .. }
            ) && found(Reached::EnumWithData)
            {
                return true;
            }
            named
                .definition
                .type_refs()
                .any(|reference| reaches(reference, seen, found))
        }
        _ => reference
            .children()
            .iter()
            .any(|reference| reaches(reference, seen, found)),
    }
}

/// What [`reaches`] looks for.
#[derive(Clone, Copy)]
enum Reached {
    Buffer,
    EnumWithData,
}

/// One linked type of the contract, and what the extension makes of it once
/// the generated package has registered its class.
pub(crate) struct Named {
    key: String,
    definition: TypeDef,
    plan: PyOnceLock<NamedPlan>,
}

impl std::fmt::Debug for Named {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("Named").field(&self.key).finish()
    }
}

/// The linked type with `key`, the key that the generated package registers
/// its class under.
fn named(key: &str) -> Option<&'static Arc<Named>> {
    static NAMED: OnceLock<HashMap<String, Arc<Named>>> = OnceLock::new();
    NAMED
        .get_or_init(|| {
            let mut defaults = Vec::new();
            inventory::iter::<TypeRegistration>
                .into_iter()
                .map(|registration| {
                    let definition = (registration.0)(&mut defaults);
                    let key = definition.identity().to_string();
                    let named = Named {
                        key: key.clone(),
                        definition,
                        plan: PyOnceLock::new(),
                    };
                    (key, Arc::new(named))
                })
                .collect()
        })
        .get(key)
}

impl Named {
    /// What the extension makes of this type, built at its first use.
    ///
    /// # Errors
    ///
    /// Returns `RuntimeError` when the generated package registered no class
    /// for the type, or when the class differs from the contract.
    pub(crate) fn plan<'a>(&'a self, py: Python<'_>) -> PyResult<&'a NamedPlan> {
        self.plan
            .get_or_try_init(py, || NamedPlan::build(py, &self.key, &self.definition))
    }
}

/// What the extension makes of one model, string enum, or enum with data.
#[derive(Debug)]
pub(crate) enum NamedPlan {
    Model(ModelPlan),
    /// Each member, by the text that Serde writes for it.
    StringEnum(HashMap<String, Py<PyAny>>),
    /// The model of each variant, by the value of its tag field.
    Tagged {
        tag: String,
        variants: HashMap<String, ModelPlan>,
    },
    /// The model of each variant, by its wire name.
    External(HashMap<String, ModelPlan>),
}

impl NamedPlan {
    fn build(py: Python<'_>, key: &str, definition: &TypeDef) -> PyResult<Self> {
        let registered = classes(py)
            .ok_or_else(|| PyRuntimeError::new_err("the package registers no model classes"))?
            .bind(py)
            .get_item(key)?
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "the generated package registers no class for `{key}`; run `bimetal build`"
                ))
            })?;
        Ok(match &definition.shape {
            TypeShape::Struct { fields } => {
                Self::Model(ModelPlan::build(registered.cast_into::<PyType>()?, fields)?)
            }
            TypeShape::StringEnum { .. } => {
                let mut members = HashMap::new();
                for member in registered.try_iter()? {
                    let member = member?;
                    let text: String = member.getattr(intern!(py, "value"))?.extract()?;
                    members.insert(text, member.unbind());
                }
                Self::StringEnum(members)
            }
            TypeShape::TaggedEnum { tag, variants } => {
                let classes = registered.cast::<PyDict>()?;
                let mut built = HashMap::new();
                for variant in variants {
                    let class = variant_class(classes, &variant.wire_name)?;
                    built.insert(
                        variant.wire_name.clone(),
                        ModelPlan::build(class, &variant.fields)?,
                    );
                }
                Self::Tagged {
                    tag: tag.clone(),
                    variants: built,
                }
            }
            TypeShape::ExternalEnum { variants } => {
                let classes = registered.cast::<PyDict>()?;
                let mut built = HashMap::new();
                for variant in variants {
                    let class = variant_class(classes, &variant.wire_name)?;
                    let fields = match &variant.payload {
                        VariantPayload::Unit => Vec::new(),
                        VariantPayload::Value { ty } => vec![FieldDef {
                            rust_name: "value".to_owned(),
                            wire_name: "value".to_owned(),
                            docs: None,
                            ty: ty.clone(),
                            required: true,
                            default: None,
                            constraints: Default::default(),
                        }],
                        VariantPayload::Fields { fields } => fields.clone(),
                    };
                    built.insert(variant.wire_name.clone(), ModelPlan::build(class, &fields)?);
                }
                Self::External(built)
            }
            TypeShape::Alias { .. } => {
                return Err(PyRuntimeError::new_err(format!(
                    "`{key}` is an alias, which has no class"
                )));
            }
        })
    }
}

/// The class of the variant `wire_name` of an enum with data.
fn variant_class<'py>(
    classes: &Bound<'py, PyDict>,
    wire_name: &str,
) -> PyResult<Bound<'py, PyType>> {
    classes
        .get_item(wire_name)?
        .ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "the generated package registers no class for the variant `{wire_name}`"
            ))
        })?
        .cast_into::<PyType>()
        .map_err(PyErr::from)
}

/// How the extension makes an instance of one generated model class.
#[derive(Debug)]
pub(crate) struct ModelPlan {
    pub(crate) class: Py<PyType>,
    /// The class's fields, in its order.
    pub(crate) fields: Vec<FieldPlan>,
    /// Each field's attribute mapped to `None`, in the class's order: the
    /// `__dict__` that an instance copies, table and all, before it sets the
    /// values.
    blank: Py<PyDict>,
    /// Each field's attribute: the fields set of an instance whose fields
    /// Serde all wrote.
    every_field: Py<PySet>,
}

/// One field of a [`ModelPlan`].
#[derive(Debug)]
pub(crate) struct FieldPlan {
    /// The name that Serde writes.
    pub(crate) wire_name: String,
    /// The attribute, which keys the instance's `__dict__`.
    pub(crate) attribute: Py<PyString>,
    /// Pydantic's `FieldInfo`, which gives the field's default.
    pub(crate) info: Py<PyAny>,
    pub(crate) plan: Plan,
}

impl ModelPlan {
    /// The plan of `class`, a model whose fields Serde writes as `fields`.
    /// A field of the class that `fields` lacks is a tag, which is a string.
    fn build(class: Bound<'_, PyType>, fields: &[FieldDef]) -> PyResult<Self> {
        let py = class.py();
        let model_fields = class.getattr(intern!(py, "model_fields"))?;
        let model_fields = model_fields.cast::<PyDict>()?;
        let mut planned = Vec::with_capacity(model_fields.len());
        let blank = PyDict::new(py);
        for (attribute, info) in model_fields.iter() {
            blank.set_item(&attribute, py.None())?;
            let alias = info.getattr(intern!(py, "serialization_alias"))?;
            let wire_name: String = if alias.is_none() {
                attribute.extract()?
            } else {
                alias.extract()?
            };
            let plan = fields
                .iter()
                .find(|field| field.wire_name == wire_name)
                .map_or(Plan::Plain, |field| Plan::of(&field.ty));
            planned.push(FieldPlan {
                wire_name,
                attribute: attribute.cast_into::<PyString>()?.unbind(),
                info: info.unbind(),
                plan,
            });
        }
        Ok(Self {
            class: class.unbind(),
            fields: planned,
            every_field: PySet::new(py, blank.keys())?.unbind(),
            blank: blank.unbind(),
        })
    }

    /// Make an instance of the class from the value of each field, in the
    /// class's order, as Pydantic's validation would leave it. A field that
    /// Serde did not write takes the class's default.
    pub(crate) fn instance<'py>(
        &self,
        py: Python<'py>,
        values: Vec<Option<Bound<'py, PyAny>>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let dict = self.blank.bind(py).copy()?;
        let every = values.iter().all(Option::is_some);
        let written = if every {
            copy_set(self.every_field.bind(py))?
        } else {
            PySet::empty(py)?
        };
        for (field, value) in self.fields.iter().zip(values) {
            let attribute = field.attribute.bind(py);
            let value = match value {
                Some(value) => {
                    if !every {
                        written.add(attribute)?;
                    }
                    value
                }
                None => {
                    let options = PyDict::new(py);
                    options.set_item(intern!(py, "call_default_factory"), true)?;
                    field.info.bind(py).call_method(
                        intern!(py, "get_default"),
                        (),
                        Some(&options),
                    )?
                }
            };
            dict.set_item(attribute, value)?;
        }
        static OBJECT_NEW: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
        let instance = OBJECT_NEW
            .get_or_try_init(py, || {
                py.import(intern!(py, "builtins"))?
                    .getattr(intern!(py, "object"))?
                    .getattr(intern!(py, "__new__"))
                    .map(Bound::unbind)
            })?
            .bind(py)
            .call1((self.class.bind(py),))?;
        let none = py.None();
        let none = none.bind(py);
        set_slot(&instance, intern!(py, "__dict__"), dict.as_any())?;
        set_slot(
            &instance,
            intern!(py, "__pydantic_fields_set__"),
            written.as_any(),
        )?;
        set_slot(&instance, intern!(py, "__pydantic_extra__"), none)?;
        set_slot(&instance, intern!(py, "__pydantic_private__"), none)?;
        Ok(instance)
    }

    /// The index of the field that Serde writes as `wire_name`, looked for
    /// from `from` on first, since Serde writes a struct's fields in the
    /// order that its class has them.
    pub(crate) fn field(&self, wire_name: &str, from: usize) -> Option<usize> {
        let (before, after) = self.fields.split_at(from.min(self.fields.len()));
        let named = |field: &FieldPlan| field.wire_name == wire_name;
        after
            .iter()
            .position(named)
            .map(|index| from + index)
            .or_else(|| before.iter().position(named))
    }
}

/// A new set with the items of `set`, whose table it copies as it is.
fn copy_set<'py>(set: &Bound<'py, PySet>) -> PyResult<Bound<'py, PySet>> {
    // SAFETY: the `Bound` proves the interpreter lock is held; `PySet_New` returns a new
    // reference to a `set`, so the unchecked cast holds, or null with an error set.
    unsafe {
        Bound::from_owned_ptr_or_err(set.py(), ffi::PySet_New(set.as_ptr()))
            .map(|copy| copy.cast_into_unchecked())
    }
}

/// Set an attribute as `object.__setattr__` does, past the frozen model's
/// own `__setattr__`, which refuses every assignment.
fn set_slot(
    instance: &Bound<'_, PyAny>,
    name: &Bound<'_, PyString>,
    value: &Bound<'_, PyAny>,
) -> PyResult<()> {
    // SAFETY: the `Bound`s keep the three objects live and prove the interpreter
    // lock is held; `PyObject_GenericSetAttr` only borrows them.
    let status =
        unsafe { ffi::PyObject_GenericSetAttr(instance.as_ptr(), name.as_ptr(), value.as_ptr()) };
    if status == 0 {
        Ok(())
    } else {
        Err(PyErr::fetch(instance.py()))
    }
}

/// The package name of the extension, from its module, such as `probe` for
/// `probe.native`, which `register` records.
pub(crate) static PACKAGE: OnceLock<String> = OnceLock::new();

/// The generated package's `NATIVE_CLASSES`, or `None` when the extension
/// has no generated package.
pub(crate) fn classes(py: Python<'_>) -> Option<&Py<PyDict>> {
    static CLASSES: PyOnceLock<Option<Py<PyDict>>> = PyOnceLock::new();
    CLASSES
        .get_or_init(py, || runtime_table(py, intern!(py, "NATIVE_CLASSES")))
        .as_ref()
}

/// The exception class of the contract error `E`, from the module that the
/// generated package's `NATIVE_ERRORS` names for it, found once, or `None`
/// when the extension has no generated package.
pub(crate) fn error_class<E: ContractError + 'static>(py: Python<'_>) -> Option<Bound<'_, PyAny>> {
    static ERRORS: PyOnceLock<Option<Py<PyDict>>> = PyOnceLock::new();
    static FOUND: Mutex<Vec<(TypeId, Py<PyAny>)>> = Mutex::new(Vec::new());
    let found = |classes: &[(TypeId, Py<PyAny>)]| {
        classes
            .iter()
            .find(|(error, _)| *error == TypeId::of::<E>())
            .map(|(_, class)| class.bind(py).clone())
    };
    if let Some(class) = found(
        &FOUND
            .lock()
            .expect("no code panics with the classes locked"),
    ) {
        return Some(class);
    }
    let errors = ERRORS
        .get_or_init(py, || runtime_table(py, intern!(py, "NATIVE_ERRORS")))
        .as_ref()?;
    let key = E::type_identity().to_string();
    let (module, name): (String, String) = errors.bind(py).get_item(key).ok()??.extract().ok()?;
    let class = py.import(module).ok()?.getattr(name).ok()?;
    FOUND
        .lock()
        .expect("no code panics with the classes locked")
        .push((TypeId::of::<E>(), class.clone().unbind()));
    Some(class)
}

/// The dictionary `name` of the generated package's runtime module.
fn runtime_table(py: Python<'_>, name: &Bound<'_, PyString>) -> Option<Py<PyDict>> {
    let package = PACKAGE.get()?;
    let runtime = py
        .import(format!(
            "{package}.{}",
            crate::ir::python_names::RUNTIME_MODULE
        ))
        .ok()?;
    let table = runtime.getattr(name).ok()?;
    table.cast_into::<PyDict>().ok().map(Bound::unbind)
}
