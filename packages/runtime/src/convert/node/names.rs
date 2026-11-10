//! The strings of the field and variant names that one conversion reads or
//! writes, which it makes once.

use std::cell::RefCell;
use std::ptr;

use crate::runtime::node::{Env, Error, Value};

/// The most names whose strings one conversion keeps, so that finding one
/// stays a short scan.
const KEPT_NAMES: usize = 64;

/// The string of each field and variant name that one conversion has used.
/// Serde passes the same `&'static str` for every value of a type, and V8
/// reuses a string that it has made a property key once for less than it
/// makes a new one.
pub(super) struct Names<'env> {
    env: Env<'env>,
    kept: RefCell<Vec<(&'static str, Value<'env>)>>,
}

impl<'env> Names<'env> {
    /// Keep the names of one conversion in `env`.
    pub(super) fn new(env: Env<'env>) -> Self {
        Self {
            env,
            kept: RefCell::default(),
        }
    }

    /// Return the string of a field or variant name.
    pub(super) fn string(&self, name: &'static str) -> Result<Value<'env>, Error> {
        // Equal pointers to `str` have the same address and length.
        let kept = self
            .kept
            .borrow()
            .iter()
            .find_map(|&(kept, string)| ptr::eq(kept, name).then_some(string));
        if let Some(string) = kept {
            return Ok(string);
        }
        let string = self.env.string(name)?;
        let mut kept = self.kept.borrow_mut();
        if kept.len() < KEPT_NAMES {
            kept.push((name, string));
        }
        Ok(string)
    }
}
