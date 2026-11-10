//! `--remap-path-prefix` mappings, which keep local paths out of compiled
//! artifacts, and their inverse for the paths in compiler diagnostics.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The local directories that rustc writes under location-independent names.
#[derive(Debug, Clone, Default)]
pub(crate) struct PathRemaps {
    remaps: Vec<PathRemap>,
}

/// One local directory and the name that artifacts use for it.
#[derive(Debug, Clone)]
struct PathRemap {
    local: PathBuf,
    portable: String,
    /// Whether restoring `portable` names `local`, rather than another path
    /// of the same directory.
    restores: bool,
}

impl PathRemaps {
    /// Write `local` and the paths below it as `portable` and the same paths
    /// below that. A later mapping of the same directory replaces an earlier one.
    pub(crate) fn insert(&mut self, local: PathBuf, portable: String) {
        self.remaps.retain(|remap| remap.local != local);
        self.remaps.push(PathRemap {
            local,
            portable,
            restores: true,
        });
    }

    /// Also write `alias`, another path through which rustc reaches a
    /// directory, as `portable`. Restoring `portable` still names the
    /// directory given to [`Self::insert`].
    pub(crate) fn insert_alias(&mut self, alias: PathBuf, portable: String) {
        self.remaps.retain(|remap| remap.local != alias);
        self.remaps.push(PathRemap {
            local: alias,
            portable,
            restores: false,
        });
    }

    /// Return the rustc flags, one argument each.
    ///
    /// rustc applies the last mapping that matches, so the flags list deeper
    /// directories later: a dependency in the Cargo target directory, inside
    /// the workspace, takes the target directory's name.
    ///
    /// # Errors
    ///
    /// Returns an error when a path is not UTF-8, which Cargo cannot pass.
    pub(crate) fn flags(&self) -> Result<Vec<String>> {
        let mut remaps: Vec<&PathRemap> = self.remaps.iter().collect();
        remaps.sort_by_key(|remap| remap.local.components().count());
        let mut flags = Vec::new();
        for remap in remaps {
            let local = remap
                .local
                .to_str()
                .with_context(|| format!("{} is not a UTF-8 path", remap.local.display()))?;
            flags.push(format!("--remap-path-prefix={local}={}", remap.portable));
        }
        Ok(flags)
    }

    /// Return the local path of a path that rustc wrote with a portable name,
    /// or `None` if no mapping produced it. A relative portable name, such as
    /// the empty one, produces only relative paths.
    #[must_use]
    pub(crate) fn local_path(&self, path: &str) -> Option<PathBuf> {
        let path = Path::new(path);
        let depth = |remap: &PathRemap| Path::new(&remap.portable).components().count();
        let mut deepest: Option<&PathRemap> = None;
        let restorable = self.remaps.iter().filter(|remap| {
            remap.restores && Path::new(&remap.portable).has_root() == path.has_root()
        });
        for remap in restorable {
            if path.starts_with(&remap.portable)
                && deepest.is_none_or(|current| depth(remap) > depth(current))
            {
                deepest = Some(remap);
            }
        }
        let remap = deepest?;
        let relative = path.strip_prefix(&remap.portable).ok()?;
        // Joined part by part, so the path uses this platform's separators.
        Some(
            relative
                .iter()
                .fold(remap.local.clone(), |local, part| local.join(part)),
        )
    }

    /// Replace the portable paths in the locations of rendered diagnostics,
    /// such as ` --> /workspace/src/lib.rs:3:5`, with local paths that an
    /// editor or terminal can open.
    #[must_use]
    pub(crate) fn restore_paths(&self, text: &str) -> String {
        let mut restored = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            restored.push_str(&self.restore_line(line));
        }
        restored
    }

    /// Restore the path of one `-->` or `:::` location line, which can be in
    /// colour.
    fn restore_line(&self, line: &str) -> String {
        let (start, rest) = line.split_at(styled_prefix(line, /* spaces */ true));
        let Some((marker, styled)) = ["--> ", "::: "]
            .into_iter()
            .find_map(|marker| rest.strip_prefix(marker).map(|location| (marker, location)))
        else {
            return line.to_owned();
        };
        let (style, location) = styled.split_at(styled_prefix(styled, /* spaces */ false));
        let end = location.find('\u{1b}').unwrap_or(location.len());
        let ending = &location[location[..end].trim_end().len()..];
        let location = location[..end].trim_end();
        let (path, position) = split_position(location);
        match self.local_path(path) {
            Some(local) => format!(
                "{start}{marker}{style}{}{position}{ending}",
                local.display()
            ),
            None => line.to_owned(),
        }
    }
}

/// Return the length of the colour codes, such as `\u{1b}[1m`, that `text`
/// starts with, and with `spaces`, of the spaces among them too.
#[must_use]
fn styled_prefix(text: &str, spaces: bool) -> usize {
    let mut length = 0;
    loop {
        let rest = &text[length..];
        if spaces && rest.starts_with(' ') {
            length += 1;
        } else if let Some(code) = rest.strip_prefix("\u{1b}[")
            && let Some(end) = code.find(|character: char| character.is_ascii_alphabetic())
        {
            length += "\u{1b}[".len() + end + 1;
        } else {
            return length;
        }
    }
}

