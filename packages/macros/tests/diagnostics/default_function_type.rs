//! A default function that is missing or returns another type fails once,
//! at the path, where Serde reports it.

use bimetal::Model;
use serde::{Deserialize, Serialize};

fn count() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Model)]
pub struct Mistyped {
    #[serde(default = "count")]
    pub icon: String,
}

#[derive(Serialize, Deserialize, Model)]
pub struct Missing {
    #[serde(default = "nowhere")]
    pub icon: String,
}

fn main() {}
