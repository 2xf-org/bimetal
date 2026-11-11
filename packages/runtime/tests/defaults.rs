#![cfg(not(target_arch = "wasm32"))]

//! Defaults that cannot enter the contract, which discovery reports. The
//! suite has its own registry, so the problems reach no other suite.

use std::sync::atomic::{AtomicU32, Ordering};

use bimetal::Model;
use serde::{Deserialize, Serialize};

/// A span of time whose `Default`, zero seconds, its rule refuses.
#[derive(Debug, Default, Serialize, Deserialize, Model)]
pub struct Window {
    /// How long the span lasts.
    #[bimetal(ge = 1)]
    pub seconds: u32,
}

/// A number that differs on every call of its `Default`.
#[derive(Debug, Serialize, Deserialize, Model)]
#[serde(transparent)]
pub struct Ticket(pub u32);

impl Default for Ticket {
    fn default() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Limits whose fields hold their defaults when the caller gives none.
#[derive(Debug, Serialize, Deserialize, Model)]
pub struct Limits {
    /// The span that the limits apply to.
    #[serde(default)]
    pub window: Window,
    /// Which request the limits were read for.
    #[serde(default)]
    pub ticket: Ticket,
}

/// What a request may do, in a variant that holds a default.
#[derive(Debug, Serialize, Deserialize, Model)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Grant {
    /// The request may run for a span.
    Timed {
        /// How long the request may run.
        #[serde(default)]
        window: Window,
    },
}

/// Return the limits, so that an export uses the model.
#[bimetal::export]
#[must_use]
pub fn echo_limits(limits: Limits) -> Limits {
    limits
}

#[test]
fn discovery_reports_each_default_that_cannot_enter_the_contract() {
    let errors = bimetal::registry::manifest("defaults", "0.0.0")
        .expect_err("three defaults cannot enter the contract");

    // The models stay in the contract, so the export misses no type.
    assert_eq!(
        errors.to_string(),
        "the Rust API has 3 problems:\n  \
         - the default of field `ticket` of `defaults::Limits` changes from one call of `Default::default` to the next\n  \
         - the default of field `window` of `defaults::Grant::Timed` breaks a field rule: `seconds` must be at least 1, got 0\n  \
         - the default of field `window` of `defaults::Limits` breaks a field rule: `seconds` must be at least 1, got 0"
    );
}