/// Split `path:line:column` into the path and the `:line:column` suffix.
#[must_use]
fn split_position(location: &str) -> (&str, &str) {
    let mut end = location.len();
    for _ in 0..2 {
        let Some(colon) = location[..end].rfind(':') else {
            break;
        };
        let number = &location[colon + 1..end];
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            break;
        }
        end = colon;
    }
    location.split_at(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Return a local path as `local_path` writes it on this platform.
    fn local(root: &str, relative: &str) -> String {
        relative
            .split('/')
            .fold(PathBuf::from(root), |path, part| path.join(part))
            .display()
            .to_string()
    }

    fn remaps() -> PathRemaps {
        let mut remaps = PathRemaps::default();
        remaps.insert(PathBuf::from("/Users/a b/app"), "/workspace".to_owned());
        remaps.insert(PathBuf::from("/Users/a b/.cargo"), "/cargo".to_owned());
        remaps.insert(PathBuf::from("/Users/a b/app/target"), "/target".to_owned());
        remaps.insert(
            PathBuf::from("/Users/a b/bimetal/packages/runtime"),
            "/source/bimetal".to_owned(),
        );
        remaps
    }

    #[test]
    fn deeper_directories_are_mapped_last() {
        assert_eq!(
            remaps().flags().expect("UTF-8 paths"),
            [
                "--remap-path-prefix=/Users/a b/app=/workspace",
                "--remap-path-prefix=/Users/a b/.cargo=/cargo",
                "--remap-path-prefix=/Users/a b/app/target=/target",
                "--remap-path-prefix=/Users/a b/bimetal/packages/runtime=/source/bimetal",
            ]
        );
    }

    #[test]
    fn diagnostic_locations_name_local_files() {
        let rendered = "error[E0425]: cannot find value `x` in this scope\n --> /workspace/src/lib.rs:3:5\n  |\n3 |     x\n  |     ^ not found\n   ::: /source/bimetal/src/lib.rs:10:1\n    --> /elsewhere/lib.rs:1:1\nnote: /workspace/src/lib.rs stays as written\n";

        assert_eq!(
            remaps().restore_paths(rendered),
            format!(
                "error[E0425]: cannot find value `x` in this scope\n --> {}:3:5\n  |\n3 |     x\n  |     ^ not found\n   ::: {}:10:1\n    --> /elsewhere/lib.rs:1:1\nnote: /workspace/src/lib.rs stays as written\n",
                local("/Users/a b/app", "src/lib.rs"),
                local("/Users/a b/bimetal/packages/runtime", "src/lib.rs"),
            )
        );
    }

    #[test]
    fn coloured_diagnostic_locations_keep_their_colours() {
        let rendered = "\u{1b}[1m\u{1b}[91merror\u{1b}[0m: x\n \u{1b}[1m\u{1b}[94m--> \u{1b}[0m/workspace/src/lib.rs:6:5\n  \u{1b}[1m\u{1b}[94m::: \u{1b}[0m/source/bimetal/src/lib.rs:1:1\u{1b}[0m\n";

        assert_eq!(
            remaps().restore_paths(rendered),
            format!(
                "\u{1b}[1m\u{1b}[91merror\u{1b}[0m: x\n \u{1b}[1m\u{1b}[94m--> \u{1b}[0m{}:6:5\n  \u{1b}[1m\u{1b}[94m::: \u{1b}[0m{}:1:1\u{1b}[0m\n",
                local("/Users/a b/app", "src/lib.rs"),
                local("/Users/a b/bimetal/packages/runtime", "src/lib.rs"),
            )
        );
    }

    #[test]
    fn local_paths_prefer_the_deepest_portable_name() {
        let mut remaps = remaps();
        remaps.insert(PathBuf::from("/tmp/link"), "/workspace/link".to_owned());
        remaps.insert_alias(PathBuf::from("/tmp/alias/app"), "/workspace".to_owned());

        assert_eq!(
            remaps.local_path("/workspace/link/src/lib.rs"),
            Some(PathBuf::from("/tmp/link/src/lib.rs"))
        );
        assert_eq!(
            remaps.local_path("/workspace/src/lib.rs"),
            Some(PathBuf::from("/Users/a b/app/src/lib.rs"))
        );
        assert_eq!(remaps.local_path("/workspaces/lib.rs"), None);
        assert!(
            remaps
                .flags()
                .expect("UTF-8 paths")
                .contains(&"--remap-path-prefix=/tmp/alias/app=/workspace".to_owned())
        );
        assert_eq!(remaps.local_path("src/lib.rs"), None);
    }

    #[test]
    fn an_empty_portable_name_keeps_paths_relative() {
        let mut remaps = PathRemaps::default();
        remaps.insert(PathBuf::from("/Users/a b/app"), String::new());
        remaps.insert(PathBuf::from("/Users/a b/.cargo"), "/cargo".to_owned());

        assert_eq!(
            remaps.flags().expect("UTF-8 paths")[0],
            "--remap-path-prefix=/Users/a b/app="
        );
        assert_eq!(
            remaps.local_path("src/lib.rs"),
            Some(PathBuf::from("/Users/a b/app/src/lib.rs"))
        );
        assert_eq!(
            remaps.local_path("/cargo/registry/x.rs"),
            Some(PathBuf::from("/Users/a b/.cargo/registry/x.rs"))
        );
        assert_eq!(remaps.local_path("/elsewhere/lib.rs"), None);
    }

    #[test]
    fn positions_split_from_paths_with_colons() {
        assert_eq!(split_position("C:/a/b.rs:3:5"), ("C:/a/b.rs", ":3:5"));
        assert_eq!(split_position("/a/b.rs"), ("/a/b.rs", ""));
        assert_eq!(split_position("/a/b.rs:7"), ("/a/b.rs", ":7"));
    }
}
