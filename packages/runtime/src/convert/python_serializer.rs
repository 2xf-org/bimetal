//! A Serde serializer that makes the Python value of a Rust result by its
//! [`Plan`]: plain values as `pythonize` makes them, and models, string
//! enums, date-times, dates and durations as Pydantic would validate them.

use std::fmt::{self, Display, Write as _};

use pyo3::IntoPyObjectExt;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};
use serde::ser::{self, Serialize};

use super::python_plan::{ModelPlan, NamedPlan, Plan};
use super::python_time;
use super::short_text::ShortText;

/// Make the Python value of `value` by `plan`.
///
/// # Errors
///
/// Returns `ValueError` when Serde writes the value in another shape than
/// its contract type has, or a Python error from making it.
pub(crate) fn to_python<'py, T: Serialize + ?Sized>(
    py: Python<'py>,
    plan: &Plan,
    value: &T,
) -> PyResult<Bound<'py, PyAny>> {
    match plan {
        Plan::Plain => pythonize::pythonize(py, value)
            .map_err(|error| PyValueError::new_err(error.to_string())),
        plan => value
            .serialize(Serializer { py, plan })
            .map_err(|Error(error)| error),
    }
}

/// A failure while serializing, which carries the Python error.
#[derive(Debug)]
pub(crate) struct Error(PyErr);

impl Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: Display>(message: T) -> Self {
        Self(PyValueError::new_err(message.to_string()))
    }
}

impl From<PyErr> for Error {
    fn from(error: PyErr) -> Self {
        Self(error)
    }
}

type Result<T> = std::result::Result<T, Error>;

/// A failure with `message`, as a `ValueError`.
fn custom(message: impl Display) -> Error {
    <Error as ser::Error>::custom(message)
}

/// Serializes one value by one plan.
struct Serializer<'a, 'py> {
    py: Python<'py>,
    plan: &'a Plan,
}

impl<'a, 'py> Serializer<'a, 'py> {
    /// Report a value that Serde writes as `wrote` where the plan expects
    /// another shape: a contract type that its `Serialize` does not match.
    #[cold]
    fn mismatch(&self, wrote: &str) -> Error {
        Error(PyValueError::new_err(format!(
            "Rust wrote {wrote} where its contract type expects {}",
            describe(self.plan)
        )))
    }

    /// The model, string enum or enum with data that the plan names.
    fn named(&self) -> Result<Option<&'a NamedPlan>> {
        Ok(match self.plan {
            Plan::Named(named) => Some(named.plan(self.py)?),
            _ => None,
        })
    }
}

/// Describe a plan for an error.
fn describe(plan: &Plan) -> &'static str {
    match plan {
        Plan::Plain => "a plain value",
        Plan::DateTime => "a date-time",
        Plan::Date => "a date",
        Plan::Duration => "a duration",
        Plan::Bytes => "bytes",
        Plan::Option(_) => "an option",
        Plan::List(_) => "a list",
        Plan::Tuple(_) => "a tuple",
        Plan::Map(_) => "a map",
        Plan::Named(_) => "a model or an enum",
    }
}

impl<'a, 'py> ser::Serializer for Serializer<'a, 'py> {
    type Ok = Bound<'py, PyAny>;
    type Error = Error;
    type SerializeSeq = Items<'a, 'py>;
    type SerializeTuple = Items<'a, 'py>;
    type SerializeTupleStruct = Items<'a, 'py>;
    type SerializeTupleVariant = ser::Impossible<Self::Ok, Error>;
    type SerializeMap = Entries<'a, 'py>;
    type SerializeStruct = Fields<'a, 'py>;
    type SerializeStructVariant = Fields<'a, 'py>;

