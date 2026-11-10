//! String literals and identifiers as every generated language reads them.

/// Quote a string with JSON escaping, which TOML, Python, and JavaScript accept.
#[must_use]
pub(crate) fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

/// Return whether `value` follows Bimetal's portable ASCII identifier grammar,
/// which Python and JavaScript both accept, keywords aside.
#[must_use]
pub(crate) fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}
