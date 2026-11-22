//! A Serde deserializer that reads a Python argument by its [`Plan`]: a
//! model instance of its generated class from its fields, a `datetime` and a
//! `date` from theirs, and a duration from its nanoseconds, with no dump
//! through Pydantic, since the instance was validated when it was made.
//!
//! It reads only what Pydantic would pass unchanged. Anything else, such as a
//! dictionary passed for a model, fails, and the generated code then calls
//! again with Pydantic's dump, which validates it. A plain value is read as
//! `pythonize` reads it, but a bool only from a bool, where `pythonize` takes
//! any object's truth, and a container item by item, so that the rule holds
//! inside it.

use std::fmt::{self, Display};

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::iter::BoundDictIterator;
use pyo3::types::{PyBool, PyBytes, PyDict, PyList, PyString, PyTuple};
use pythonize::Depythonizer;
use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, Visitor};

use super::python_plan::{ExternalForm, ExternalVariant, ModelPlan, NamedPlan, Plan};
use super::python_time;

/// How deep nested containers and models may go before falling back, preventing
/// native stack overflow on cyclic or deeply nested Python inputs.
const MAX_DEPTH: usize = 128;

/// Read `value` by `plan`.
///
/// # Errors
///
/// Returns an [`Error`] when the value is not one that the plan reads as it
/// is, or when Serde rejects it.
pub(crate) fn from_python<T: DeserializeOwned>(
    value: &Bound<'_, PyAny>,
    plan: &Plan,
) -> Result<T, Error> {
    T::deserialize(Reader {
        value: value.clone(),
        plan,
        depth: 0,
    })
}

/// Why a value cannot be read as it is, with the Python error that stopped
/// the read, if one did.
#[derive(Debug)]
pub(crate) struct Error(pub(crate) Option<PyErr>);

impl Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the argument needs Pydantic's dump")
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<T: Display>(_: T) -> Self {
        Self(None)
    }
}

impl From<PyErr> for Error {
    fn from(error: PyErr) -> Self {
        Self(Some(error))
    }
}

impl From<pythonize::PythonizeError> for Error {
    fn from(_: pythonize::PythonizeError) -> Self {
        Self(None)
    }
}

/// Reads one Python value by one plan.
struct Reader<'a, 'py> {
    value: Bound<'py, PyAny>,
    plan: &'a Plan,
    depth: usize,
}

impl<'a, 'py> Reader<'a, 'py> {
    /// Return the next nesting depth, or fall back past [`MAX_DEPTH`].
    fn inner(&self) -> Result<usize, Error> {
        if self.depth < MAX_DEPTH {
            Ok(self.depth + 1)
        } else {
            Err(Error(None))
        }
    }

    /// Read a value whose plan is plain as `pythonize` reads it.
    fn plain(&self) -> Depythonizer<'_, 'py> {
        Depythonizer::from_object(&self.value)
    }

    /// Read the items of a list or a tuple whose plan is plain, each by the
    /// plain plan.
    fn plain_items<'de, V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let depth = self.inner()?;
        let items = SeqItems::of(self.value)?;
        visitor.visit_seq(Items {
            items,
            plans: Plans::Same(&Plan::Plain),
            index: 0,
            depth,
        })
    }
}

/// Forward a method to `pythonize` for a plain value, and read every other
/// one through `deserialize_any`.
macro_rules! by_plan {
    ($($method:ident($($argument:ident: $type:ty),*);)+) => {
        $(
            fn $method<V: Visitor<'de>>(self, $($argument: $type,)* visitor: V) -> Result<V::Value, Error> {
                if matches!(self.plan, Plan::Plain) {
                    return Ok(de::Deserializer::$method(&mut self.plain(), $($argument,)* visitor)?);
                }
                self.deserialize_any(visitor)
            }
        )+
    };
}

