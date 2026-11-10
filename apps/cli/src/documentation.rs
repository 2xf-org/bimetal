//! Host-neutral parsing of documentation attached to Rust declarations.
//!
//! Rustdoc is the contract's source documentation, but its Markdown section
//! names are not the public conventions of either generated language. This
//! module separates semantic sections before the Python and TypeScript
//! renderers lower them into Google docstrings and TSDoc, respectively, and
//! wraps the comments of both. A paragraph's lines are joined first, so that
//! each comment wraps it once, at its own width.

use std::collections::BTreeMap;

use bimetal_ir::{CallableDef, TypeRef};

/// Whether a generated callable returns a value, which its documentation
/// then describes.
#[derive(Clone, Copy)]
pub(crate) enum CallableReturn {
    /// Nothing, as for `__init__` or a function that returns unit.
    Nothing,
    /// A contract value or a resource.
    Value,
}

impl CallableReturn {
    /// Return whether a callable whose return type is `reference` returns a
    /// value.
    #[must_use]
    pub(crate) fn from_type(reference: &TypeRef) -> Self {
        if matches!(reference, TypeRef::Unit) {
            Self::Nothing
        } else {
            Self::Value
        }
    }
}

/// Host-neutral inputs required to document one generated callable.
pub(crate) struct CallableDocumentation<'manifest> {
    /// The callable, with its Rustdoc, parameters, and typed error.
    pub(crate) callable: &'manifest CallableDef,
    /// Summary used when the Rust declaration has no documentation.
    pub(crate) fallback_summary: String,
    pub(crate) returns: CallableReturn,
}

impl CallableDocumentation<'_> {
    /// Choose what both hosts document of the callable, which each host only
    /// formats.
    #[must_use]
    pub(crate) fn sections(&self) -> CallableSections {
        let Documentation {
            summary,
            notes,
            parameters,
            returns,
            errors,
            examples,
        } = Documentation::parse(self.callable.docs.as_deref());
        CallableSections {
            summary: if summary.is_empty() {
                self.fallback_summary.clone()
            } else {
                summary
            },
            notes,
            parameters: self
                .callable
                .params
                .iter()
                .map(|param| {
                    parameters
                        .get(&param.rust_name)
                        .or_else(|| parameters.get(&param.host_name))
                        .filter(|description| !description.is_empty())
                        .cloned()
                })
                .collect(),
            returns: match self.returns {
                CallableReturn::Value => returns,
                CallableReturn::Nothing => String::new(),
            },
            errors,
            examples,
        }
    }
}

/// What the generated documentation of one callable says, as Rustdoc
/// writes it. An empty section is absent.
pub(crate) struct CallableSections {
    /// The summary, or the fallback summary when the Rust docs have none.
    pub(crate) summary: String,
    /// Additional prose and explicit notes.
    pub(crate) notes: String,
    /// The description of each parameter, in order, where the Rust docs give
    /// one, by its Rust name or its host name.
    pub(crate) parameters: Vec<Option<String>>,
    /// The description of the returned value, when the callable returns one.
    pub(crate) returns: String,
    /// The conditions under which the callable fails with its typed error.
    pub(crate) errors: String,
    /// Example prose or source.
    pub(crate) examples: String,
}

/// Documentation sections understood by the host-language renderers.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Documentation {
    /// The opening summary paragraph.
    pub(crate) summary: String,
    /// Additional prose and explicit notes.
    pub(crate) notes: String,
    /// Parameter descriptions keyed by their Rust names.
    pub(crate) parameters: BTreeMap<String, String>,
    /// Description of a successful return value.
    pub(crate) returns: String,
    /// Conditions described by Rustdoc's errors section.
    pub(crate) errors: String,
    /// Example prose or source supplied by the author.
    pub(crate) examples: String,
}

