//! A Rust module. Hosts see it as the `text` namespace.

use bimetal::Model;
use serde::{Deserialize, Serialize};

/// Text and how many words it has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct Phrase {
    pub text: String,
    pub word_count: u32,
}

/// Count the words in a text.
#[bimetal::export]
#[must_use]
pub fn phrase(text: &str) -> Phrase {
    Phrase {
        word_count: u32::try_from(text.split_whitespace().count()).unwrap_or(u32::MAX),
        text: text.to_owned(),
    }
}

/// The words of a text, with their lengths.
///
/// A field may have the name of a Python builtin, such as `list`. A field
/// with the name of a member of Pydantic's `BaseModel`, such as `json`, is
/// `json_value` in Python, and keeps its name on the wire and in TypeScript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
pub struct Words {
    pub list: Vec<String>,
    pub lengths: Vec<u32>,
    /// The words as a JSON array.
    pub json: String,
}

/// List the words of a text. An export may have the name of a Python
/// builtin.
#[bimetal::export]
#[must_use]
pub fn list(text: &str) -> Words {
    let list: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    Words {
        lengths: list
            .iter()
            .map(|word| u32::try_from(word.chars().count()).unwrap_or(u32::MAX))
            .collect(),
        json: serde_json::to_string(&list).expect("a list of strings is valid JSON"),
        list,
    }
}
