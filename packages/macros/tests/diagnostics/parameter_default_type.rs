//! A parameter's default is an expression of the parameter's type, which the
//! compiler checks where it is written.

#[bimetal::export]
#[must_use]
pub fn repeat(#[bimetal(default = 1.5)] times: u32) -> u32 {
    times
}

fn main() {}
