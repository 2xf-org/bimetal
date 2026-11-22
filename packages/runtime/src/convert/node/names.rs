//! The strings of the field and variant names that one conversion reads or
//! writes, which it makes once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr;

use crate::runtime::node::{Env, Error, Value};

/// How many names stay in a linear list before promoting to a hash map.
const LINEAR_LIMIT: usize = 32;

/// Backing storage for [`Names`], keyed by static string slice identity
/// (pointer + length) so repeated Serde field names hit without hashing bytes.
enum Store<'env> {
    Linear(Vec<(&'static str, Value<'env>)>),
    #[allow(
        clippy::box_collection,
        reason = "keeps the common Linear variant small on the stack"
    )]
    Map(Box<HashMap<(*const u8, usize), Value<'env>>>),
}

/// The string of each field and variant name that one conversion has used.
/// Serde passes the same `&'static str` for every value of a type, and V8
/// reuses a string that it has made a property key once for less than it
/// makes a new one.
pub(super) struct Names<'env> {
    env: Env<'env>,
    kept: RefCell<Store<'env>>,
}

impl<'env> Names<'env> {
    /// Keep the names of one conversion in `env`.
    pub(super) fn new(env: Env<'env>) -> Self {
        Self {
            env,
            kept: RefCell::new(Store::Linear(Vec::new())),
        }
    }

    /// Return the string of a field or variant name.
    pub(super) fn string(&self, name: &'static str) -> Result<Value<'env>, Error> {
        let key = (name.as_ptr(), name.len());
        let hit = match &*self.kept.borrow() {
            Store::Linear(entries) => entries
                .iter()
                .find_map(|&(kept, string)| ptr::eq(kept, name).then_some(string)),
            Store::Map(entries) => entries.get(&key).copied(),
        };
        if let Some(string) = hit {
            return Ok(string);
        }
        let string = self.env.string(name)?;
        let mut store = self.kept.borrow_mut();
        match &mut *store {
            Store::Linear(entries) if entries.len() < LINEAR_LIMIT => {
                entries.push((name, string));
            }
            Store::Linear(entries) => {
                let mut map = HashMap::with_capacity(entries.len() * 2);
                for &(kept, value) in entries.iter() {
                    map.insert((kept.as_ptr(), kept.len()), value);
                }
                map.insert(key, string);
                *store = Store::Map(Box::new(map));
            }
            Store::Map(entries) => {
                entries.insert(key, string);
            }
        }
        Ok(string)
    }
}
