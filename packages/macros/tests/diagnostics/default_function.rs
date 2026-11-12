//! A `required` field takes no `#[serde(default = "path")]`, which Serde
//! calls for an absent `Option` too, and a default function takes no
//! `skip_serializing_if`, whose `None` Serde would read back as the
//! function's value.

use bimetal::Model;
use serde::{Deserialize, Serialize};

fn label() -> Option<String> {
    Some("brackets-curly".to_owned())
}

#[derive(Serialize, Deserialize, Model)]
pub struct Required {
    #[serde(default = "label")]
    #[bimetal(required)]
    pub icon: Option<String>,
}

#[derive(Serialize, Deserialize, Model)]
pub struct Skipped {
    #[serde(default = "label", skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

fn main() {}
