//! Direct boundaries carry only the shapes that they name.

#[bimetal::export]
#[must_use]
pub fn checksum(#[bimetal(bytes)] payload: Vec<u16>) -> u32 {
    payload.len() as u32
}

#[bimetal::export]
#[must_use]
pub fn total(#[bimetal(buffer)] samples: &[String]) -> u32 {
    samples.len() as u32
}

#[bimetal::export]
#[bimetal(returns(bytes))]
pub fn nothing() {}

fn main() {}
