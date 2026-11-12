//! A gate on an exported method must be a `#[cfg]` of its own. Inside a
//! `cfg_attr`, at any depth, rustc would strip the method after Bimetal
//! listed it. An empty `any()` holds in no build, so it is rejected too.

pub struct Counter;

#[bimetal::export]
impl Counter {
    #[cfg_attr(not(feature = "extras"), cfg(feature = "extras"))]
    #[must_use]
    pub fn value(&self) -> i64 {
        0
    }
}

#[bimetal::export]
impl Counter {
    #[cfg_attr(unix, cfg_attr(not(feature = "extras"), cfg(feature = "extras")))]
    #[must_use]
    pub fn limit(&self) -> i64 {
        0
    }
}

#[bimetal::export]
impl Counter {
    #[cfg(any())]
    #[must_use]
    pub fn never(&self) -> i64 {
        0
    }
}

fn main() {}
