//! A target cannot gate an exported method, because every host of one build
//! shares a contract but not a target. Features can, also inside `all`,
//! `any` and `not`, so a method's `#[cfg]` must test only features.

pub struct Counter;

#[bimetal::export]
impl Counter {
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn value(&self) -> i64 {
        0
    }
}

#[bimetal::export]
impl Counter {
    #[cfg(all(feature = "extras", unix))]
    #[must_use]
    pub fn limit(&self) -> i64 {
        0
    }
}

fn main() {}
