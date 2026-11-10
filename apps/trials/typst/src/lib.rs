//! Typst, the typesetting system, behind Bimetal: compile markup to PDF, SVG, and
//! PNG in Python, in Node.js, and in a browser.

mod world;

use bimetal::Model;
use serde::{Deserialize, Serialize};
use typst::diag::{Severity, SourceDiagnostic};
use typst_layout::PagedDocument;
use typst_pdf::PdfOptions;
use typst_render::RenderOptions;
use typst_svg::SvgOptions;

use crate::world::Files;

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// The document cannot compile.
    Error,
    /// The document compiled, but something looks wrong.
    Warning,
}

/// One problem that the compiler found.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
pub struct Diagnostic {
    /// How serious the problem is.
    pub level: Level,
    /// What is wrong.
    pub message: String,
    /// The virtual path of the file, when the problem has a location.
    pub path: Option<String>,
    /// The zero-based line, when the problem has a location.
    pub line: Option<u32>,
    /// The zero-based column, when the problem has a location.
    pub column: Option<u32>,
    /// Suggestions for fixing the problem.
    pub hints: Vec<String>,
}

/// Why a document did not compile or export.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum TypstError {
    /// The markup has errors.
    #[error("{}", summary(diagnostics))]
    #[bimetal(details)]
    Compile {
        /// Every error, with its location.
        diagnostics: Vec<Diagnostic>,
    },
    /// A page number is past the end of the document.
    #[error("page {page} is out of range; the document has {count} pages")]
    #[bimetal(details)]
    PageOutOfRange {
        /// The page asked for.
        page: u32,
        /// How many pages the document has.
        count: u32,
    },
    /// A file was added under a path that Typst cannot resolve.
    #[error("invalid path: {0}")]
    InvalidPath(String),
    /// A font file could not be read.
    #[error("not a font this compiler can read")]
    InvalidFont,
    /// The PDF or PNG export failed.
    #[error("export failed: {0}")]
    Export(String),
}

fn summary(diagnostics: &[Diagnostic]) -> String {
    match diagnostics {
        [only] => only.message.clone(),
        many => format!("{} errors, the first: {}", many.len(), many[0].message),
    }
}

/// A set of virtual files and fonts that documents compile against.
pub struct Compiler {
    files: Files,
}

#[bimetal::export]
impl Compiler {
    /// Create a compiler with Typst's bundled fonts and no files.
    #[bimetal(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            files: Files::new(),
        }
    }

    /// Add or replace a source file, such as `main.typ`.
    pub fn add_source(&mut self, path: &str, text: &str) -> Result<(), TypstError> {
        self.files.add(path, text.as_bytes().to_vec())
    }

    /// Add or replace a binary file, such as an image or a CSV file.
    pub fn add_file(
        &mut self,
        path: &str,
        #[bimetal(bytes)] data: &[u8],
    ) -> Result<(), TypstError> {
        self.files.add(path, data.to_vec())
    }

    /// Add a font, and return how many faces the file holds.
    pub fn add_font(&mut self, #[bimetal(bytes)] data: &[u8]) -> Result<u32, TypstError> {
        self.files.add_font(data)
    }

    /// How many font faces this compiler knows.
    #[bimetal(getter)]
    pub fn font_count(&self) -> u32 {
        self.files.font_count()
    }

    /// Compile a document from the file at `main`.
    #[bimetal(returns(resource))]
    pub fn compile(&self, main: &str) -> Result<Document, TypstError> {
        let warned = typst::compile::<PagedDocument>(&self.files.world(main)?);
        let world = self.files.world(main)?;
        let warnings = warned
            .warnings
            .iter()
            .map(|diagnostic| world.describe(diagnostic))
            .collect();
        match warned.output {
            Ok(document) => Ok(Document { document, warnings }),
            Err(errors) => Err(TypstError::Compile {
                diagnostics: errors.iter().map(|error| world.describe(error)).collect(),
            }),
        }
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

/// A compiled document.
pub struct Document {
    document: PagedDocument,
    warnings: Vec<Diagnostic>,
}

#[bimetal::export]
impl Document {
    /// How many pages the document has.
    #[bimetal(len)]
    pub fn page_count(&self) -> u32 {
        self.document.pages().len() as u32
    }

    /// What the compiler warned about.
    #[bimetal(getter)]
    pub fn warnings(&self) -> Vec<Diagnostic> {
        self.warnings.clone()
    }

    /// Walk the pages as SVG documents.
    #[bimetal(returns(resource))]
    pub fn pages(&self) -> Pages {
        Pages {
            svgs: self
                .document
                .pages()
                .iter()
                .map(|page| typst_svg::svg(page, &SvgOptions::default()))
                .collect::<Vec<_>>()
                .into_iter(),
        }
    }

    /// Export the whole document as a PDF file.
    #[bimetal(returns(bytes))]
    pub fn pdf(&self) -> Result<Vec<u8>, TypstError> {
        typst_pdf::pdf(&self.document, &PdfOptions::default()).map_err(|errors| {
            TypstError::Export(
                errors
                    .first()
                    .map_or_else(String::new, |e| e.message.to_string()),
            )
        })
    }

    /// Export one zero-based page as an SVG document.
    pub fn svg(&self, page: u32) -> Result<String, TypstError> {
        let page = self.page(page)?;
        Ok(typst_svg::svg(page, &SvgOptions::default()))
    }

    /// Export one zero-based page as a PNG image.
    #[bimetal(returns(bytes))]
    pub fn png(
        &self,
        page: u32,
        #[bimetal(default = 2.0)] pixels_per_point: f32,
    ) -> Result<Vec<u8>, TypstError> {
        let page = self.page(page)?;
        let options = RenderOptions {
            pixel_per_pt: typst::utils::Scalar::new(f64::from(pixels_per_point)),
            ..RenderOptions::default()
        };
        typst_render::render(page, &options)
            .encode_png()
            .map_err(|error| TypstError::Export(error.to_string()))
    }
}

/// The pages of a document, one SVG document at a time.
pub struct Pages {
    svgs: std::vec::IntoIter<String>,
}

#[bimetal::export]
impl Pages {
    /// The next page, or nothing at the end.
    #[bimetal(next)]
    pub fn next_page(&mut self) -> Option<String> {
        self.svgs.next()
    }
}

impl Document {
    fn page(&self, page: u32) -> Result<&typst_layout::Page, TypstError> {
        let pages = self.document.pages();
        pages.get(page as usize).ok_or(TypstError::PageOutOfRange {
            page,
            count: pages.len() as u32,
        })
    }
}

pub(crate) fn level(severity: Severity) -> Level {
    match severity {
        Severity::Error => Level::Error,
        Severity::Warning => Level::Warning,
    }
}

pub(crate) type Raw = SourceDiagnostic;

/// The version of Typst behind this package.
#[bimetal::export]
#[must_use]
pub fn typst_version() -> String {
    typst::syntax::package::PackageVersion::compiler().to_string()
}
