//! The Rust `Default` that `#[serde(default)]` fills in must keep the
//! field's rules.

use bimetal::Model;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Model)]
pub struct Settings {
    #[serde(default)]
    #[bimetal(min_length = 1)]
    pub prefix: String,
}

#[derive(Serialize, Deserialize, Model)]
pub struct Listing {
    #[serde(default)]
    #[bimetal(min_length = 1)]
    pub names: Vec<String>,
}

fn main() {}
