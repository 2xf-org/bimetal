//! The in-memory world that Typst reads files and fonts from.

use std::collections::HashMap;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World, WorldExt};

use crate::{Diagnostic, Raw, TypstError, level};

/// The files and fonts of one compiler.
pub struct Files {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    files: HashMap<FileId, Bytes>,
}

fn file_id(path: &str) -> Result<FileId, TypstError> {
    let path = VirtualPath::new(path).map_err(|_| TypstError::InvalidPath(path.to_owned()))?;
    Ok(FileId::new(RootedPath::new(VirtualRoot::Project, path)))
}

impl Files {
    pub fn new() -> Self {
        let fonts: Vec<Font> = typst_assets::fonts()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect();
        Self {
            library: LazyHash::new(Library::default()),
            book: LazyHash::new(FontBook::from_fonts(&fonts)),
            fonts,
            files: HashMap::new(),
        }
    }

    pub fn add(&mut self, path: &str, data: Vec<u8>) -> Result<(), TypstError> {
        self.files.insert(file_id(path)?, Bytes::new(data));
        Ok(())
    }

    pub fn add_font(&mut self, data: &[u8]) -> Result<u32, TypstError> {
        let added: Vec<Font> = Font::iter(Bytes::new(data.to_vec())).collect();
        if added.is_empty() {
            return Err(TypstError::InvalidFont);
        }
        self.fonts.extend(added.iter().cloned());
        self.book = LazyHash::new(FontBook::from_fonts(&self.fonts));
        Ok(added.len() as u32)
    }

    pub fn font_count(&self) -> u32 {
        self.fonts.len() as u32
    }

    pub fn world(&self, main: &str) -> Result<Compilation<'_>, TypstError> {
        Ok(Compilation {
            files: self,
            main: file_id(main)?,
        })
    }
}

/// One compilation over a set of files.
pub struct Compilation<'files> {
    files: &'files Files,
    main: FileId,
}

impl Compilation<'_> {
    pub fn describe(&self, diagnostic: &Raw) -> Diagnostic {
        let located = self.range(diagnostic.span).and_then(|range| {
            let id = diagnostic.span.id()?;
            let source = self.source(id).ok()?;
            let lines = source.lines();
            let line = lines.byte_to_line(range.start)?;
            let column = lines.byte_to_column(range.start)?;
            Some((
                id.vpath().get_with_slash().to_owned(),
                line as u32,
                column as u32,
            ))
        });
        let (path, line, column) = match located {
            Some((path, line, column)) => (Some(path), Some(line), Some(column)),
            None => (None, None, None),
        };
        Diagnostic {
            level: level(diagnostic.severity),
            message: diagnostic.message.to_string(),
            path,
            line,
            column,
            hints: diagnostic
                .hints
                .iter()
                .map(|hint| hint.v.to_string())
                .collect(),
        }
    }
}

impl World for Compilation<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.files.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.files.book
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        let bytes = self.file(id)?;
        let text = std::str::from_utf8(bytes.as_slice()).map_err(|_| FileError::InvalidUtf8)?;
        Ok(Source::new(id, text.to_owned()))
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files
            .files
            .get(&id)
            .cloned()
            .ok_or_else(|| FileError::NotFound(id.vpath().get_with_slash().into()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.files.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        None
    }
}
