//! Oxc, the JavaScript toolchain, behind Bimetal: parse, check, transform, and
//! minify JavaScript and TypeScript from Python, from Node.js, and from a
//! browser.
//!
//! Oxc's syntax trees live in an arena that is neither `Send` nor `Sync`, so
//! they cannot be the state of a resource. A [`Module`] keeps its source, and
//! each call builds the tree it needs.

use std::path::Path;

use bimetal::{Callback, Model};
use oxc::allocator::Allocator;
use oxc::ast::ast::{Program, Statement};
use oxc::codegen::{Codegen, CodegenOptions};
use oxc::diagnostics::{OxcDiagnostic, Severity};
use oxc::isolated_declarations::{IsolatedDeclarations, IsolatedDeclarationsOptions};
use oxc::minifier::{Minifier, MinifierOptions};
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::{GetSpan, SourceType};
use oxc::transformer::{TransformOptions, Transformer};
use serde::{Deserialize, Serialize};

/// The language of a source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// JavaScript.
    Js,
    /// JavaScript with JSX.
    Jsx,
    /// TypeScript.
    Ts,
    /// TypeScript with JSX.
    Tsx,
}

impl Language {
    fn source_type(self) -> SourceType {
        match self {
            Self::Js => SourceType::mjs(),
            Self::Jsx => SourceType::jsx(),
            Self::Ts => SourceType::ts(),
            Self::Tsx => SourceType::tsx(),
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Js => "module.js",
            Self::Jsx => "module.jsx",
            Self::Ts => "module.ts",
            Self::Tsx => "module.tsx",
        }
    }
}

/// One problem in a source text.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
pub struct Diagnostic {
    /// What is wrong.
    pub message: String,
    /// Whether the problem stops the tool, rather than only warning.
    pub fatal: bool,
    /// The byte offset where the first label starts, when there is one.
    pub start: Option<u32>,
    /// The byte offset where the first label ends, when there is one.
    pub end: Option<u32>,
    /// A suggestion for fixing the problem.
    pub help: Option<String>,
}

impl From<&OxcDiagnostic> for Diagnostic {
    fn from(diagnostic: &OxcDiagnostic) -> Self {
        let label = diagnostic.labels.as_ref().and_then(|labels| labels.first());
        Self {
            message: diagnostic.message.to_string(),
            fatal: matches!(diagnostic.severity, Severity::Error),
            start: label.map(|label| label.offset() as u32),
            end: label.map(|label| (label.offset() + label.len()) as u32),
            help: diagnostic.help.as_ref().map(ToString::to_string),
        }
    }
}

/// Why a tool could not process a source text.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum OxcError {
    /// The source text has errors.
    #[error("{} errors, the first: {}", diagnostics.len(), diagnostics[0].message)]
    #[bimetal(details)]
    Syntax {
        /// Every error, with its location.
        diagnostics: Vec<Diagnostic>,
    },
    /// The transform failed.
    #[error("{} errors, the first: {}", diagnostics.len(), diagnostics[0].message)]
    #[bimetal(details)]
    Transform {
        /// Every error, with its location.
        diagnostics: Vec<Diagnostic>,
    },
}

fn diagnostics(errors: &[OxcDiagnostic]) -> Vec<Diagnostic> {
    errors.iter().map(Diagnostic::from).collect()
}

/// Generated code and its source map.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
pub struct Output {
    /// The generated code.
    pub code: String,
    /// The source map as JSON, when one was asked for.
    pub map: Option<String>,
}

/// How to generate code.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Model)]
pub struct OutputOptions {
    /// Whether to write a source map.
    #[serde(default)]
    pub source_map: bool,
    /// Whether to rename local variables, when minifying.
    #[serde(default)]
    pub mangle: bool,
}

fn parse<'a>(
    allocator: &'a Allocator,
    source: &'a str,
    language: Language,
) -> Result<Program<'a>, OxcError> {
    let parsed = Parser::new(allocator, source, language.source_type()).parse();
    if parsed.errors.is_empty() {
        Ok(parsed.program)
    } else {
        Err(OxcError::Syntax {
            diagnostics: diagnostics(&parsed.errors),
        })
    }
}

