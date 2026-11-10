//! An exported method must exist on every host, because they share a contract.

pub struct Counter;

#[bimetal::export]
impl Counter {
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn value(&self) -> i64 {
        0
    }
}

fn main() {}