impl<'de> de::Deserializer<'de> for Reader<'_, '_> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.plan {
            Plan::Plain => Ok(de::Deserializer::deserialize_any(
                &mut self.plain(),
                visitor,
            )?),
            Plan::DateTime => {
                let text = python_time::date_time_text(&self.value)?.ok_or(Error(None))?;
                visitor.visit_str(text.as_str())
            }
            Plan::Date => {
                let text = python_time::date_text(&self.value)?.ok_or(Error(None))?;
                visitor.visit_str(text.as_str())
            }
            Plan::Duration => {
                // Pydantic reads a duration strictly, so it refuses a bool.
                if self.value.is_instance_of::<PyBool>() {
                    return Err(Error(None));
                }
                let nanoseconds: u128 = self.value.extract()?;
                let seconds =
                    u64::try_from(nanoseconds / 1_000_000_000).map_err(|_| Error(None))?;
                let nanos = u32::try_from(nanoseconds % 1_000_000_000).map_err(|_| Error(None))?;
                visitor.visit_map(de::value::MapDeserializer::new(
                    [("secs", seconds), ("nanos", u64::from(nanos))].into_iter(),
                ))
            }
            Plan::Bytes => {
                let bytes = self.value.cast::<PyBytes>().map_err(PyErr::from)?;
                visitor.visit_bytes(bytes.as_bytes())
            }
            Plan::Option(item) => {
                if self.value.is_none() {
                    visitor.visit_none()
                } else {
                    visitor.visit_some(Reader {
                        value: self.value,
                        plan: item,
                        depth: self.depth,
                    })
                }
            }
            Plan::List(item) => {
                let depth = self.inner()?;
                let items = SeqItems::of(self.value)?;
                visitor.visit_seq(Items {
                    items,
                    plans: Plans::Same(item),
                    index: 0,
                    depth,
                })
            }
            Plan::Tuple(plans) => {
                let depth = self.inner()?;
                let items = SeqItems::of(self.value)?;
                if items.len() != plans.len() {
                    return Err(Error(None));
                }
                visitor.visit_seq(Items {
                    items,
                    plans: Plans::Each(plans),
                    index: 0,
                    depth,
                })
            }
            Plan::Map(value) => {
                let depth = self.inner()?;
                let dict = self.value.cast_into::<PyDict>().map_err(PyErr::from)?;
                visitor.visit_map(Entries {
                    iter: dict.iter(),
                    plan: value,
                    pending: None,
                    depth,
                })
            }
            Plan::Named(named) => {
                let py = self.value.py();
                match named.plan(py)? {
                    NamedPlan::Model(model) if self.value.get_type().is(model.class.bind(py)) => {
                        let depth = self.inner()?;
                        visitor.visit_map(Fields::of(&self.value, model, depth)?)
                    }
                    NamedPlan::Tagged { variants, .. } => {
                        let class = self.value.get_type();
                        let model = variants
                            .values()
                            .find(|model| class.is(model.class.bind(py)))
                            .ok_or(Error(None))?;
                        let depth = self.inner()?;
                        visitor.visit_map(Fields::of(&self.value, model, depth)?)
                    }
                    NamedPlan::External(variants) => {
                        let class = self.value.get_type();
                        let (wire_name, variant) = variants
                            .iter()
                            .find(|(_, variant)| class.is(variant.model.class.bind(py)))
                            .ok_or(Error(None))?;
                        match variant.form {
                            ExternalForm::Unit => visitor.visit_str(wire_name),
                            ExternalForm::Value => {
                                let depth = self.inner()?;
                                let (payload, plan) =
                                    external_value_payload(&self.value, &variant.model)?;
                                visitor.visit_map(SingleValueEntry {
                                    entry: Some((wire_name.as_str(), payload, plan)),
                                    pending: None,
                                    depth,
                                })
                            }
                            ExternalForm::Fields => {
                                let depth = self.inner()?;
                                visitor.visit_map(SingleFieldsEntry {
                                    entry: Some((wire_name.as_str(), self.value, &variant.model)),
                                    pending: None,
                                    depth,
                                })
                            }
                        }
                    }
                    // A member of a string enum is its text.
                    NamedPlan::StringEnum(_) if self.value.is_instance_of::<PyString>() => {
                        let text = self.value.cast::<PyString>().map_err(PyErr::from)?;
                        visitor.visit_str(text.to_str()?)
                    }
                    _ => Err(Error(None)),
                }
            }
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.plan {
            Plan::Plain if self.value.is_none() => visitor.visit_none(),
            Plan::Option(_) => self.deserialize_any(visitor),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match (self.plan, self.value.cast::<PyBool>()) {
            (Plan::Plain, Ok(value)) => visitor.visit_bool(value.is_true()),
            (Plan::Plain, Err(_)) => Err(Error(None)),
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.plan {
            Plan::Plain => self.plain_items(visitor),
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.plan {
            Plan::Plain => {
                let depth = self.inner()?;
                let dict = self.value.cast_into::<PyDict>().map_err(PyErr::from)?;
                visitor.visit_map(Entries {
                    iter: dict.iter(),
                    plan: &Plan::Plain,
                    pending: None,
                    depth,
                })
            }
            _ => self.deserialize_any(visitor),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.plan {
            Plan::Plain => Ok(de::Deserializer::deserialize_enum(
                &mut self.plain(),
                name,
                variants,
                visitor,
            )?),
            Plan::Named(named) => {
                let py = self.value.py();
                match named.plan(py)? {
                    NamedPlan::StringEnum(_) if self.value.is_instance_of::<PyString>() => {
                        Ok(de::Deserializer::deserialize_enum(
                            &mut self.plain(),
                            name,
                            variants,
                            visitor,
                        )?)
                    }
                    NamedPlan::External(external) => {
                        let class = self.value.get_type();
                        let (wire_name, variant) = external
                            .iter()
                            .find(|(_, variant)| class.is(variant.model.class.bind(py)))
                            .ok_or(Error(None))?;
                        let depth = self.inner()?;
                        visitor.visit_enum(ExternalEnumAccess {
                            wire_name: wire_name.as_str(),
                            variant,
                            value: self.value,
                            depth,
                        })
                    }
                    _ => Err(Error(None)),
                }
            }
            _ => Err(Error(None)),
        }
    }

    by_plan! {
        deserialize_i8();
        deserialize_i16();
        deserialize_i32();
        deserialize_i64();
        deserialize_i128();
        deserialize_u8();
        deserialize_u16();
        deserialize_u32();
        deserialize_u64();
        deserialize_u128();
        deserialize_f32();
        deserialize_f64();
        deserialize_char();
        deserialize_str();
        deserialize_string();
        deserialize_bytes();
        deserialize_byte_buf();
        deserialize_unit();
        deserialize_unit_struct(name: &'static str);
        deserialize_struct(name: &'static str, fields: &'static [&'static str]);
        deserialize_identifier();
        deserialize_ignored_any();
    }
}

/// Direct index iteration over a Python `list` or `tuple` without collecting
/// items into an intermediate Rust `Vec`.
enum SeqItems<'py> {
    List {
        list: Bound<'py, PyList>,
        len: usize,
    },
    Tuple {
        tuple: Bound<'py, PyTuple>,
        len: usize,
    },
}

impl<'py> SeqItems<'py> {
    fn of(value: Bound<'py, PyAny>) -> Result<Self, Error> {
        let value = match value.cast_into::<PyList>() {
            Ok(list) => {
                let len = list.len();
                return Ok(Self::List { list, len });
            }
            Err(error) => error.into_inner(),
        };
        if let Ok(tuple) = value.cast_into::<PyTuple>() {
            let len = tuple.len();
            return Ok(Self::Tuple { tuple, len });
        }
        Err(Error(None))
    }

    fn len(&self) -> usize {
        match self {
            Self::List { len, .. } | Self::Tuple { len, .. } => *len,
        }
    }

    fn get(&self, index: usize) -> PyResult<Bound<'py, PyAny>> {
        match self {
            Self::List { list, .. } => list.get_item(index),
            Self::Tuple { tuple, .. } => tuple.get_item(index),
        }
    }
}

/// The plan of each item of a sequence.
enum Plans<'a> {
    Same(&'a Plan),
    Each(&'a [Plan]),
}

struct Items<'a, 'py> {
    items: SeqItems<'py>,
    plans: Plans<'a>,
    index: usize,
    depth: usize,
}

impl<'de> de::SeqAccess<'de> for Items<'_, '_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.index >= self.items.len() {
            return Ok(None);
        }
        let value = self.items.get(self.index)?;
        let plan = match self.plans {
            Plans::Same(plan) => plan,
            Plans::Each(plans) => &plans[self.index],
        };
        self.index += 1;
        seed.deserialize(Reader {
            value,
            plan,
            depth: self.depth,
        })
        .map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.items.len() - self.index)
    }
}

struct Entries<'a, 'py> {
    iter: BoundDictIterator<'py>,
    plan: &'a Plan,
    pending: Option<Bound<'py, PyAny>>,
    depth: usize,
}

impl<'de> de::MapAccess<'de> for Entries<'_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((key, value)) = self.iter.next() else {
            return Ok(None);
        };
        self.pending = Some(value);
        seed.deserialize(Reader {
            value: key,
            plan: &Plan::Plain,
            depth: self.depth,
        })
        .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let value = self.pending.take().ok_or(Error(None))?;
        seed.deserialize(Reader {
            value,
            plan: self.plan,
            depth: self.depth,
        })
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

/// The fields of a model instance, from its `__dict__`, under the names
/// that Serde reads.
struct Fields<'a, 'py> {
    dict: Bound<'py, PyDict>,
    model: &'a ModelPlan,
    index: usize,
    pending: Option<(Bound<'py, PyAny>, &'a Plan)>,
    depth: usize,
}

impl<'a, 'py> Fields<'a, 'py> {
    fn of(instance: &Bound<'py, PyAny>, model: &'a ModelPlan, depth: usize) -> Result<Self, Error> {
        let dict = instance
            .getattr(intern!(instance.py(), "__dict__"))?
            .cast_into::<PyDict>()
            .map_err(PyErr::from)?;
        Ok(Self {
            dict,
            model,
            index: 0,
            pending: None,
            depth,
        })
    }
}

impl<'de> de::MapAccess<'de> for Fields<'_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        while let Some(field) = self.model.fields.get(self.index) {
            self.index += 1;
            if let Some(value) = self.dict.get_item(field.attribute.bind(self.dict.py()))? {
                self.pending = Some((value, &field.plan));
                let name: &str = &field.wire_name;
                return seed.deserialize(name.into_deserializer()).map(Some);
            }
        }
        Ok(None)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let (value, plan) = self.pending.take().ok_or(Error(None))?;
        seed.deserialize(Reader {
            value,
            plan,
            depth: self.depth,
        })
    }
}