    fn serialize_bool(self, _: bool) -> Result<Self::Ok> {
        Err(self.mismatch("a bool"))
    }
    fn serialize_i8(self, _: i8) -> Result<Self::Ok> {
        Err(self.mismatch("an integer"))
    }
    fn serialize_i16(self, _: i16) -> Result<Self::Ok> {
        Err(self.mismatch("an integer"))
    }
    fn serialize_i32(self, _: i32) -> Result<Self::Ok> {
        Err(self.mismatch("an integer"))
    }
    fn serialize_i64(self, _: i64) -> Result<Self::Ok> {
        Err(self.mismatch("an integer"))
    }
    fn serialize_u8(self, _: u8) -> Result<Self::Ok> {
        Err(self.mismatch("an integer"))
    }
    fn serialize_u16(self, _: u16) -> Result<Self::Ok> {
        Err(self.mismatch("an integer"))
    }
    fn serialize_u32(self, _: u32) -> Result<Self::Ok> {
        Err(self.mismatch("an integer"))
    }
    fn serialize_u64(self, _: u64) -> Result<Self::Ok> {
        Err(self.mismatch("an integer"))
    }
    fn serialize_f32(self, _: f32) -> Result<Self::Ok> {
        Err(self.mismatch("a float"))
    }
    fn serialize_f64(self, _: f64) -> Result<Self::Ok> {
        Err(self.mismatch("a float"))
    }
    fn serialize_char(self, _: char) -> Result<Self::Ok> {
        Err(self.mismatch("a character"))
    }

    fn serialize_str(self, text: &str) -> Result<Self::Ok> {
        match self.plan {
            Plan::DateTime => Ok(python_time::date_time(self.py, text)?),
            Plan::Date => Ok(python_time::date(self.py, text)?),
            _ => match self.named()? {
                Some(NamedPlan::StringEnum(members)) => member(self.py, members, text),
                _ => Err(self.mismatch("text")),
            },
        }
    }

    /// Write text that fits on the stack there, such as a date-time's, and
    /// longer text to the heap.
    fn collect_str<T: ?Sized + Display>(self, value: &T) -> Result<Self::Ok> {
        let mut text = ShortText::new();
        if write!(text, "{value}").is_ok() {
            return self.serialize_str(text.as_str());
        }
        self.serialize_str(&value.to_string())
    }

    fn serialize_bytes(self, bytes: &[u8]) -> Result<Self::Ok> {
        match self.plan {
            Plan::Bytes => Ok(PyBytes::new(self.py, bytes).into_any()),
            _ => Err(self.mismatch("bytes")),
        }
    }

