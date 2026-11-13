//! Direct boundaries carry only the shapes that they name, and bytes need no
//! attribute.

#[bimetal::export]
#[must_use]
pub fn checksum(#[bimetal(bytes)] payload: Vec<u8>) -> u32 {
    payload.len() as u32
}

#[bimetal::export]
#[must_use]
pub fn total(#[bimetal(buffer)] samples: &[String]) -> u32 {
    samples.len() as u32
}

#[bimetal::export]
#[bimetal(returns(buffer))]
pub fn nothing() {}

fn main() {}