/// Read the single payload value and plan of an externally tagged newtype
/// variant model (`ExternalForm::Value`).
fn external_value_payload<'a, 'py>(
    instance: &Bound<'py, PyAny>,
    model: &'a ModelPlan,
) -> Result<(Bound<'py, PyAny>, &'a Plan), Error> {
    let field = model.fields.first().ok_or(Error(None))?;
    let dict = instance
        .getattr(intern!(instance.py(), "__dict__"))?
        .cast_into::<PyDict>()
        .map_err(PyErr::from)?;
    let value = dict
        .get_item(field.attribute.bind(dict.py()))?
        .ok_or(Error(None))?;
    Ok((value, &field.plan))
}

/// Direct Serde `EnumAccess` over an exact generated `ExternalVariant` object.
struct ExternalEnumAccess<'a, 'py> {
    wire_name: &'a str,
    variant: &'a ExternalVariant,
    value: Bound<'py, PyAny>,
    depth: usize,
}

impl<'de, 'a, 'py> de::EnumAccess<'de> for ExternalEnumAccess<'a, 'py> {
    type Error = Error;
    type Variant = ExternalVariantAccess<'a, 'py>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Error> {
        let name: de::value::StrDeserializer<'_, Error> = self.wire_name.into_deserializer();
        let variant = seed.deserialize(name)?;
        Ok((
            variant,
            ExternalVariantAccess {
                variant: self.variant,
                value: self.value,
                depth: self.depth,
            },
        ))
    }
}