    fn serialize_none(self) -> Result<Self::Ok> {
        match self.plan {
            Plan::Option(_) => Ok(self.py.None().into_bound(self.py)),
            _ => Err(self.mismatch("nothing")),
        }
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Self::Ok> {
        match self.plan {
            Plan::Option(item) => Ok(to_python(self.py, item, value)?),
            _ => Err(self.mismatch("an option")),
        }
    }

    fn serialize_unit(self) -> Result<Self::Ok> {
        Err(self.mismatch("a unit"))
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<Self::Ok> {
        Err(self.mismatch("a unit"))
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Self::Ok> {
        match self.named()? {
            Some(NamedPlan::StringEnum(members)) => member(self.py, members, variant),
            Some(NamedPlan::External(variants)) => {
                Ok(model(variants, variant)?.instance(self.py, Vec::new())?)
            }
            _ => Err(self.mismatch("a unit variant")),
        }
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Self::Ok> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok> {
        let Some(NamedPlan::External(variants)) = self.named()? else {
            return Err(self.mismatch("a variant with a value"));
        };
        let model = model(variants, variant)?;
        let mut values = vec![None; model.fields.len()];
        let index = model
            .field("value", 0)
            .ok_or_else(|| self.mismatch("a variant with a value"))?;
        values[index] = Some(to_python(self.py, &model.fields[index].plan, value)?);
        Ok(model.instance(self.py, values)?)
    }

    fn serialize_seq(self, length: Option<usize>) -> Result<Items<'a, 'py>> {
        Items::new(self, length.unwrap_or(0))
    }

    fn serialize_tuple(self, length: usize) -> Result<Items<'a, 'py>> {
        Items::new(self, length)
    }

    fn serialize_tuple_struct(self, _: &'static str, length: usize) -> Result<Items<'a, 'py>> {
        Items::new(self, length)
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        Err(self.mismatch("a tuple variant"))
    }

    fn serialize_map(self, length: Option<usize>) -> Result<Entries<'a, 'py>> {
        match self.plan {
            Plan::Map(value) => Ok(Entries {
                py: self.py,
                plan: value,
                dict: PyDict::new(self.py),
                key: None,
                _length: length,
            }),
            _ => Err(self.mismatch("a map")),
        }
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Fields<'a, 'py>> {
        match self.plan {
            Plan::Duration => return Ok(Fields::duration(self.py)),
            Plan::Named(_) => {}
            _ => return Err(self.mismatch("a struct")),
        }
        match self.named()? {
            Some(NamedPlan::Model(model)) => Ok(Fields::model(self.py, model)),
            Some(NamedPlan::Tagged { tag, variants }) => Ok(Fields::tagged(self.py, tag, variants)),
            _ => Err(self.mismatch("a struct")),
        }
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Fields<'a, 'py>> {
        match self.named()? {
            Some(NamedPlan::External(variants)) => {
                Ok(Fields::model(self.py, model(variants, variant)?))
            }
            _ => Err(self.mismatch("a struct variant")),
        }
    }
}

/// The member of a string enum that Serde writes as `text`.
fn member<'py>(
    py: Python<'py>,
    members: &std::collections::HashMap<String, Py<PyAny>>,
    text: &str,
) -> Result<Bound<'py, PyAny>> {
    members
        .get(text)
        .map(|member| member.bind(py).clone())
        .ok_or_else(|| custom(format!("`{text}` is no member of its enum")))
}

/// The model of the variant `variant` of an enum with data.
fn model<'a>(
    variants: &'a std::collections::HashMap<String, ModelPlan>,
    variant: &str,
) -> Result<&'a ModelPlan> {
    variants
        .get(variant)
        .ok_or_else(|| custom(format!("`{variant}` is no variant of its enum")))
}

/// The items of a list, a tuple or bytes.
pub(crate) struct Items<'a, 'py> {
    py: Python<'py>,
    plan: &'a Plan,
    items: Vec<Bound<'py, PyAny>>,
}

impl<'a, 'py> Items<'a, 'py> {
    fn new(serializer: Serializer<'a, 'py>, length: usize) -> Result<Self> {
        match serializer.plan {
            Plan::List(_) | Plan::Tuple(_) | Plan::Bytes => Ok(Self {
                py: serializer.py,
                plan: serializer.plan,
                items: Vec::with_capacity(length),
            }),
            _ => Err(serializer.mismatch("a sequence")),
        }
    }

    fn push<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        let plan = match self.plan {
            Plan::List(item) => item,
            Plan::Tuple(items) => items
                .get(self.items.len())
                .ok_or_else(|| custom("a tuple has more items than its type"))?,
            _ => &Plan::Plain,
        };
        self.items.push(to_python(self.py, plan, value)?);
        Ok(())
    }

    fn finish(self) -> Result<Bound<'py, PyAny>> {
        Ok(match self.plan {
            Plan::Tuple(_) => PyTuple::new(self.py, self.items)?.into_any(),
            Plan::Bytes => {
                let bytes = self
                    .items
                    .iter()
                    .map(|item| item.extract::<u8>())
                    .collect::<PyResult<Vec<u8>>>()?;
                PyBytes::new(self.py, &bytes).into_any()
            }
            _ => PyList::new(self.py, self.items)?.into_any(),
        })
    }
}

impl<'py> ser::SerializeSeq for Items<'_, 'py> {
    type Ok = Bound<'py, PyAny>;
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok> {
        self.finish()
    }
}

impl<'py> ser::SerializeTuple for Items<'_, 'py> {
    type Ok = Bound<'py, PyAny>;
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok> {
        self.finish()
    }
}

impl<'py> ser::SerializeTupleStruct for Items<'_, 'py> {
    type Ok = Bound<'py, PyAny>;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok> {
        self.finish()
    }
}

