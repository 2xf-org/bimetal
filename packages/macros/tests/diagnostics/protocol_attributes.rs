//! A method implements one protocol, and a constructor none.

pub struct Twice;

#[bimetal::export]
impl Twice {
    #[bimetal(len)]
    #[bimetal(hash)]
    #[must_use]
    pub fn value(&self) -> u32 {
        0
    }
}

pub struct Enters;

#[bimetal::export]
impl Enters {
    #[bimetal(constructor)]
    #[bimetal(enter)]
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[bimetal::export]
#[bimetal(next)]
#[must_use]
pub fn free_function() -> Option<u32> {
    None
}

fn main() {}
