//! `#[serde(default)]` needs the Rust `Default` of the field's type.

use bimetal::Model;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Model)]
pub struct Retry {
    pub attempts: u32,
}

#[derive(Serialize, Model)]
pub struct Settings {
    #[serde(default)]
    pub retry: Retry,
}

fn main() {}