/// The entries of a string-keyed map.
pub(crate) struct Entries<'a, 'py> {
    py: Python<'py>,
    plan: &'a Plan,
    dict: Bound<'py, PyDict>,
    key: Option<Bound<'py, PyAny>>,
    _length: Option<usize>,
}

impl<'py> ser::SerializeMap for Entries<'_, 'py> {
    type Ok = Bound<'py, PyAny>;
    type Error = Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
        self.key = Some(to_python(self.py, &Plan::Plain, key)?);
        Ok(())
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        let key = self
            .key
            .take()
            .ok_or_else(|| custom("a map's value came before its key"))?;
        self.dict
            .set_item(key, to_python(self.py, self.plan, value)?)?;
        Ok(())
    }

    fn end(self) -> Result<Self::Ok> {
        Ok(self.dict.into_any())
    }
}

/// The fields of a model, of a variant of a tagged enum, which the first
/// field's tag picks, or of a duration.
pub(crate) enum Fields<'a, 'py> {
    Model {
        py: Python<'py>,
        model: &'a ModelPlan,
        values: Vec<Option<Bound<'py, PyAny>>>,
        /// The field after the last that Serde wrote, which is where the
        /// next is looked for first.
        next: usize,
    },
    Tagged {
        py: Python<'py>,
        tag: &'a str,
        variants: &'a std::collections::HashMap<String, ModelPlan>,
    },
    Duration {
        py: Python<'py>,
        nanoseconds: u128,
    },
}

impl<'a, 'py> Fields<'a, 'py> {
    fn model(py: Python<'py>, model: &'a ModelPlan) -> Self {
        Self::Model {
            py,
            model,
            values: vec![None; model.fields.len()],
            next: 0,
        }
    }

    fn tagged(
        py: Python<'py>,
        tag: &'a str,
        variants: &'a std::collections::HashMap<String, ModelPlan>,
    ) -> Self {
        Self::Tagged { py, tag, variants }
    }

    fn duration(py: Python<'py>) -> Self {
        Self::Duration { py, nanoseconds: 0 }
    }

    fn field<T: ?Sized + Serialize>(&mut self, key: &'static str, value: &T) -> Result<()> {
        match self {
            Self::Model {
                py,
                model,
                values,
                next,
            } => {
                let index = model
                    .field(key, *next)
                    .ok_or_else(|| custom(format!("`{key}` is no field of its model")))?;
                values[index] = Some(to_python(*py, &model.fields[index].plan, value)?);
                *next = index + 1;
            }
            Self::Tagged { py, tag, variants } => {
                if key != *tag {
                    return Err(custom(format!(
                        "a tagged enum writes its tag `{tag}` before `{key}`"
                    )));
                }
                let name: String = pythonize::pythonize(*py, value)
                    .map_err(custom)?
                    .extract()?;
                let variant = model(variants, &name)?;
                let mut fields = Self::model(*py, variant);
                fields.field(key, value)?;
                *self = fields;
            }
            Self::Duration { py, nanoseconds } => {
                let part: u128 = pythonize::pythonize(*py, value)
                    .map_err(custom)?
                    .extract()?;
                match key {
                    "secs" => *nanoseconds += part * 1_000_000_000,
                    "nanos" => *nanoseconds += part,
                    _ => return Err(custom(format!("`{key}` is no part of a duration"))),
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Bound<'py, PyAny>> {
        match self {
            Self::Model {
                py, model, values, ..
            } => Ok(model.instance(py, values)?),
            Self::Tagged { .. } => Err(custom("a tagged enum wrote no tag")),
            Self::Duration { py, nanoseconds } => Ok(nanoseconds.into_bound_py_any(py)?),
        }
    }
}

impl<'py> ser::SerializeStruct for Fields<'_, 'py> {
    type Ok = Bound<'py, PyAny>;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.field(key, value)
    }

    fn end(self) -> Result<Self::Ok> {
        self.finish()
    }
}

impl<'py> ser::SerializeStructVariant for Fields<'_, 'py> {
    type Ok = Bound<'py, PyAny>;
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.field(key, value)
    }

    fn end(self) -> Result<Self::Ok> {
        self.finish()
    }
}
