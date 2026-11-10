//! What a Cargo build reports: its JSON messages, and its own output.

use std::io;
use std::path::PathBuf;
use std::process::ChildStderr;

use serde_json::Value;

/// Return the JSON messages in Cargo's output.
#[must_use]
pub(crate) fn parse(output: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Return every file that Cargo reported for the `cdylib` target of the
/// package `package_id`, such as its libraries and their debug symbols.
#[must_use]
pub(crate) fn cdylib_files(messages: &[Value], package_id: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for message in messages {
        if message["reason"] != "compiler-artifact" || message["package_id"] != package_id {
            continue;
        }
        let Some(crate_types) = message["target"]["crate_types"].as_array() else {
            continue;
        };
        if !crate_types.iter().any(|value| value == "cdylib") {
            continue;
        }
        let Some(filenames) = message["filenames"].as_array() else {
            continue;
        };
        files.extend(
            filenames
                .iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from),
        );
    }
    files
}

/// Return the compiler diagnostics in Cargo's JSON messages whose level, such
/// as `warning`, `selected` accepts, as rustc rendered them.
#[must_use]
pub(crate) fn compiler_messages(
    messages: &[Value],
    selected: impl Fn(&str) -> bool,
) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message["reason"] == "compiler-message")
        .filter(|message| message["message"]["level"].as_str().is_some_and(&selected))
        .filter_map(|message| message["message"]["rendered"].as_str().map(str::to_owned))
        .collect()
}

/// Read Cargo's own output as it comes, say each time that Cargo waits for
/// the lock of a build directory that another Cargo command holds, such as
/// rust-analyzer's, and return all of it. `label` names the build.
pub(crate) fn read_output(output: ChildStderr, label: &str) -> io::Result<Vec<u8>> {
    use std::io::BufRead;

    let mut reader = io::BufReader::new(output);
    let mut all = Vec::new();
    loop {
        let start = all.len();
        if reader.read_until(b'\n', &mut all)? == 0 {
            return Ok(all);
        }
        let line = String::from_utf8_lossy(&all[start..]);
        // The builds wait for each other's short hold of the package cache
        // as they start, which is not worth a line.
        if let Some(lock) = line
            .trim()
            .strip_prefix("Blocking waiting for file lock on ")
            && !lock.ends_with("package cache")
        {
            eprintln!("  The {label} build waits for another Cargo command to release its {lock}");
        }
    }
}

/// Return Cargo's own report of a failed build without its progress lines.
#[must_use]
pub(crate) fn errors(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .split_inclusive('\n')
        .filter(|line| !is_progress(line))
        .collect()
}

/// Return whether a line of Cargo's output reports progress, such as
/// `   Compiling app v0.1.0 (...)`, rather than a problem.
#[must_use]
fn is_progress(line: &str) -> bool {
    const STATUSES: [&str; 15] = [
        "Adding",
        "Blocking",
        "Checking",
        "Compiling",
        "Dirty",
        "Downgrading",
        "Downloaded",
        "Downloading",
        "Finished",
        "Fresh",
        "Locking",
        "Removing",
        "Running",
        "Unpacking",
        "Updating",
    ];
    line.starts_with(' ')
        && line
            .split_whitespace()
            .next()
            .is_some_and(|status| STATUSES.contains(&status))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn diagnostics_are_the_rendered_compiler_messages() {
        let messages = parse(
            concat!(
                r#"{"reason":"compiler-message","message":{"level":"error","rendered":"error: one\n"}}"#,
                "\n",
                r#"{"reason":"compiler-artifact","package_id":"x"}"#,
                "\nnot JSON\n",
                r#"{"reason":"compiler-message","message":{"level":"warning","rendered":"warning: two\n"}}"#,
                "\n",
            )
            .as_bytes(),
        );

        assert_eq!(
            compiler_messages(&messages, |_| true),
            ["error: one\n", "warning: two\n"]
        );
        assert_eq!(
            compiler_messages(&messages, |level| level == "warning"),
            ["warning: two\n"]
        );
    }

    #[test]
    fn cdylib_files_are_those_of_the_package() {
        let messages = parse(
            concat!(
                r#"{"reason":"compiler-artifact","package_id":"dependency","target":{"crate_types":["cdylib"]},"filenames":["/t/dependency.dll"]}"#,
                "\n",
                r#"{"reason":"compiler-artifact","package_id":"link","target":{"crate_types":["lib"]},"filenames":["/t/liblink.rlib"]}"#,
                "\n",
                r#"{"reason":"compiler-artifact","package_id":"link","target":{"crate_types":["cdylib"]},"filenames":["/t/link.dll","/t/link.dll.lib","/t/link.pdb"]}"#,
                "\n",
            )
            .as_bytes(),
        );

        assert_eq!(
            cdylib_files(&messages, "link"),
            [
                Path::new("/t/link.dll"),
                Path::new("/t/link.dll.lib"),
                Path::new("/t/link.pdb")
            ]
        );
    }

    #[test]
    fn failure_reports_leave_out_cargo_progress() {
        let output = "   Compiling app v0.1.0 (/repo/app)\n    Blocking waiting for file lock on package cache\nerror: failed to run custom build command for `app v0.1.0 (/repo/app)`\n\nCaused by:\n  build failed\n";

        assert_eq!(
            errors(output.as_bytes()),
            "error: failed to run custom build command for `app v0.1.0 (/repo/app)`\n\nCaused by:\n  build failed\n"
        );
    }
}