impl Documentation {
    /// Parse normalized Rustdoc text into host-neutral semantic sections.
    #[must_use]
    pub(crate) fn parse(source: Option<&str>) -> Self {
        let Some(source) = source.filter(|source| !source.trim().is_empty()) else {
            return Self::default();
        };
        let source = reflow(source);

        let mut description = Vec::new();
        let mut notes = Vec::new();
        let mut arguments = Vec::new();
        let mut returns = Vec::new();
        let mut errors = Vec::new();
        let mut examples = Vec::new();
        let mut section = Section::Description;

        for line in source.lines() {
            if let Some((next, unknown_heading)) = section_heading(line) {
                section = next;
                if let Some(heading) = unknown_heading {
                    notes.push(format!("{heading}:"));
                }
                continue;
            }
            match section {
                Section::Description => description.push(line.to_owned()),
                Section::Notes => notes.push(line.to_owned()),
                Section::Arguments => arguments.push(line.to_owned()),
                Section::Returns => returns.push(line.to_owned()),
                Section::Errors => errors.push(line.to_owned()),
                Section::Examples => examples.push(line.to_owned()),
            }
        }

        let (summary, trailing_description) = split_summary(&description);
        append_block(&mut notes, &trailing_description);
        let (parameters, unmatched_arguments) = parse_arguments(&arguments);
        append_block(&mut notes, &unmatched_arguments);

        Self {
            summary: normalize_block(&summary),
            notes: normalize_block(&notes),
            parameters,
            returns: normalize_block(&returns),
            errors: normalize_block(&errors),
            examples: normalize_block(&examples),
        }
    }
}

/// The part of a Rustdoc comment that a line belongs to.
#[derive(Clone, Copy)]
enum Section {
    Description,
    Notes,
    Arguments,
    Returns,
    Errors,
    Examples,
}

/// Recognize conventional Rustdoc headings and Google-style section labels.
#[must_use]
fn section_heading(line: &str) -> Option<(Section, Option<String>)> {
    let trimmed = line.trim();
    let (heading, markdown) = if trimmed.starts_with('#') {
        (trimmed.trim_start_matches('#').trim(), true)
    } else if let Some(heading) = trimmed.strip_suffix(':') {
        (heading.trim(), false)
    } else {
        return None;
    };
    let normalized = heading.to_ascii_lowercase();
    let section = match normalized.as_str() {
        "arguments" | "args" | "parameters" | "params" => Section::Arguments,
        "returns" | "return" | "return value" => Section::Returns,
        "errors" | "raises" | "throws" => Section::Errors,
        "notes" | "note" | "remarks" => Section::Notes,
        "examples" | "example" => Section::Examples,
        _ if markdown => return Some((Section::Notes, Some(heading.to_owned()))),
        _ => return None,
    };
    Some((section, None))
}

/// Split the first prose paragraph from subsequent explanatory paragraphs.
#[must_use]
fn split_summary(lines: &[String]) -> (Vec<String>, Vec<String>) {
    let lines = trim_blank_lines(lines);
    let boundary = lines
        .iter()
        .position(|line| line.trim().is_empty())
        .unwrap_or(lines.len());
    let summary = lines[..boundary].to_vec();
    let trailing = lines.get(boundary + 1..).unwrap_or_default().to_vec();
    (summary, trailing)
}

/// Parse common Rustdoc parameter-list spellings without discarding prose.
#[must_use]
fn parse_arguments(lines: &[String]) -> (BTreeMap<String, String>, Vec<String>) {
    let mut parameters: BTreeMap<String, String> = BTreeMap::new();
    let mut unmatched = Vec::new();
    let mut current: Option<String> = None;

    for line in trim_blank_lines(lines) {
        if let Some((name, description)) = parameter_entry(&line) {
            parameters
                .entry(name.clone())
                .and_modify(|existing| append_sentence(existing, &description))
                .or_insert(description);
            current = Some(name);
        } else if line.trim().is_empty() {
            current = None;
        } else if let Some(name) = &current {
            append_sentence(
                parameters
                    .get_mut(name)
                    .expect("the current parameter was inserted"),
                line.trim(),
            );
        } else {
            unmatched.push(line);
        }
    }
    (parameters, unmatched)
}

