//! A model type takes no `#[bimetal(...)]` attribute; `host` is gone.

use bimetal::Model;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Model)]
#[serde(transparent)]
#[bimetal(host = String)]
pub struct JobId(pub String);

fn main() {}
