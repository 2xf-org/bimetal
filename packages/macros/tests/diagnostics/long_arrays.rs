//! Serde writes arrays of at most 32 items, so a longer array of anything but
//! bytes needs a `Vec`.

#[bimetal::export]
pub fn total(values: [u32; 64]) -> u32 {
    values.iter().sum()
}

fn main() {}
