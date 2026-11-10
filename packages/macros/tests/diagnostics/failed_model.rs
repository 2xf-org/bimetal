//! A model that Bimetal cannot represent reports its own error, and the
//! exports that use it report nothing more.

use bimetal::Model;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Model)]
#[serde(tag = "kind")]
pub struct Point {
    pub x: i32,
}

#[bimetal::export]
#[must_use]
pub fn origin(point: Point) -> Point {
    point
}

fn main() {}
