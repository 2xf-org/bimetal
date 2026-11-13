//! A brief call is synchronous, and takes nothing that waits for the host.

#[bimetal::export]
#[bimetal(brief)]
pub async fn later() {}

#[bimetal::export]
#[bimetal(brief)]
#[must_use]
pub fn apply(_callback: bimetal::Callback<(u32,), u32>) -> u32 {
    0
}

fn main() {}
