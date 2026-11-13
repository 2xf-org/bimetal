//! Python `datetime` and `date` values, and the RFC 3339 text that Serde
//! writes and reads for a contract's date-times and dates.
//!
//! A value is read field by field, and made from the bytes that `pickle`
//! keeps for it, so a crossing calls neither `isoformat` nor the argument
//! parser of a constructor.

use chrono::{DateTime, Datelike, NaiveDate, Timelike};
use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBytes, PyDict, PyString, PyType};

use super::short_text::{Full, ShortText};

/// The `datetime` and `date` classes.
fn classes(py: Python<'_>) -> PyResult<(&Bound<'_, PyType>, &Bound<'_, PyType>)> {
    static DATETIME: PyOnceLock<Py<PyType>> = PyOnceLock::new();
    static DATE: PyOnceLock<Py<PyType>> = PyOnceLock::new();
    Ok((
        DATETIME.import(py, "datetime", "datetime")?,
        DATE.import(py, "datetime", "date")?,
    ))
}

/// Pydantic's `TzInfo` for an offset from UTC, one object for each offset.
fn tzinfo(py: Python<'_>, seconds: i32) -> PyResult<Bound<'_, PyAny>> {
    static UTC: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    static ZONES: PyOnceLock<Py<PyDict>> = PyOnceLock::new();
    static TZINFO: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    let make = |seconds: i32| {
        TZINFO
            .import(py, "pydantic_core", "TzInfo")?
            .call1((seconds,))
    };
    if seconds == 0 {
        return UTC
            .get_or_try_init(py, || make(0).map(Bound::unbind))
            .map(|zone| zone.bind(py).clone());
    }
    let zones = ZONES.get_or_init(py, || PyDict::new(py).unbind()).bind(py);
    if let Some(zone) = zones.get_item(seconds)? {
        return Ok(zone);
    }
    let zone = make(seconds)?;
    zones.set_item(seconds, &zone)?;
    Ok(zone)
}

/// One field of a `date` or a `datetime`.
fn field(value: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<u32> {
    value.getattr(name)?.extract()
}

impl From<Full> for PyErr {
    fn from(_: Full) -> Self {
        PyValueError::new_err("the date or time is out of range")
    }
}

/// The RFC 3339 text of `value`, as `isoformat` writes it, when `value` is
/// exactly a `datetime`, or `None` for any other object. A naive `datetime`
/// has no offset, which Rust's date-times do not read.
///
/// # Errors
///
/// Returns a Python error when reading a field or the offset fails, or
/// `ValueError` for an offset with microseconds.
pub(crate) fn date_time_text(value: &Bound<'_, PyAny>) -> PyResult<Option<ShortText>> {
    let py = value.py();
    if !value.get_type().is(classes(py)?.0) {
        return Ok(None);
    }
    let mut text = date_of(value)?;
    text.push(b'T')?;
    text.digits(field(value, intern!(py, "hour"))?, 2)?;
    text.push(b':')?;
    text.digits(field(value, intern!(py, "minute"))?, 2)?;
    text.push(b':')?;
    text.digits(field(value, intern!(py, "second"))?, 2)?;
    let microsecond = field(value, intern!(py, "microsecond"))?;
    if microsecond != 0 {
        text.push(b'.')?;
        text.digits(microsecond, 6)?;
    }
    if let Some(offset) = offset(value)? {
        text.push(if offset < 0 { b'-' } else { b'+' })?;
        let offset = offset.unsigned_abs();
        text.digits(offset / 3600, 2)?;
        text.push(b':')?;
        text.digits(offset / 60 % 60, 2)?;
        if offset % 60 != 0 {
            text.push(b':')?;
            text.digits(offset % 60, 2)?;
        }
    }
    Ok(Some(text))
}

/// The `YYYY-MM-DD` text of `value` when it is exactly a `date`, or `None`
/// for any other object, such as a `datetime`.
///
/// # Errors
///
/// Returns a Python error when reading a field fails.
pub(crate) fn date_text(value: &Bound<'_, PyAny>) -> PyResult<Option<ShortText>> {
    if !value.get_type().is(classes(value.py())?.1) {
        return Ok(None);
    }
    date_of(value).map(Some)
}

/// The `YYYY-MM-DD` text of a `date` or a `datetime`.
fn date_of(value: &Bound<'_, PyAny>) -> PyResult<ShortText> {
    let py = value.py();
    let mut text = ShortText::new();
    text.digits(field(value, intern!(py, "year"))?, 4)?;
    text.push(b'-')?;
    text.digits(field(value, intern!(py, "month"))?, 2)?;
    text.push(b'-')?;
    text.digits(field(value, intern!(py, "day"))?, 2)?;
    Ok(text)
}

/// The offset from UTC of a `datetime`, in seconds, or `None` when it is
/// naive. UTC from Python or from this module needs no call.
fn offset(value: &Bound<'_, PyAny>) -> PyResult<Option<i32>> {
    static PYTHON_UTC: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    let py = value.py();
    let zone = value.getattr(intern!(py, "tzinfo"))?;
    if zone.is_none() {
        return Ok(None);
    }
    if zone.is(PYTHON_UTC.import(py, "datetime", "UTC")?) || zone.is(&tzinfo(py, 0)?) {
        return Ok(Some(0));
    }
    let delta = value.call_method0(intern!(py, "utcoffset"))?;
    if delta.is_none() {
        return Ok(None);
    }
    let part = |name: &Bound<'_, PyString>| -> PyResult<i32> { delta.getattr(name)?.extract() };
    if part(intern!(py, "microseconds"))? != 0 {
        return Err(PyValueError::new_err(
            "an offset from UTC with microseconds has no RFC 3339 text",
        ));
    }
    Ok(Some(
        part(intern!(py, "days"))? * 86_400 + part(intern!(py, "seconds"))?,
    ))
}

/// The `datetime` of RFC 3339 text, to the microsecond, as Pydantic reads it.
///
/// # Errors
///
/// Returns `ValueError` for text that is not RFC 3339, or for a year that
/// Python does not hold.
pub(crate) fn date_time<'py>(py: Python<'py>, text: &str) -> PyResult<Bound<'py, PyAny>> {
    let parsed = DateTime::parse_from_rfc3339(text)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    // A leap second's nanoseconds run past one second.
    let microsecond = (parsed.nanosecond() % 1_000_000_000) / 1_000;
    let [_, micro_high, micro_middle, micro_low] = microsecond.to_be_bytes();
    let [year_high, year_low] = year(parsed.year())?;
    // The ten bytes of a pickled `datetime`, which the constructor reads,
    // with the `tzinfo`, as they are.
    let state = [
        year_high,
        year_low,
        small(parsed.month()),
        small(parsed.day()),
        small(parsed.hour()),
        small(parsed.minute()),
        small(parsed.second()),
        micro_high,
        micro_middle,
        micro_low,
    ];
    let zone = tzinfo(py, parsed.offset().local_minus_utc())?;
    classes(py)?.0.call1((PyBytes::new(py, &state), zone))
}

/// The `date` of `YYYY-MM-DD` text.
///
/// # Errors
///
/// Returns `ValueError` for text that is not a date, or for a year that
/// Python does not hold.
pub(crate) fn date<'py>(py: Python<'py>, text: &str) -> PyResult<Bound<'py, PyAny>> {
    let parsed: NaiveDate = text
        .parse()
        .map_err(|error: chrono::ParseError| PyValueError::new_err(error.to_string()))?;
    let [year_high, year_low] = year(parsed.year())?;
    // The four bytes of a pickled `date`.
    let state = [
        year_high,
        year_low,
        small(parsed.month()),
        small(parsed.day()),
    ];
    classes(py)?.1.call1((PyBytes::new(py, &state),))
}

/// The two bytes of a year that Python holds, from 1 to 9999.
fn year(year: i32) -> PyResult<[u8; 2]> {
    u16::try_from(year)
        .ok()
        .filter(|year| (1..=9999).contains(year))
        .map(u16::to_be_bytes)
        .ok_or_else(|| PyValueError::new_err(format!("year {year} is out of range")))
}

/// A month, a day, an hour, a minute or a second, which fits a byte.
fn small(value: u32) -> u8 {
    u8::try_from(value).expect("chrono keeps each field of a date-time below 256")
}
