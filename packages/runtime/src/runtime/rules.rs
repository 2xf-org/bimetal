//! The field rules that `#[bimetal(...)]` declares on model fields.
//!
//! Rust checks them on every value that crosses a host boundary, in both
//! directions, so that every host enforces the same rules: each checks an
//! argument through [`checked`] and a result through [`checked_return`].
//! `#[derive(Model)]` calls [`length`], [`range`], and [`literal`] for the
//! rules that a field declares. The generated Pydantic models also check them
//! in Python, where a model is built.

use std::fmt::{self, Display, Formatter};

use crate::ContractType;
use crate::runtime::ValueError;

/// Return `value` once it keeps the field rules inside it, as every host
/// checks an argument.
///
/// # Errors
///
/// Returns the first rule that the value breaks.
pub fn checked<T: ContractType>(value: T) -> Result<T, ValueError> {
    value.check_rules()?;
    Ok(value)
}

/// Check the field rules inside what `callable` returned, as every host
/// checks a result.
///
/// # Errors
///
/// Returns the message of the first rule that the value breaks, which is a
/// bug of the Rust code rather than of the caller.
pub fn checked_return<T: ContractType + ?Sized>(value: &T, callable: &str) -> Result<(), String> {
    value.check_rules().map_err(|violation| {
        format!("{callable}() returned a value that breaks a field rule: {violation}")
    })
}

/// A field value as the rules see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleValue<'a> {
    /// A boolean.
    Bool(bool),
    /// An integer of any width.
    Integer(i128),
    /// A string, whose length counts Unicode code points, as Python's `len`
    /// does.
    String(&'a str),
    /// A list of this many items.
    List(usize),
}

/// A field type that rules can apply to: a boolean, an integer, a string, a
/// list, or an `Option` of one.
#[diagnostic::on_unimplemented(
    message = "field rules do not apply to `{Self}`",
    note = "`literal` applies to booleans, integers, and strings, `min_length` and `max_length` to `String` and `Vec`, and `ge` and `le` to integers, also inside an `Option`"
)]
pub trait RuleTarget {
    /// Return the value as the rules see it, or `None` for an absent
    /// optional value, to which no rule applies.
    fn rule_value(&self) -> Option<RuleValue<'_>>;
}

/// Check that a string has between `minimum` and `maximum` characters, or a
/// list between `minimum` and `maximum` items.
///
/// # Errors
///
/// Returns the limit that `value` breaks.
pub fn length(
    value: &impl RuleTarget,
    minimum: Option<u64>,
    maximum: Option<u64>,
) -> Result<(), ValueError> {
    let (length, unit) = match value.rule_value() {
        Some(RuleValue::String(text)) => (text.chars().count(), "character"),
        Some(RuleValue::List(length)) => (length, "item"),
        _ => return Ok(()),
    };
    // A `usize` always fits in a `u64` on the targets Bimetal supports.
    let length = length as u64;
    if let Some(minimum) = minimum
        && length < minimum
    {
        return Err(ValueError::new(format!(
            "must have at least {}, got {length}",
            counted(minimum, unit)
        )));
    }
    if let Some(maximum) = maximum
        && length > maximum
    {
        return Err(ValueError::new(format!(
            "must have at most {}, got {length}",
            counted(maximum, unit)
        )));
    }
    Ok(())
}

/// Check that an integer is at least `minimum` and at most `maximum`.
///
/// # Errors
///
/// Returns the bound that `value` breaks.
pub fn range(
    value: &impl RuleTarget,
    minimum: Option<i64>,
    maximum: Option<i64>,
) -> Result<(), ValueError> {
    let Some(RuleValue::Integer(integer)) = value.rule_value() else {
        return Ok(());
    };
    if let Some(minimum) = minimum
        && integer < i128::from(minimum)
    {
        return Err(ValueError::new(format!(
            "must be at least {minimum}, got {integer}"
        )));
    }
    if let Some(maximum) = maximum
        && integer > i128::from(maximum)
    {
        return Err(ValueError::new(format!(
            "must be at most {maximum}, got {integer}"
        )));
    }
    Ok(())
}