/// Parse one bullet such as ``- `name` - description`` or `name: description`.
#[must_use]
fn parameter_entry(line: &str) -> Option<(String, String)> {
    let mut value = line.trim();
    let mut bullet = false;
    if let Some(rest) = value
        .strip_prefix("- ")
        .or_else(|| value.strip_prefix("* "))
    {
        value = rest.trim();
        bullet = true;
    }

    let (name, rest) = if let Some(value) = value.strip_prefix('`') {
        let end = value.find('`')?;
        (&value[..end], &value[end + 1..])
    } else if let Some(end) = value.find(':') {
        (&value[..end], &value[end..])
    } else if bullet {
        let end = value.find([' ', '-'])?;
        (&value[..end], &value[end..])
    } else {
        return None;
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let description = rest
        .trim_start()
        .strip_prefix([':', '-'])
        .unwrap_or(rest.trim_start())
        .trim()
        .to_owned();
    Some((name.to_owned(), description))
}

/// Remove Rustdoc's link brackets while retaining readable inline-code text.
#[must_use]
pub(crate) fn remove_rustdoc_link_brackets(value: &str) -> String {
    value.replace("[`", "`").replace("`]", "`")
}

/// Make the conventional opening of a Rustdoc errors section read naturally
/// as a host-language exception description.
#[must_use]
pub(crate) fn contextualize_error_description(value: &str) -> String {
    value.strip_prefix("Returns ").map_or_else(
        || value.to_owned(),
        |conditions| format!("The Rust implementation returns {conditions}"),
    )
}

/// Join the lines of each Markdown paragraph, whose breaks are soft, so that
/// a comment wraps it once, at its own width.
///
/// A line continues the paragraph above it only at that paragraph's
/// continuation: its own indentation, a list item's content, or four columns
/// deeper for an indented `name: description` entry, as Google-style
/// sections write them. Blank lines, fenced code, code indented after a blank
/// line, headings, section labels such as `Args:`, quotes and doctests,
/// tables, and the lines after a line that ends with `:` or a hard break keep
/// their breaks.
#[must_use]
pub(crate) fn reflow(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    // The indentation at which a line continues the last one, while that one
    // is prose.
    let mut continuation: Option<usize> = None;
    let mut fenced = false;
    let mut indented_code = false;
    let mut after_blank = true;
    for line in text.lines() {
        let content = line.trim_start();
        let indent = line.len() - content.len();
        if content.is_empty() {
            lines.push(String::new());
            continuation = None;
            after_blank = true;
            continue;
        }
        let fence = content.starts_with("```") || content.starts_with("~~~");
        indented_code &= indent >= 4;
        indented_code |= after_blank && indent >= 4;
        after_blank = false;
        if fenced || fence || indented_code {
            fenced ^= fence;
            lines.push(line.to_owned());
            continuation = None;
            continue;
        }
        let joined = match (continuation, lines.last_mut()) {
            (Some(expected), Some(last)) if expected == indent && !opens_block(content, indent) => {
                last.truncate(last.trim_end().len());
                last.push(' ');
                last.push_str(content);
                true
            }
            _ => false,
        };
        if !joined {
            lines.push(line.to_owned());
            continuation = continues_at(content, indent);
        }
        if ends_paragraph(line) {
            continuation = None;
        }
    }
    lines.join("\n")
}

/// Return whether `content`, a line without its `indent`, opens a block of its
/// own rather than continuing a paragraph.
fn opens_block(content: &str, indent: usize) -> bool {
    list_marker(content).is_some()
        || content.starts_with(['#', '>', '|'])
        || section_heading(content).is_some()
        || (indent > 0 && entry_name(content).is_some())
}

/// Return the indentation at which a line continues the one that `content`
/// at `indent` opens, or `None` when nothing continues it.
fn continues_at(content: &str, indent: usize) -> Option<usize> {
    if content.starts_with(['#', '>', '|']) {
        None
    } else if let Some(marker) = list_marker(content) {
        Some(indent + marker)
    } else if indent > 0 && entry_name(content).is_some() {
        Some(indent + 4)
    } else {
        Some(indent)
    }
}

/// Return whether nothing may continue `line`: it ends with a label's `:`,
/// or with a Markdown hard break.
fn ends_paragraph(line: &str) -> bool {
    line.trim_end().ends_with(':') || line.ends_with("  ") || line.ends_with('\\')
}

/// Return the width of the list marker that opens `content`, such as `- ` or
/// `1. `, with its space.
fn list_marker(content: &str) -> Option<usize> {
    if ["- ", "* ", "+ "]
        .iter()
        .any(|marker| content.starts_with(marker))
    {
        return Some(2);
    }
    let digits = content.chars().take_while(char::is_ascii_digit).count();
    let rest = &content[digits..];
    (digits > 0 && (rest.starts_with(". ") || rest.starts_with(") "))).then_some(digits + 2)
}

/// Return the name of a `name: description` entry that opens `content`.
pub(crate) fn entry_name(content: &str) -> Option<&str> {
    let (name, _) = content
        .split_once(": ")
        .or_else(|| content.strip_suffix(':').map(|name| (name, "")))?;
    let name = name
        .strip_prefix('`')
        .and_then(|name| name.strip_suffix('`'))
        .unwrap_or(name);
    let identifier = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_');
    identifier.then_some(name)
}

/// Wrap the lines of `text`, which are already escaped for their comment,
/// at `width` characters, between words. A wrapped line continues at its
/// own indentation, deeper by `hanging(content, indent)` for a line that
/// opens an entry, such as an argument. Lines inside a Markdown code fence, and words
/// longer than a line, stay whole, and blank lines become empty.
#[must_use]
pub(crate) fn wrap(
    text: &str,
    width: usize,
    hanging: impl Fn(&str, usize) -> usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let content = line.trim_start();
        let fence = content.starts_with("```");
        if content.is_empty() {
            lines.push(String::new());
            continue;
        }
        if fenced || fence {
            fenced ^= fence;
            lines.push(line.to_owned());
            continue;
        }
        let indent = line.len() - content.len();
        let continuation = " ".repeat(indent + hanging(content, indent));
        let mut prefix = &line[..indent];
        let mut remaining = content;
        while prefix.chars().count() + remaining.chars().count() > width {
            let available = width.saturating_sub(prefix.chars().count());
            let mut boundary = None;
            for (count, (index, character)) in remaining.char_indices().enumerate() {
                if count > available {
                    break;
                }
                if character.is_whitespace() && index > 0 {
                    boundary = Some(index);
                }
            }
            let Some(boundary) = boundary else {
                break;
            };
            lines.push(format!("{prefix}{}", remaining[..boundary].trim_end()));
            remaining = remaining[boundary..].trim_start();
            prefix = &continuation;
        }
        lines.push(format!("{prefix}{remaining}"));
    }
    lines
}

