//! Reporting problems together, so that one command shows every one to fix.

use std::fmt::{self, Display, Write as _};
use std::path::Path;

use anyhow::{Result, anyhow};

/// What a report's problems belong to.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Subject<'path> {
    /// The Rust API, whose problems are sentences of their own.
    RustApi,
    /// The application: its Rust API and its hosts' manifests.
    Application,
    /// A user-owned file, whose problems complete a sentence that starts
    /// with its path.
    File(&'path Path),
}

impl Display for Subject<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RustApi => formatter.write_str("the Rust API"),
            Self::Application => formatter.write_str("the application"),
            Self::File(path) => write!(formatter, "{}", path.display()),
        }
    }
}

/// Render problems as an indented list, each after a line break. A problem
/// that lists problems of its own nests them one level deeper.
#[must_use]
pub(crate) fn list(problems: impl IntoIterator<Item = impl Display>) -> String {
    let mut list = String::new();
    for problem in problems {
        let problem = problem.to_string().replace('\n', "\n    ");
        let _ = write!(list, "\n  - {problem}");
    }
    list
}

/// Return the error that reports `problems` of `subject`, which are at least
/// one: several after a count, and one alone.
#[must_use]
pub(crate) fn error(subject: Subject<'_>, problems: &[String]) -> anyhow::Error {
    match (subject, problems) {
        (Subject::File(path), [problem]) => anyhow!("{} {problem}", path.display()),
        (_, [problem]) => anyhow!("{problem}"),
        _ => anyhow!(
            "{subject} has {} problems:{}",
            problems.len(),
            list(problems)
        ),
    }
}

/// Fail with every problem of `subject`, or succeed when there are none.
pub(crate) fn report(subject: Subject<'_>, problems: &[String]) -> Result<()> {
    if problems.is_empty() {
        Ok(())
    } else {
        Err(error(subject, problems))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_problem_lists_indent_one_level_deeper() {
        assert_eq!(
            list(["one", "package.json has 2 problems:\n  - name"]),
            "\n  - one\n  - package.json has 2 problems:\n      - name"
        );
    }

    #[test]
    fn one_problem_stands_alone_and_several_follow_a_count() {
        let file = Subject::File(Path::new("pyproject.toml"));
        let one = ["must define `[project]`".to_owned()];
        let two = ["first".to_owned(), "second".to_owned()];

        assert!(report(file, &[]).is_ok());
        assert_eq!(
            error(file, &one).to_string(),
            "pyproject.toml must define `[project]`"
        );
        assert_eq!(
            error(file, &two).to_string(),
            "pyproject.toml has 2 problems:\n  - first\n  - second"
        );
        assert_eq!(error(Subject::RustApi, &one).to_string(), one[0]);
        assert_eq!(
            error(Subject::RustApi, &two).to_string(),
            "the Rust API has 2 problems:\n  - first\n  - second"
        );
    }
}