struct ExternalVariantAccess<'a, 'py> {
    variant: &'a ExternalVariant,
    value: Bound<'py, PyAny>,
    depth: usize,
}

impl<'de> de::VariantAccess<'de> for ExternalVariantAccess<'_, '_> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        if self.variant.form == ExternalForm::Unit {
            Ok(())
        } else {
            Err(Error(None))
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        if self.variant.form != ExternalForm::Value {
            return Err(Error(None));
        }
        let (value, plan) = external_value_payload(&self.value, &self.variant.model)?;
        seed.deserialize(Reader {
            value,
            plan,
            depth: self.depth,
        })
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        _length: usize,
        _visitor: V,
    ) -> Result<V::Value, Error> {
        Err(Error(None))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        if self.variant.form != ExternalForm::Fields {
            return Err(Error(None));
        }
        visitor.visit_map(Fields::of(&self.value, &self.variant.model, self.depth)?)
    }
}

/// Single-entry `{wire_name: payload}` map for an `ExternalForm::Value` variant
/// read through `deserialize_any` (such as inside an internally tagged enum).
struct SingleValueEntry<'a, 'py> {
    entry: Option<(&'a str, Bound<'py, PyAny>, &'a Plan)>,
    pending: Option<(Bound<'py, PyAny>, &'a Plan)>,
    depth: usize,
}

impl<'de> de::MapAccess<'de> for SingleValueEntry<'_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((name, value, plan)) = self.entry.take() else {
            return Ok(None);
        };
        self.pending = Some((value, plan));
        let key: de::value::StrDeserializer<'_, Error> = name.into_deserializer();
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let (value, plan) = self.pending.take().ok_or(Error(None))?;
        seed.deserialize(Reader {
            value,
            plan,
            depth: self.depth,
        })
    }
}

