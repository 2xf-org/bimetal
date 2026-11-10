//! Native export names: the symbols that the generated wrappers of every
//! host expose, which the generated host code calls.
//!
//! A name hashes the package, the source location, the kind of export, and
//! its host name, so exports declared in different places get different
//! symbols, and the same source produces the same name on every machine.

use std::env;

use bimetal_ir::{ExportKind, hash};
use proc_macro2::Span;

/// Derive a deterministic, collision-resistant native export symbol.
pub(crate) fn native_export_name(span: Span, kind: ExportKind, public_name: &str) -> String {
    // `Span::unwrap` converts to the compiler's own span, which knows its
    // source file and position. It is not `Option::unwrap`.
    let span = span.unwrap();
    let package = env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "crate".to_owned());
    let local_file = span
        .local_file()
        .map(|path| path.to_string_lossy().into_owned());
    let manifest_dir =
        env::var_os("CARGO_MANIFEST_DIR").map(|path| path.to_string_lossy().into_owned());
    let current_dir = env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let file = local_file.map_or_else(
        || normalize_source_path(&span.file()),
        |path| relative_source_path(&path, manifest_dir.as_deref(), current_dir.as_deref()),
    );
    let identity = format!(
        "{package}\0{file}\0{}\0{}\0{}\0{public_name}",
        span.line(),
        span.column(),
        kind.name()
    );
    let package: String = package
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{}{package}_{:016x}",
        kind.prefix(),
        hash::fnv1a(identity.as_bytes())
    )
}

/// Convert a compiler source path into a reproducible workspace-relative path.
fn relative_source_path(
    file: &str,
    manifest_dir: Option<&str>,
    current_dir: Option<&str>,
) -> String {
    let mut file = normalize_source_path(file);
    if !is_absolute_source_path(&file)
        && let Some(current_dir) = current_dir
    {
        file = normalize_source_path(&format!("{current_dir}/{file}"));
    }
    let Some(manifest_dir) = manifest_dir else {
        return file;
    };
    let manifest_dir = normalize_source_path(manifest_dir);
    file.strip_prefix(&manifest_dir)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .unwrap_or(&file)
        .to_owned()
}

/// Return whether a source path is absolute on Unix or Windows.
fn is_absolute_source_path(path: &str) -> bool {
    path.starts_with('/') || path.as_bytes().get(1) == Some(&b':')
}

/// Normalize platform separators, the Windows verbatim prefix, the drive
/// letter's case, and trailing separators in a source path.
fn normalize_source_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut path = path.strip_prefix("//?/").unwrap_or(&path).to_owned();
    if path.as_bytes().get(1) == Some(&b':') {
        let drive = path[..1].to_ascii_lowercase();
        path.replace_range(0..1, &drive);
    }
    while path.ends_with('/') {
        path.pop();
    }
    path
}
