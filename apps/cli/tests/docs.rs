//! The docs against what they describe: every Rust, Python and TypeScript sample in the README and
//! in `docs/` comes from a program in `apps/examples/` or `apps/trials/`, every relative link goes
//! somewhere, and the prose has none of the dashes our style rules out. A sample here can't go
//! stale, as those programs compile and run.

// Tests unwrap; Clippy's test exemption doesn't reach the helpers of an integration test.
#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// How long the README's prose may grow, code left out.
const README_WORDS: usize = 650;

/// The folders that hold no program of ours.
const NOT_PROGRAMS: [&str; 8] = [
    "target",
    "node_modules",
    ".venv",
    "vendor",
    "__pycache__",
    ".bimetal",
    ".pdm-build",
    ".mypy_cache",
];

/// The fenced languages that the check covers, with the files that can hold their samples.
const LANGUAGES: [(&str, &[&str]); 3] = [
    ("rust", &["rs"]),
    ("python", &["py"]),
    ("ts", &["ts", "mjs"]),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every file below `folder`, leaving out the folders that hold no program of ours.
fn files_below(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for item in fs::read_dir(folder).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                let name = path.file_name().unwrap().to_string_lossy();
                if !NOT_PROGRAMS.contains(&name.as_ref()) {
                    folders.push(path);
                }
            } else {
                found.push(path);
            }
        }
    }
    found
}

fn markdown_files() -> Vec<PathBuf> {
    let root = root();
    let mut found: Vec<PathBuf> = files_below(&root.join("docs"))
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .collect();
    found.push(root.join("README.md"));
    found.sort();
    found
}

/// The lines of each fenced block of `language` in `text`.
fn blocks<'text>(text: &'text str, language: &str) -> Vec<Vec<&'text str>> {
    let (mut blocks, mut current) = (Vec::new(), None);
    let mut open = false;
    for line in text.lines() {
        if let Some(fence) = line.trim_start().strip_prefix("```") {
            if open {
                blocks.extend(current.take());
            } else if fence == language {
                current = Some(Vec::new());
            }
            open = !open;
        } else if let Some(block) = &mut current {
            block.push(line);
        }
    }
    blocks
}

/// The trimmed lines of each program in `apps/examples/` and `apps/trials/` with one of
/// `extensions`.
fn programs(extensions: &[&str]) -> Vec<Vec<String>> {
    let root = root();
    let mut sources = Vec::new();
    for folder in ["apps/examples", "apps/trials"] {
        for path in files_below(&root.join(folder)) {
            let extension = path
                .extension()
                .map(|extension| extension.to_string_lossy());
            if extension.is_some_and(|extension| extensions.contains(&extension.as_ref())) {
                let text = fs::read_to_string(path).unwrap();
                sources.push(text.lines().map(|line| line.trim().to_owned()).collect());
            }
        }
    }
    sources
}

/// Whether `source` holds `lines` in their order.
fn holds(source: &[String], lines: &[&str]) -> bool {
    let mut at = 0;
    lines.iter().all(|line| {
        let found = source[at..].iter().position(|there| there == line);
        found.is_some_and(|found| {
            at += found + 1;
            true
        })
    })
}

#[test]
fn every_sample_comes_from_a_program() {
    let mut stale = Vec::new();
    for (language, extensions) in LANGUAGES {
        let sources = programs(extensions);
        for path in markdown_files() {
            let text = fs::read_to_string(&path).unwrap();
            for block in blocks(&text, language) {
                let lines: Vec<&str> = block
                    .iter()
                    .map(|line| line.trim())
                    .filter(|line| !line.is_empty())
                    .collect();
                if !sources.iter().any(|source| holds(source, &lines)) {
                    stale.push(format!("{}: {}", path.display(), lines.join(" / ")));
                }
            }
        }
    }
    assert!(
        stale.is_empty(),
        "samples no program holds:\n{}",
        stale.join("\n\n")
    );
}

/// The anchors of a Markdown file's headings, as GitHub makes them.
fn anchors(text: &str) -> HashSet<String> {
    let mut open = false;
    let mut found = HashSet::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            open = !open;
        } else if !open && line.starts_with('#') {
            let heading = line.trim_start_matches('#').trim().to_lowercase();
            let anchor: String = heading
                .chars()
                .filter(|letter| letter.is_alphanumeric() || *letter == ' ' || *letter == '-')
                .map(|letter| if letter == ' ' { '-' } else { letter })
                .collect();
            found.insert(anchor);
        }
    }
    found
}

#[test]
fn every_relative_link_goes_somewhere() {
    let mut broken = Vec::new();
    for path in markdown_files() {
        let text = fs::read_to_string(&path).unwrap();
        for piece in text.split("](").skip(1) {
            let Some(end) = piece.find(')') else {
                continue;
            };
            let target = &piece[..end];
            if target.starts_with("http") {
                continue;
            }
            let (file, anchor) = target.split_once('#').unwrap_or((target, ""));
            let destination = if file.is_empty() {
                path.clone()
            } else {
                path.parent().unwrap().join(file)
            };
            let exists = destination.exists();
            let anchored = anchor.is_empty()
                || destination
                    .extension()
                    .is_none_or(|extension| extension != "md")
                || (exists && anchors(&fs::read_to_string(&destination).unwrap()).contains(anchor));
            if !exists || !anchored {
                broken.push(format!("{}: {target}", path.display()));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "links to nothing:\n{}",
        broken.join("\n")
    );
}

#[test]
fn the_prose_keeps_to_the_style() {
    let mut found = Vec::new();
    let mut files = markdown_files();
    files.push(root().join("docs/paper/bimetal.tex"));
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        for (number, line) in text.lines().enumerate() {
            if line.contains('\u{2014}') || line.contains('\u{2013}') {
                found.push(format!("{}:{}", path.display(), number + 1));
            }
        }
    }
    assert!(found.is_empty(), "em or en dashes at {}", found.join(", "));
}

#[test]
fn the_readme_stays_short() {
    let text = fs::read_to_string(root().join("README.md")).unwrap();
    let mut open = false;
    let mut words = 0;
    for line in text.lines() {
        if line.starts_with("```") {
            open = !open;
        } else if !open {
            words += line.split_whitespace().count();
        }
    }
    assert!(
        words <= README_WORDS,
        "the README has {words} words of prose"
    );
}