/// Single-entry `{wire_name: {fields...}}` map for an `ExternalForm::Fields`
/// variant read through `deserialize_any`.
struct SingleFieldsEntry<'a, 'py> {
    entry: Option<(&'a str, Bound<'py, PyAny>, &'a ModelPlan)>,
    pending: Option<(Bound<'py, PyAny>, &'a ModelPlan)>,
    depth: usize,
}

impl<'de> de::MapAccess<'de> for SingleFieldsEntry<'_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((name, value, model)) = self.entry.take() else {
            return Ok(None);
        };
        self.pending = Some((value, model));
        let key: de::value::StrDeserializer<'_, Error> = name.into_deserializer();
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let (value, model) = self.pending.take().ok_or(Error(None))?;
        seed.deserialize(ModelFieldsDeserializer {
            value,
            model,
            depth: self.depth,
        })
    }
}

/// Deserializes a struct-variant model's fields map for [`SingleFieldsEntry`].
struct ModelFieldsDeserializer<'a, 'py> {
    value: Bound<'py, PyAny>,
    model: &'a ModelPlan,
    depth: usize,
}

impl<'de> de::Deserializer<'de> for ModelFieldsDeserializer<'_, '_> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_map(Fields::of(&self.value, self.model, self.depth)?)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

/// The marker around an argument that Pydantic dumped, which the extension
/// reads as `pythonize` does.
#[pyclass(frozen, module = "bimetal", name = "Dumped", skip_from_py_object)]
pub(crate) struct Dumped(pub(crate) Py<PyAny>);

#[pymethods]
impl Dumped {
    #[new]
    fn new(value: Py<PyAny>) -> Self {
        Self(value)
    }
}

/// Register the marker and the exception that asks for it on `module`.
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Dumped>()?;
    module.add("Revalidate", module.py().get_type::<Revalidate>())
}

pyo3::create_exception!(
    bimetal,
    Revalidate,
    pyo3::exceptions::PyException,
    "An argument that the extension cannot read as it is: the generated code calls again with Pydantic's dump."
);
