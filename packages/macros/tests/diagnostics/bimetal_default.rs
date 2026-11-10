//! `#[bimetal(default = ...)]` is gone: `#[serde(default)]` is enough.

use bimetal::Model;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Model)]
pub struct Settings {
    #[serde(default)]
    #[bimetal(default = false)]
    pub verbose: bool,
}

fn main() {}
