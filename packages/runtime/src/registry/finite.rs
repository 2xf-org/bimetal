//! Find a NaN or an infinity anywhere in a value. JSON has neither, so Serde
//! writes `null` for both, which inside an `Option` reads back as `None`.

use serde::Serialize;
use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant,
};

/// Return whether `value`, as Serde writes it, holds a float that is a NaN
/// or an infinity.
pub(super) fn has_non_finite_float(value: &impl Serialize) -> bool {
    matches!(value.serialize(Finite), Err(Stop::NonFinite))
}

/// Why the walk stopped before the end of the value.
#[derive(Debug, thiserror::Error)]
enum Stop {
    /// A float that is a NaN or an infinity.
    #[error("a NaN or an infinity")]
    NonFinite,
    /// The value's own `Serialize` failed, which the JSON reports.
    #[error("the value could not be serialized")]
    Failed,
}

impl ser::Error for Stop {
    fn custom<T: std::fmt::Display>(_message: T) -> Self {
        Self::Failed
    }
}

/// A serializer that writes nothing and stops at the first non-finite float.
#[derive(Clone, Copy)]
struct Finite;

impl Finite {
    fn float(value: f64) -> Result<(), Stop> {
        if value.is_finite() {
            Ok(())
        } else {
            Err(Stop::NonFinite)
        }
    }
}

impl ser::Serializer for Finite {
    type Ok = ();
    type Error = Stop;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, _value: bool) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_i8(self, _value: i8) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_i16(self, _value: i16) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_i32(self, _value: i32) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_i64(self, _value: i64) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_u8(self, _value: u8) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_u16(self, _value: u16) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_u32(self, _value: u32) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_u64(self, _value: u64) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_f32(self, value: f32) -> Result<(), Stop> {
        Self::float(value.into())
    }

    fn serialize_f64(self, value: f64) -> Result<(), Stop> {
        Self::float(value)
    }

    fn serialize_char(self, _value: char) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_str(self, _value: &str) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_none(self) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), Stop> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
    ) -> Result<(), Stop> {
        Ok(())
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Stop> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<(), Stop> {
        value.serialize(self)
    }

    fn serialize_seq(self, _length: Option<usize>) -> Result<Self, Stop> {
        Ok(self)
    }

    fn serialize_tuple(self, _length: usize) -> Result<Self, Stop> {
        Ok(self)
    }

    fn serialize_tuple_struct(self, _name: &'static str, _length: usize) -> Result<Self, Stop> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self, Stop> {
        Ok(self)
    }

    fn serialize_map(self, _length: Option<usize>) -> Result<Self, Stop> {
        Ok(self)
    }

    fn serialize_struct(self, _name: &'static str, _length: usize) -> Result<Self, Stop> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self, Stop> {
        Ok(self)
    }
}

impl SerializeSeq for Finite {
    type Ok = ();
    type Error = Stop;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Stop> {
        value.serialize(*self)
    }

    fn end(self) -> Result<(), Stop> {
        Ok(())
    }
}

impl SerializeTuple for Finite {
    type Ok = ();
    type Error = Stop;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Stop> {
        value.serialize(*self)
    }

    fn end(self) -> Result<(), Stop> {
        Ok(())
    }
}

impl SerializeTupleStruct for Finite {
    type Ok = ();
    type Error = Stop;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Stop> {
        value.serialize(*self)
    }

    fn end(self) -> Result<(), Stop> {
        Ok(())
    }
}

impl SerializeTupleVariant for Finite {
    type Ok = ();
    type Error = Stop;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Stop> {
        value.serialize(*self)
    }

    fn end(self) -> Result<(), Stop> {
        Ok(())
    }
}

impl SerializeMap for Finite {
    type Ok = ();
    type Error = Stop;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Stop> {
        key.serialize(*self)
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Stop> {
        value.serialize(*self)
    }

    fn end(self) -> Result<(), Stop> {
        Ok(())
    }
}

impl SerializeStruct for Finite {
    type Ok = ();
    type Error = Stop;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), Stop> {
        value.serialize(*self)
    }

    fn end(self) -> Result<(), Stop> {
        Ok(())
    }
}

impl SerializeStructVariant for Finite {
    type Ok = ();
    type Error = Stop;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), Stop> {
        value.serialize(*self)
    }

    fn end(self) -> Result<(), Stop> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn a_non_finite_float_is_found_at_any_depth() {
        assert!(has_non_finite_float(&f64::NAN));
        assert!(has_non_finite_float(&Some(f32::INFINITY)));
        assert!(has_non_finite_float(&Some(vec![1.0, f64::NEG_INFINITY])));
        assert!(has_non_finite_float(&BTreeMap::from([(
            "share",
            Some(f64::NAN)
        )])));
        assert!(has_non_finite_float(&(1, Some(Some(f64::NAN)))));
    }

    #[test]
    fn finite_floats_and_other_values_pass() {
        assert!(!has_non_finite_float(&Some(0.5)));
        assert!(!has_non_finite_float(&None::<f64>));
        assert!(!has_non_finite_float(&vec![f64::MAX, f64::MIN_POSITIVE]));
        assert!(!has_non_finite_float(&("text", 'c', true, ())));
    }
}
