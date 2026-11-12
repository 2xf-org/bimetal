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

/// A pace whose `Default` holds a NaN inside an `Option`, which JSON writes
/// as `null`, the same as `None`.
#[derive(Debug, Serialize, Deserialize, Model)]
pub struct Pace {
    /// How many requests run for each one that waits.
    pub ratio: Option<f64>,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            ratio: Some(f64::NAN),
        }
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
    /// How many times the request may run, which no default function may
    /// leave at zero.
    #[serde(default = "never")]
    #[bimetal(ge = 1)]
    pub runs: u32,
    /// Which limits these are, a number that differs on every call.
    #[serde(default = "next_serial")]
    pub serial: u32,
    /// The share of the capacity that the request may use, a NaN by default.
    #[serde(default = "unknown_share")]
    pub share: Option<f64>,
    /// The weight of each retry, an infinity by default.
    #[serde(default = "unbounded_weights")]
    pub weights: Option<Vec<f64>>,
    /// How fast the request runs.
    #[serde(default)]
    pub pace: Pace,
}

/// Return zero runs, which the rule of `runs` refuses.
fn never() -> u32 {
    0
}

/// Return a number that differs on every call.
fn next_serial() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Return a share that is not a number.
fn unknown_share() -> Option<f64> {
    Some(f64::NAN)
}

/// Return weights that hold an infinity.
fn unbounded_weights() -> Option<Vec<f64>> {
    Some(vec![1.0, f64::INFINITY])
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
        .expect_err("eight defaults cannot enter the contract");

    // The models stay in the contract, so the export misses no type.
    assert_eq!(
        errors.to_string(),
        "the Rust API has 8 problems:\n  \
         - the default of field `pace` of `defaults::Limits` holds a NaN or an infinity, which the generated packages cannot represent; use a finite value\n  \
         - the default of field `runs` of `defaults::Limits` breaks a field rule: must be at least 1, got 0\n  \
         - the default of field `serial` of `defaults::Limits` changes from one call of `next_serial` to the next\n  \
         - the default of field `share` of `defaults::Limits` holds a NaN or an infinity, which the generated packages cannot represent; use a finite value\n  \
         - the default of field `ticket` of `defaults::Limits` changes from one call of `Default::default` to the next\n  \
         - the default of field `weights` of `defaults::Limits` holds a NaN or an infinity, which the generated packages cannot represent; use a finite value\n  \
         - the default of field `window` of `defaults::Grant::Timed` breaks a field rule: `seconds` must be at least 1, got 0\n  \
         - the default of field `window` of `defaults::Limits` breaks a field rule: `seconds` must be at least 1, got 0"
    );
}