fn generate(
    program: &Program<'_>,
    language: Language,
    minify: bool,
    source_map: bool,
    scoping: Option<oxc::semantic::Scoping>,
) -> Output {
    let options = CodegenOptions {
        minify,
        source_map_path: source_map.then(|| Path::new(language.file_name()).to_path_buf()),
        ..CodegenOptions::default()
    };
    let generated = Codegen::new()
        .with_options(options)
        .with_scoping(scoping)
        .build(program);
    Output {
        code: generated.code,
        map: generated.map.map(|map| map.to_json_string()),
    }
}

/// Report every syntax and semantic problem of a source text.
#[bimetal::export]
#[must_use]
pub fn check(source: &str, #[bimetal(default = "js")] language: Language) -> Vec<Diagnostic> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, language.source_type()).parse();
    if !parsed.errors.is_empty() {
        return diagnostics(&parsed.errors);
    }
    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(&parsed.program);
    diagnostics(&semantic.errors)
}

/// Compile TypeScript and JSX down to JavaScript.
#[bimetal::export]
pub fn transform(
    source: &str,
    #[bimetal(default = "ts")] language: Language,
    options: Option<OutputOptions>,
) -> Result<Output, OxcError> {
    let options = options.unwrap_or_default();
    let allocator = Allocator::default();
    let mut program = parse(&allocator, source, language)?;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();
    let transformed = Transformer::new(
        &allocator,
        Path::new(language.file_name()),
        &TransformOptions::default(),
    )
    .build_with_scoping(scoping, &mut program);
    if !transformed.errors.is_empty() {
        return Err(OxcError::Transform {
            diagnostics: diagnostics(&transformed.errors),
        });
    }
    Ok(generate(
        &program,
        language,
        false,
        options.source_map,
        None,
    ))
}

/// Shrink JavaScript.
#[bimetal::export]
pub fn minify(
    source: &str,
    #[bimetal(default = "js")] language: Language,
    options: Option<OutputOptions>,
) -> Result<Output, OxcError> {
    let options = options.unwrap_or_default();
    let allocator = Allocator::default();
    let mut program = parse(&allocator, source, language)?;
    let minifier = MinifierOptions {
        mangle: options.mangle.then(Default::default),
        ..MinifierOptions::default()
    };
    let minified = Minifier::new(minifier).minify(&allocator, &mut program);
    Ok(generate(
        &program,
        language,
        true,
        options.source_map,
        minified.scoping,
    ))
}

/// Write the `.d.ts` declarations of a TypeScript module.
#[bimetal::export]
pub fn declarations(source: &str) -> Result<String, OxcError> {
    let allocator = Allocator::default();
    let program = parse(&allocator, source, Language::Ts)?;
    let declared = IsolatedDeclarations::new(&allocator, IsolatedDeclarationsOptions::default())
        .build(&program);
    if !declared.errors.is_empty() {
        return Err(OxcError::Transform {
            diagnostics: diagnostics(&declared.errors),
        });
    }
    Ok(Codegen::new().build(&declared.program).code)
}

/// A variable, function, or class that a module declares.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
pub struct Symbol {
    /// The declared name.
    pub name: String,
    /// How many references to it the module holds.
    pub references: u32,
    /// Whether the module scope declares it.
    pub top_level: bool,
}

/// A statement of a module.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
pub struct StatementInfo {
    /// The statement's ESTree kind, such as `FunctionDeclaration`.
    pub kind: String,
    /// The byte offset where it starts.
    pub start: u32,
    /// The byte offset where it ends.
    pub end: u32,
}

fn statements_of(program: &Program<'_>) -> Vec<StatementInfo> {
    program
        .body
        .iter()
        .map(|statement| {
            let span = statement.span();
            StatementInfo {
                kind: kind_of(statement).to_owned(),
                start: span.start,
                end: span.end,
            }
        })
        .collect()
}