/// Check that a value is `expected`, the one value its field may hold.
///
/// # Errors
///
/// Returns what `value` is instead.
pub fn literal(value: &impl RuleTarget, expected: RuleValue<'static>) -> Result<(), ValueError> {
    match value.rule_value() {
        None => Ok(()),
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(ValueError::new(format!("must be {expected}, got {actual}"))),
    }
}

impl Display for RuleValue<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::String(value) => write!(formatter, "{value:?}"),
            Self::List(length) => {
                write!(formatter, "a list of {}", counted(*length as u64, "item"))
            }
        }
    }
}

/// Write `count` with `unit`, plural unless the count is one.
fn counted(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

impl RuleTarget for bool {
    fn rule_value(&self) -> Option<RuleValue<'_>> {
        Some(RuleValue::Bool(*self))
    }
}

macro_rules! integer {
    ($($type:ty),+) => {
        $(
            impl RuleTarget for $type {
                fn rule_value(&self) -> Option<RuleValue<'_>> {
                    Some(RuleValue::Integer(i128::from(*self)))
                }
            }
        )+
    };
}

integer!(u8, u16, u32, u64, i8, i16, i32, i64);

impl RuleTarget for String {
    fn rule_value(&self) -> Option<RuleValue<'_>> {
        Some(RuleValue::String(self))
    }
}

impl<T> RuleTarget for Vec<T> {
    fn rule_value(&self) -> Option<RuleValue<'_>> {
        Some(RuleValue::List(self.len()))
    }
}

impl<T: RuleTarget> RuleTarget for Option<T> {
    fn rule_value(&self) -> Option<RuleValue<'_>> {
        self.as_ref().and_then(RuleTarget::rule_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_lengths_count_code_points() {
        assert_eq!(length(&"äöü".to_owned(), Some(1), Some(3)), Ok(()));
        assert_eq!(
            length(&"abcd".to_owned(), Some(1), Some(3))
                .unwrap_err()
                .in_field("label")
                .to_string(),
            "`label` must have at most 3 characters, got 4"
        );
        assert_eq!(
            length(&String::new(), Some(1), None)
                .unwrap_err()
                .in_field("label")
                .to_string(),
            "`label` must have at least 1 character, got 0"
        );
    }

    #[test]
    fn list_lengths_count_items() {
        assert_eq!(
            length(&vec![0_u8; 5], None, Some(3))
                .unwrap_err()
                .in_field("tags")
                .to_string(),
            "`tags` must have at most 3 items, got 5"
        );
    }

    #[test]
    fn integer_bounds_are_inclusive() {
        assert_eq!(range(&1_u8, Some(1), Some(5)), Ok(()));
        assert_eq!(range(&5_u8, Some(1), Some(5)), Ok(()));
        assert_eq!(
            range(&0_u8, Some(1), Some(5))
                .unwrap_err()
                .in_field("stars")
                .to_string(),
            "`stars` must be at least 1, got 0"
        );
        assert_eq!(
            range(&u64::MAX, None, Some(5))
                .unwrap_err()
                .in_field("stars")
                .to_string(),
            "`stars` must be at most 5, got 18446744073709551615"
        );
    }

    #[test]
    fn literals_must_match_exactly() {
        let expected = RuleValue::String("v1");

        assert_eq!(literal(&"v1".to_owned(), expected), Ok(()));
        assert_eq!(
            literal(&"v2".to_owned(), expected)
                .unwrap_err()
                .in_field("version")
                .to_string(),
            r#"`version` must be "v1", got "v2""#
        );
    }

    #[test]
    fn absent_optional_values_keep_every_rule() {
        assert_eq!(length(&None::<String>, Some(1), None), Ok(()));
        assert_eq!(literal(&None::<bool>, RuleValue::Bool(true)), Ok(()));
        assert!(length(&Some(String::new()), Some(1), None).is_err());
    }
}
