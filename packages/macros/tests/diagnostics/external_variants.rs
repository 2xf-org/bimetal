//! A tuple variant crosses only with one value, neither a variant nor the
//! value of a one-field variant takes a rule, and an internally tagged enum
//! has no tuple variants.

use bimetal::Model;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Model)]
pub enum Pair {
    Both(u32, u32),
}

#[derive(Serialize, Deserialize, Model)]
pub enum Wrapped {
    Value(#[serde(default)] u32),
}

#[derive(Serialize, Deserialize, Model)]
pub enum Counted {
    #[bimetal(ge = 1)]
    Count(u32),
}

#[derive(Serialize, Deserialize, Model)]
#[serde(tag = "kind")]
pub enum Tagged {
    Value(u32),
}

fn main() {}