fn kind_of(statement: &Statement<'_>) -> &'static str {
    match statement {
        Statement::FunctionDeclaration(_) => "FunctionDeclaration",
        Statement::ClassDeclaration(_) => "ClassDeclaration",
        Statement::VariableDeclaration(_) => "VariableDeclaration",
        Statement::ExpressionStatement(_) => "ExpressionStatement",
        Statement::ImportDeclaration(_) => "ImportDeclaration",
        Statement::ExportNamedDeclaration(_) => "ExportNamedDeclaration",
        Statement::ExportDefaultDeclaration(_) => "ExportDefaultDeclaration",
        Statement::IfStatement(_) => "IfStatement",
        Statement::ReturnStatement(_) => "ReturnStatement",
        _ => "Other",
    }
}

/// A parsed module.
pub struct Module {
    source: String,
    language: Language,
}

#[bimetal::export]
impl Module {
    /// Parse a source text. A syntax error fails with its diagnostics.
    #[bimetal(constructor)]
    pub fn new(
        source: String,
        #[bimetal(default = "js")] language: Language,
    ) -> Result<Self, OxcError> {
        parse(&Allocator::default(), &source, language)?;
        Ok(Self { source, language })
    }

    /// The source text.
    #[bimetal(getter)]
    pub fn source(&self) -> String {
        self.source.clone()
    }

    /// A short description for logs.
    #[bimetal(repr)]
    pub fn describe(&self) -> String {
        format!("Module({:?}, {} bytes)", self.language, self.source.len())
    }

    /// The syntax tree, in the ESTree format that JavaScript tools share.
    pub fn ast(&self) -> serde_json::Value {
        let allocator = Allocator::default();
        let program = parse(&allocator, &self.source, self.language)
            .expect("the constructor parsed this source");
        let json = match self.language {
            Language::Ts | Language::Tsx => program.to_estree_ts_json(true),
            Language::Js | Language::Jsx => program.to_estree_js_json(true),
        };
        serde_json::from_str(&json).expect("Oxc writes valid JSON")
    }

    /// The syntax tree as ESTree JSON text, which crosses far faster than
    /// [`Module::ast`] when the tree is very large.
    pub fn ast_text(&self) -> String {
        let allocator = Allocator::default();
        let program = parse(&allocator, &self.source, self.language)
            .expect("the constructor parsed this source");
        match self.language {
            Language::Ts | Language::Tsx => program.to_estree_ts_json(true),
            Language::Js | Language::Jsx => program.to_estree_js_json(true),
        }
    }

    /// Every symbol that the module declares.
    pub fn symbols(&self) -> Vec<Symbol> {
        let allocator = Allocator::default();
        let program = parse(&allocator, &self.source, self.language)
            .expect("the constructor parsed this source");
        let scoping = SemanticBuilder::new()
            .build(&program)
            .semantic
            .into_scoping();
        let root = scoping.root_scope_id();
        scoping
            .symbol_ids()
            .map(|symbol| Symbol {
                name: scoping.symbol_name(symbol).to_owned(),
                references: scoping.get_resolved_reference_ids(symbol).len() as u32,
                top_level: scoping.symbol_scope_id(symbol) == root,
            })
            .collect()
    }

    /// Whether the module scope declares `name`.
    #[bimetal(contains)]
    pub fn declares(&self, name: &str) -> bool {
        self.symbols()
            .iter()
            .any(|symbol| symbol.top_level && symbol.name == name)
    }

    /// Walk the top-level statements.
    #[bimetal(returns(resource))]
    pub fn statements(&self) -> Statements {
        let allocator = Allocator::default();
        let program = parse(&allocator, &self.source, self.language)
            .expect("the constructor parsed this source");
        Statements {
            statements: statements_of(&program).into_iter(),
        }
    }

    /// Call `visitor` with each top-level statement until it returns false,
    /// and return how many it saw.
    pub fn visit(&self, visitor: Callback<(StatementInfo,), bool>) -> Result<u32, OxcError> {
        let allocator = Allocator::default();
        let program = parse(&allocator, &self.source, self.language)?;
        let mut visited = 0;
        for statement in statements_of(&program) {
            visited += 1;
            let keep_going = visitor
                .call((statement,))
                .map_err(|error| OxcError::Transform {
                    diagnostics: vec![Diagnostic {
                        message: error.to_string(),
                        fatal: true,
                        start: None,
                        end: None,
                        help: None,
                    }],
                })?;
            if !keep_going {
                break;
            }
        }
        Ok(visited)
    }
}

/// The top-level statements of a module.
pub struct Statements {
    statements: std::vec::IntoIter<StatementInfo>,
}

#[bimetal::export]
impl Statements {
    /// The next statement, or nothing at the end.
    #[bimetal(next)]
    pub fn next_statement(&mut self) -> Option<StatementInfo> {
        self.statements.next()
    }
}
