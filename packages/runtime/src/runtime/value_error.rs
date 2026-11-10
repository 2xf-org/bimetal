//! [`ValueError`], where a value that crosses a host boundary is invalid.

use std::fmt::{self, Display, Formatter};

/// Where a value is invalid, and why: a field rule that it breaks, or a schema
/// that breaks the Arrow C Data Interface.
///
/// It is boxed, so that the `Result` of every rule check stays one word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueError(Box<Problem>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Problem {
    /// The path from the outermost value, such as `records[2].label`. Each
    /// container prepends its step as the error leaves it, so a valid value
    /// builds no path.
    path: String,
    /// What is wrong, such as `must be at least 1, got 0` or `is NULL`.
    problem: String,
}

impl ValueError {
    /// Describe a problem of a value that has no path yet.
    #[cold]
    #[must_use]
    pub fn new(problem: impl Into<String>) -> Self {
        Self(Box::new(Problem {
            path: String::new(),
            problem: problem.into(),
        }))
    }

    /// Place the problem inside the field `name` of a model.
    #[cold]
    #[must_use]
    pub fn in_field(self, name: &str) -> Self {
        self.prepend(name)
    }

    /// Place the problem inside item `index` of a list or tuple.
    #[cold]
    #[must_use]
    pub fn at_index(self, index: usize) -> Self {
        self.prepend(&format!("[{index}]"))
    }

    /// Place the problem inside the value of `key` in a map.
    #[cold]
    #[must_use]
    pub fn at_key(self, key: &str) -> Self {
        self.prepend(&format!("[{key:?}]"))
    }

    /// Put `step`, a field name or an `[index]`, in front of the path, with a
    /// dot when the path goes on with a field.
    fn prepend(mut self, step: &str) -> Self {
        let path = &mut self.0.path;
        if !path.is_empty() && !path.starts_with('[') {
            path.insert(0, '.');
        }
        path.insert_str(0, step);
        self
    }
}

impl Display for ValueError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let Problem { path, problem } = &*self.0;
        if path.is_empty() {
            formatter.write_str(problem)
        } else {
            write!(formatter, "`{path}` {problem}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_read_from_the_outermost_value_inward() {
        let error = ValueError::new("is NULL")
            .in_field("label")
            .at_key("first")
            .in_field("records")
            .at_index(2)
            .at_index(0);

        assert_eq!(
            error.to_string(),
            r#"`[0][2].records["first"].label` is NULL"#
        );
    }
}
