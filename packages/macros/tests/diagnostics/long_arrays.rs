//! Serde writes arrays of at most 32 items, so a longer byte array needs
//! `bytes` or a `Vec`.

#[bimetal::export]
pub fn digest(data: &[u8; 64]) -> usize {
    data.len()
}

fn main() {}