#[must_use]
fn normalize_block(lines: &[String]) -> String {
    trim_blank_lines(lines)
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

#[must_use]
fn trim_blank_lines(lines: &[String]) -> Vec<String> {
    let first = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let last = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(first, |index| index + 1);
    lines[first..last].to_vec()
}

fn append_block(target: &mut Vec<String>, block: &[String]) {
    let block = trim_blank_lines(block);
    if block.is_empty() {
        return;
    }
    if target.iter().any(|line| !line.trim().is_empty()) {
        target.push(String::new());
    }
    target.extend(block);
}

fn append_sentence(target: &mut String, value: &str) {
    if value.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push(' ');
    }
    target.push_str(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rustdoc_into_language_neutral_sections() {
        let documentation = Documentation::parse(Some(
            "Perform one operation.\n\nAdditional implementation detail.\n\n# Arguments\n\n- `value` - Value to process.\n  Continued detail.\n\n# Returns\n\nThe processed value.\n\n# Errors\n\nReturns `ExampleError` when validation fails.\n\n# Examples\n\n```text\nexample\n```",
        ));

        assert_eq!(documentation.summary, "Perform one operation.");
        assert_eq!(documentation.notes, "Additional implementation detail.");
        assert_eq!(
            documentation.parameters.get("value").map(String::as_str),
            Some("Value to process. Continued detail.")
        );
        assert_eq!(documentation.returns, "The processed value.");
        assert_eq!(
            documentation.errors,
            "Returns `ExampleError` when validation fails."
        );
        assert_eq!(documentation.examples, "```text\nexample\n```");
    }

    #[test]
    fn removes_only_rustdoc_link_brackets() {
        assert_eq!(
            remove_rustdoc_link_brackets("Call [`decode`] with `bytes`."),
            "Call `decode` with `bytes`."
        );
    }

    #[test]
    fn wraps_between_words_and_leaves_code_whole() {
        let hanging = |line: &str, _| if line.starts_with('@') { 2 } else { 0 };
        assert_eq!(
            wrap(
                "one two three four\n@param five six seven\n\n```\nlong code line\n```\nunbreakable",
                10,
                hanging,
            ),
            [
                "one two",
                "three four",
                "@param",
                "  five six",
                "  seven",
                "",
                "```",
                "long code line",
                "```",
                "unbreakable",
            ]
        );
    }

    #[test]
    fn joins_a_paragraph_and_keeps_every_other_block() {
        let source = [
            "Insert or replace the rows of `rows`. Each shard's",
            "part commits on its own.",
            "",
            "- `rows` - The rows,",
            "  in order.",
            "- `wait` - Whether to wait.",
            "",
            "```rust",
            "let a = 1;",
            "let b = 2;",
            "```",
            "",
            "    indented",
            "    code",
            "",
            ">>> table.upsert(rows)",
            ">>> table.count()",
            "3",
            "# Errors",
            "Fails when the table",
            "is gone.",
            "Args:",
            "    rows: The rows",
            "        to write.",
            "    wait: Whether",
            "        to wait.",
            "| a | b |",
            "| 1 | 2 |",
            "A hard  ",
            "break.",
        ]
        .join("\n");

        assert_eq!(
            reflow(&source).lines().collect::<Vec<_>>(),
            [
                "Insert or replace the rows of `rows`. Each shard's part commits on its own.",
                "",
                "- `rows` - The rows, in order.",
                "- `wait` - Whether to wait.",
                "",
                "```rust",
                "let a = 1;",
                "let b = 2;",
                "```",
                "",
                "    indented",
                "    code",
                "",
                ">>> table.upsert(rows)",
                ">>> table.count()",
                "3",
                "# Errors",
                "Fails when the table is gone.",
                "Args:",
                "    rows: The rows to write.",
                "    wait: Whether to wait.",
                "| a | b |",
                "| 1 | 2 |",
                "A hard  ",
                "break.",
            ]
        );
    }

    #[test]
    fn a_paragraph_wraps_once_without_orphans() {
        let documentation = Documentation::parse(Some(
            "Insert or replace the rows of `rows`, a batch with the table's columns. Each shard's\npart commits on its own; the first part that failed raises its error.",
        ));
        let wrapped = wrap(&documentation.summary, 80, |_, _| 0);

        assert_eq!(
            wrapped,
            [
                "Insert or replace the rows of `rows`, a batch with the table's columns. Each",
                "shard's part commits on its own; the first part that failed raises its error.",
            ]
        );
    }

    #[test]
    fn contextualizes_conventional_rustdoc_error_prose() {
        assert_eq!(
            contextualize_error_description("Returns `ExampleError` when validation fails."),
            "The Rust implementation returns `ExampleError` when validation fails."
        );
    }
}
