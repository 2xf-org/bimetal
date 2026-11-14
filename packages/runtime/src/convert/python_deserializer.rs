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
use pyo3::types::{PyBool, PyBytes, PyDict, PyList, PyString, PyTuple};
use pythonize::Depythonizer;
use serde::de::{self, DeserializeOwned, DeserializeSeed, IntoDeserializer, Visitor};

use super::python_plan::{ModelPlan, NamedPlan, Plan};
use super::python_time;

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
}

impl<'a, 'py> Reader<'a, 'py> {
    /// The model plan that the plan names, when `value` is an instance of
    /// exactly its class.
    fn instance(&self) -> Result<&'a ModelPlan, Error> {
        let Plan::Named(named) = self.plan else {
            return Err(Error(None));
        };
        match named.plan(self.value.py())? {
            NamedPlan::Model(model)
                if self.value.get_type().is(model.class.bind(self.value.py())) =>
            {
                Ok(model)
            }
            _ => Err(Error(None)),
        }
    }

    /// Read a value whose plan is plain as `pythonize` reads it.
    fn plain(&self) -> Depythonizer<'_, 'py> {
        Depythonizer::from_object(&self.value)
    }

    /// Read the items of a list or a tuple whose plan is plain, each by the
    /// plain plan.
    fn plain_items<'de, V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let items = sequence(&self.value)?;
        visitor.visit_seq(Items {
            items: items.into_iter(),
            plans: Plans::Same(&Plan::Plain),
            index: 0,
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
                    })
                }
            }
            Plan::List(item) => {
                let items = sequence(&self.value)?;
                visitor.visit_seq(Items {
                    items: items.into_iter(),
                    plans: Plans::Same(item),
                    index: 0,
                })
            }
            Plan::Tuple(plans) => {
                let items = sequence(&self.value)?;
                if items.len() != plans.len() {
                    return Err(Error(None));
                }
                visitor.visit_seq(Items {
                    items: items.into_iter(),
                    plans: Plans::Each(plans),
                    index: 0,
                })
            }
            Plan::Map(value) => {
                let dict = self.value.cast::<PyDict>().map_err(PyErr::from)?;
                visitor.visit_map(Entries {
                    entries: dict.iter().collect::<Vec<_>>().into_iter(),
                    plan: value,
                    pending: None,
                })
            }
            Plan::Named(named) => match named.plan(self.value.py())? {
                NamedPlan::Model(_) => {
                    let model = self.instance()?;
                    visitor.visit_map(Fields::of(&self.value, model)?)
                }
                // A member of a string enum is its text.
                NamedPlan::StringEnum(_) if self.value.is_instance_of::<PyString>() => Ok(
                    de::Deserializer::deserialize_any(&mut self.plain(), visitor)?,
                ),
                _ => Err(Error(None)),
            },
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
                let dict = self.value.cast::<PyDict>().map_err(PyErr::from)?;
                visitor.visit_map(Entries {
                    entries: dict.iter().collect::<Vec<_>>().into_iter(),
                    plan: &Plan::Plain,
                    pending: None,
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
        let plain = match self.plan {
            Plan::Plain => true,
            Plan::Named(named) => {
                matches!(named.plan(self.value.py())?, NamedPlan::StringEnum(_))
                    && self.value.is_instance_of::<PyString>()
            }
            _ => false,
        };
        if !plain {
            return Err(Error(None));
        }
        Ok(de::Deserializer::deserialize_enum(
            &mut self.plain(),
            name,
            variants,
            visitor,
        )?)
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

/// The items of a list or a tuple.
fn sequence<'py>(value: &Bound<'py, PyAny>) -> Result<Vec<Bound<'py, PyAny>>, Error> {
    if let Ok(list) = value.cast::<PyList>() {
        return Ok(list.iter().collect());
    }
    if let Ok(tuple) = value.cast::<PyTuple>() {
        return Ok(tuple.iter().collect());
    }
    Err(Error(None))
}

/// The plan of each item of a sequence.
enum Plans<'a> {
    Same(&'a Plan),
    Each(&'a [Plan]),
}

struct Items<'a, 'py> {
    items: std::vec::IntoIter<Bound<'py, PyAny>>,
    plans: Plans<'a>,
    index: usize,
}

impl<'de> de::SeqAccess<'de> for Items<'_, '_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        let Some(value) = self.items.next() else {
            return Ok(None);
        };
        let plan = match self.plans {
            Plans::Same(plan) => plan,
            Plans::Each(plans) => &plans[self.index],
        };
        self.index += 1;
        seed.deserialize(Reader { value, plan }).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.items.len())
    }
}

struct Entries<'a, 'py> {
    entries: std::vec::IntoIter<(Bound<'py, PyAny>, Bound<'py, PyAny>)>,
    plan: &'a Plan,
    pending: Option<Bound<'py, PyAny>>,
}

impl<'de> de::MapAccess<'de> for Entries<'_, '_> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((key, value)) = self.entries.next() else {
            return Ok(None);
        };
        self.pending = Some(value);
        seed.deserialize(Reader {
            value: key,
            plan: &Plan::Plain,
        })
        .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let value = self.pending.take().ok_or(Error(None))?;
        seed.deserialize(Reader {
            value,
            plan: self.plan,
        })
    }
}

/// The fields of a model instance, from its `__dict__`, under the names
/// that Serde reads.
struct Fields<'a, 'py> {
    dict: Bound<'py, PyDict>,
    model: &'a ModelPlan,
    index: usize,
    pending: Option<(Bound<'py, PyAny>, &'a Plan)>,
}

impl<'a, 'py> Fields<'a, 'py> {
    fn of(instance: &Bound<'py, PyAny>, model: &'a ModelPlan) -> Result<Self, Error> {
        let dict = instance
            .getattr(intern!(instance.py(), "__dict__"))?
            .cast_into::<PyDict>()
            .map_err(PyErr::from)?;
        Ok(Self {
            dict,
            model,
            index: 0,
            pending: None,
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
        seed.deserialize(Reader { value, plan })
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
