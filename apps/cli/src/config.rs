//! Loading and comment-preserving updates of `bimetal.toml`.
//!
//! The `[application]` table and the host tables are user-owned and retained
//! byte-for-byte whenever generated ownership is published. Only the
//! `[generated]` subtree is reconstructed, which keeps configuration edits and
//! comments stable across builds.

use std::env;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml_edit::{Decor, Document, DocumentMut, Item, Table, TomlError, de};

use crate::literal;
use crate::paths;

pub(super) const CONFIG_FILE: &str = "bimetal.toml";

/// A host language that an application can generate a package for.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Host {
    Python,
    TypeScript,
}

impl Host {
    /// Return the name of the host's tables in `bimetal.toml`, such as
    /// `python` for `[python]` and `[generated.python]`.
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
        }
    }

    /// Return the project that the host's package lives in when its table
    /// names none.
    #[must_use]
    pub(crate) const fn default_project(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
        }
    }
}

/// The package of one enabled host, with its project as `bimetal.toml`
/// resolves it.
#[derive(Debug, Clone)]
pub(crate) struct HostPackage {
    pub(crate) host: Host,
    /// The Python import package, or the npm package name.
    pub(crate) package: String,
    /// The user-owned project that holds the package.
    pub(crate) project: PathBuf,
}

/// The user-owned tables of `bimetal.toml`. A host table, even an empty one,
/// turns its host on.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct Settings {
    /// Settings that apply to every host.
    pub(crate) application: ApplicationSettings,
    /// The Python package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) python: Option<PythonSettings>,
    /// The npm package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) typescript: Option<TypeScriptSettings>,
}

/// The settings of `[application]`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ApplicationSettings {
    /// Optional public application-name override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// Additional workspace crates linked into the application.
    pub(crate) additional_packages: Vec<String>,
    /// What names each package's namespaces.
    pub(crate) namespaces: Namespaces,
}

/// What names the namespaces of each package of the application.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Namespaces {
    /// The Cargo package name, without the `-`-separated prefix that all the
    /// packages share. The application package's declarations take the
    /// root only when its name is that prefix, as `app` is beside
    /// `app-hardware`.
    #[default]
    Packages,
    /// The package's folder below the deepest folder that holds all of
    /// them, one segment per folder. The application package's declarations
    /// take the root wherever its folder is.
    Folders,
}

/// The settings of `[python]`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PythonSettings {
    /// Optional import-package override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) package: Option<String>,
    /// Optional project-folder override, relative to `bimetal.toml`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
}

/// The settings of `[typescript]`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TypeScriptSettings {
    /// Optional project-folder override, relative to `bimetal.toml`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    /// How the package runs the Rust code.
    pub(crate) backend: Backend,
}

/// How the npm package runs the Rust code.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Backend {
    /// A WebAssembly module, which every JavaScript runtime loads.
    #[default]
    Wasm,
    /// A native Node.js addon.
    Node,
}

/// Complete Bimetal-owned state persisted after a successful publication.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GeneratedState {
    /// Fingerprint of Rust inputs and the user-owned tables.
    pub(crate) source_fingerprint: String,
    /// Fingerprint of the published output files, used to skip clean rebuilds.
    pub(crate) output_fingerprint: Option<String>,
    /// The files of the checkout that the last WebAssembly build read, or
    /// the contract discovery build without one, such as Rust sources and
    /// `include_str!` files, relative to the Cargo workspace. The source
    /// fingerprint covers them.
    pub(crate) inputs: Vec<String>,
    /// The generated Python files.
    pub(crate) python: Option<HostRecord>,
    /// The generated TypeScript, JavaScript, and Wasm files.
    pub(crate) typescript: Option<HostRecord>,
}

impl GeneratedState {
    /// Return the record of each host that has one.
    pub(crate) fn records(&self) -> impl Iterator<Item = (Host, &HostRecord)> {
        [Host::Python, Host::TypeScript]
            .into_iter()
            .filter_map(|host| Some((host, self.record(host)?)))
    }

    /// Return the record of `host`, which is absent for a disabled host.
    #[must_use]
    pub(crate) fn record(&self, host: Host) -> Option<&HostRecord> {
        match host {
            Host::Python => self.python.as_ref(),
            Host::TypeScript => self.typescript.as_ref(),
        }
    }

    /// Return the record of `host` to fill in or clear.
    #[must_use]
    pub(crate) fn record_mut(&mut self, host: Host) -> &mut Option<HostRecord> {
        match host {
            Host::Python => &mut self.python,
            Host::TypeScript => &mut self.typescript,
        }
    }
}

/// Equality tracks ownership only; `output_fingerprint` is a cache key and
/// must not make otherwise-identical states compare unequal.
impl PartialEq for GeneratedState {
    fn eq(&self, other: &Self) -> bool {
        self.source_fingerprint == other.source_fingerprint
            && self.inputs == other.inputs
            && self.python == other.python
            && self.typescript == other.typescript
    }
}

impl Eq for GeneratedState {}

/// What Bimetal generated for one host, as `[generated.<host>]` records it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostRecord {
    /// The host's project, relative to `bimetal.toml`.
    pub(crate) project: String,
    /// The files that Bimetal owns, relative to the project.
    pub(crate) files: Vec<String>,
    /// The packages that the generated code imports, which the project's
    /// manifest must declare.
    pub(crate) dependencies: Vec<String>,
}

/// Deserialization-only shape of the complete configuration document.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigData {
    application: ApplicationSettings,
    python: Option<PythonSettings>,
    typescript: Option<TypeScriptSettings>,
    generated: GeneratedState,
}

/// Parsed configuration plus the original editable TOML document.
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Canonical path to `bimetal.toml`.
    pub(crate) path: PathBuf,
    /// The user-owned tables.
    pub(crate) settings: Settings,
    /// Last successfully published generated state.
    pub(crate) generated: GeneratedState,
    document: DocumentMut,
}

impl Config {
    /// Return the configuration that a command uses: `path`, or the
    /// `bimetal.toml` in it when it is a directory. Without a path, it is the
    /// `bimetal.toml` of the current directory or of the nearest parent that
    /// has one, the way Cargo finds `Cargo.toml`.
    pub(crate) fn find(path: Option<&Path>) -> Result<PathBuf> {
        match path {
            Some(path) if path.is_dir() => Ok(path.join(CONFIG_FILE)),
            Some(path) => Ok(path.to_path_buf()),
            None => {
                let current = env::current_dir().context("cannot read the current directory")?;
                current
                    .ancestors()
                    .map(|directory| directory.join(CONFIG_FILE))
                    .find(|path| path.is_file())
                    .with_context(|| {
                        format!(
                            "cannot find {CONFIG_FILE} in {} or a parent directory; run the command inside a Bimetal application, or pass `--config <path>`",
                            current.display()
                        )
                    })
            }
        }
    }

    /// Parse a configuration while retaining its comments and formatting.
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let path = paths::canonical(path)
            .with_context(|| format!("cannot find Bimetal configuration {}", path.display()))?;
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let generated = generated_offset(&source);
        let in_generated = |span: Option<Range<usize>>| {
            generated
                .zip(span)
                .is_some_and(|(generated, span)| span.start >= generated)
        };
        let parsed = Document::parse(source).map_err(|error: TomlError| {
            let in_generated = in_generated(error.span());
            let error =
                anyhow::Error::new(error).context(format!("failed to parse {}", path.display()));
            if in_generated {
                // Parallel builds conflict on the fingerprints, and any side
                // of the conflict is a start that the next build corrects.
                error.context(
                    "the [generated] tables of bimetal.toml are not valid TOML, as after a merge conflict in them. Bimetal rewrites them: keep either side of the conflict, or delete the tables, and run `bimetal build`",
                )
            } else {
                error
            }
        })?;
        let data: ConfigData =
            toml_edit::de::from_document(parsed.clone()).map_err(|error: de::Error| {
                let in_generated = in_generated(error.span());
                let error = anyhow::Error::new(error)
                    .context(format!("invalid Bimetal configuration in {}", path.display()));
                if in_generated {
                    error.context(
                        "the [generated] tables of bimetal.toml are not what this Bimetal writes: delete them, and run `bimetal build`",
                    )
                } else {
                    error
                }
            })?;
        let settings = Settings {
            application: data.application,
            python: data.python,
            typescript: data.typescript,
        };
        if settings.python.is_none() && settings.typescript.is_none() {
            bail!(
                "{} turns on no host: add a [python] or [typescript] table, even an empty one, for each package to generate",
                path.display()
            );
        }
        Ok(Self {
            path,
            settings,
            generated: data.generated,
            document: parsed.into_mut(),
        })
    }

    /// Return the application root containing this configuration.
    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        self.path
            .parent()
            .expect("a canonical configuration path has a parent")
    }

    /// Replace only the generated subtree in the retained TOML document.
    pub(crate) fn render_generated(&self, generated: &GeneratedState) -> Result<String> {
        let mut fragment: DocumentMut = generated_tables(generated)
            .parse()
            .context("failed to render generated Bimetal configuration")?;
        let mut document = self.document.clone();
        let generated_decor = match document.remove("generated") {
            Some(Item::Table(table)) => table.decor().clone(),
            // toml_edit keeps comments after the last key as the document's
            // trailing text, which must stay above the first [generated].
            _ => {
                let trailing = document.trailing().as_str().unwrap_or_default();
                let decor = Decor::new(format!("{trailing}\n"), "");
                document.set_trailing("");
                decor
            }
        };
        let mut tables = fragment
            .remove("generated")
            .context("rendered Bimetal configuration omitted [generated]")?;
        let table = tables
            .as_table_mut()
            .context("rendered Bimetal configuration has no [generated] table")?;
        *table.decor_mut() = generated_decor;
        // toml_edit writes tables in the order of their positions, which the
        // fragment numbered from its own start.
        let mut position = last_position(document.as_table());
        position += 1;
        table.set_position(Some(position));
        for (_, item) in table.iter_mut() {
            if let Item::Table(record) = item {
                position += 1;
                record.set_position(Some(position));
            }
        }
        document["generated"] = tables;
        Ok(document.to_string())
    }

    /// Render the user-owned tables canonically, for source fingerprinting.
    pub(crate) fn settings_fingerprint_source(&self) -> Result<String> {
        toml::to_string(&self.settings).context("failed to render the settings of bimetal.toml")
    }
}

/// Return the highest position of `table` and of the tables nested in it.
#[must_use]
fn last_position(table: &Table) -> isize {
    table
        .iter()
        .filter_map(|(_, item)| item.as_table())
        .map(last_position)
        .fold(table.position().unwrap_or_default(), isize::max)
}

/// Return the offset of the first `[generated]` table header in a
/// configuration's text.
#[must_use]
fn generated_offset(source: &str) -> Option<usize> {
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.starts_with("[generated") {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// Render the `[generated]` tables, with the comments that explain them to
/// someone reading `bimetal.toml`.
#[must_use]
fn generated_tables(generated: &GeneratedState) -> String {
    use std::fmt::Write as _;

    let mut tables = format!(
        r#"[generated]
# Fingerprint of the Rust/Cargo sources and the tables above.
source_fingerprint = {source_fingerprint}

# Fingerprint of the published files; lets build and check no-op when current.
output_fingerprint = {output_fingerprint}

# Files that the WebAssembly build, or contract discovery without one, reads,
# such as Rust sources and include_str! files, relative to the Cargo workspace.
# source_fingerprint covers them.
inputs = {inputs}
"#,
        source_fingerprint = literal::quoted(&generated.source_fingerprint),
        output_fingerprint =
            literal::quoted(generated.output_fingerprint.as_deref().unwrap_or_default()),
        inputs = string_array(&generated.inputs),
    );
    for (host, record) in generated.records() {
        let _ = write!(
            tables,
            r#"
[generated.{host}]
# The project that Bimetal generated the files in, relative to bimetal.toml.
project = {project}

# Files owned by Bimetal and safe to overwrite or remove, relative to the project.
files = {files}
"#,
            host = host.name(),
            project = literal::quoted(&record.project),
            files = string_array(&record.files),
        );
        let manifest = match host {
            Host::Python => "pyproject.toml must depend on them",
            Host::TypeScript => "package.json must list them in\n# peerDependencies",
        };
        let _ = write!(
            tables,
            "\n# Packages that the generated code imports; {manifest}.\ndependencies = {}\n",
            string_array(&record.dependencies)
        );
    }
    tables
}

/// Render a stable multiline TOML string array.
#[must_use]
fn string_array(values: &[String]) -> String {
    if values.is_empty() {
        return "[]".to_owned();
    }
    let mut source = String::from("[\n");
    for value in values {
        source.push_str("    ");
        source.push_str(&literal::quoted(value));
        source.push_str(",\n");
    }
    source.push(']');
    source
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffold;

    /// Write `source` as the `bimetal.toml` of a new directory.
    fn write_config(source: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CONFIG_FILE);
        fs::write(&path, source).unwrap();
        (directory, path)
    }

    /// A state with a record for Python and one for TypeScript.
    fn state() -> GeneratedState {
        GeneratedState {
            source_fingerprint: "abc".to_owned(),
            output_fingerprint: Some("def".to_owned()),
            inputs: vec!["src/data.txt".to_owned()],
            python: Some(HostRecord {
                project: "python".to_owned(),
                files: vec!["demo/api.py".to_owned()],
                dependencies: vec!["pydantic".to_owned()],
            }),
            typescript: Some(HostRecord {
                project: "../web".to_owned(),
                files: vec!["demo/api.js".to_owned()],
                dependencies: vec!["apache-arrow".to_owned()],
            }),
        }
    }

    /// Publish `generated` into the configuration at `path` twice, the
    /// first build adding the tables and the second replacing them, and
    /// return the text before `[generated]`.
    fn publish_twice(path: &Path, generated: &GeneratedState) -> String {
        let mut kept = String::new();
        for generated in [&GeneratedState::default(), generated] {
            let rendered = Config::read(path)
                .unwrap()
                .render_generated(generated)
                .unwrap();
            fs::write(path, &rendered).unwrap();
            let (before, _) = rendered
                .split_once("\n[generated]\n")
                .expect("rendered config has generated settings");
            kept = before.to_owned();
        }
        kept
    }

    #[test]
    fn generated_updates_preserve_the_user_tables_and_comments() {
        let application = scaffold::configuration("my-app", "my_app").replace(
            "# name = \"my-app\"",
            "# keep this comment\nname = \"demo\"",
        );
        let (_directory, path) = write_config(&application);

        assert_eq!(publish_twice(&path, &state()), application);

        let config = Config::read(&path).unwrap();
        assert_eq!(config.settings.application.name.as_deref(), Some("demo"));
        assert_eq!(config.generated, state());
        assert_eq!(config.generated.output_fingerprint.as_deref(), Some("def"));
    }

    #[test]
    fn generated_tables_follow_every_user_table() {
        let user = "[application]\nname = \"demo\"\n\n[python]\npackage = \"demo_py\"\n\n# The web package.\n[typescript]\nproject = \"../web\"\nbackend = \"node\"\n";
        let (_directory, path) = write_config(user);

        assert_eq!(publish_twice(&path, &state()), user);

        let config = Config::read(&path).unwrap();
        assert_eq!(config.generated, state());
        let settings = &config.settings;
        assert_eq!(
            settings.python.as_ref().unwrap().package.as_deref(),
            Some("demo_py")
        );
        let typescript = settings.typescript.as_ref().unwrap();
        assert_eq!(typescript.project.as_deref(), Some("../web"));
        assert_eq!(typescript.backend, Backend::Node);
    }

    #[test]
    fn a_user_table_after_generated_moves_above_it() {
        let (_directory, path) = write_config(
            "[application]\n\n[generated]\nsource_fingerprint = \"old\"\n\n[python]\nproject = \"python\"\n",
        );

        let rendered = Config::read(&path)
            .unwrap()
            .render_generated(&state())
            .unwrap();
        fs::write(&path, &rendered).unwrap();

        let python = rendered.find("[python]").unwrap();
        assert!(python < rendered.find("[generated]").unwrap(), "{rendered}");
        assert!(
            python < rendered.find("[generated.python]").unwrap(),
            "{rendered}"
        );
        let config = Config::read(&path).unwrap();
        assert_eq!(config.generated, state());
        assert_eq!(
            config.settings.python.unwrap().project.as_deref(),
            Some("python")
        );
        assert_eq!(generated_offset(&rendered), rendered.find("[generated]"));
    }

    #[test]
    fn a_conflict_in_generated_says_how_to_recover() {
        let (_directory, path) = write_config(
            "[python]\n\n[generated]\n<<<<<<< HEAD\nsource_fingerprint = \"a\"\n=======\nsource_fingerprint = \"b\"\n>>>>>>> other\n",
        );

        let error = format!("{:#}", Config::read(&path).unwrap_err());

        assert!(
            error.contains("keep either side of the conflict"),
            "{error}"
        );
        assert!(error.contains("TOML parse error"), "{error}");

        fs::write(&path, "[application\n[generated]\n").unwrap();
        let error = format!("{:#}", Config::read(&path).unwrap_err());
        assert!(!error.contains("[generated] tables"), "{error}");
    }

    #[test]
    fn a_directory_names_its_configuration() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(
            Config::find(Some(directory.path())).unwrap(),
            directory.path().join(CONFIG_FILE)
        );
        let file = directory.path().join("other.toml");
        assert_eq!(Config::find(Some(&file)).unwrap(), file);
    }

    #[test]
    fn first_generated_table_follows_the_last_comments() {
        let user = "[application]\nname = \"demo\"\n\n[python]\n# a closing comment\n";
        let (_directory, path) = write_config(user);
        let config = Config::read(&path).unwrap();

        let rendered = config.render_generated(&GeneratedState::default()).unwrap();

        assert!(rendered.starts_with(&format!("{user}\n[generated]\n")));
    }

    #[test]
    fn host_tables_turn_hosts_on() {
        let (_directory, path) = write_config("[application]\nname = \"demo\"\n");
        let error = format!("{:#}", Config::read(&path).unwrap_err());
        assert!(error.contains("turns on no host"), "{error}");

        fs::write(&path, "[typescript]\n").unwrap();
        let settings = Config::read(&path).unwrap().settings;
        assert_eq!(settings.python, None);
        assert_eq!(settings.typescript.unwrap().backend, Backend::Wasm);

        fs::write(&path, "[typescript]\nbackend = \"deno\"\n").unwrap();
        let error = format!("{:#}", Config::read(&path).unwrap_err());
        assert!(error.contains("unknown variant `deno`"), "{error}");

        fs::write(&path, "[python]\nheader = \"app.h\"\n").unwrap();
        let error = format!("{:#}", Config::read(&path).unwrap_err());
        assert!(error.contains("unknown field `header`"), "{error}");
    }

    #[test]
    fn namespaces_come_from_packages_unless_folders_are_chosen() {
        let (_directory, path) = write_config("[python]\n");
        let application = Config::read(&path).unwrap().settings.application;
        assert_eq!(application.namespaces, Namespaces::Packages);

        fs::write(&path, "[application]\nnamespaces = \"folders\"\n[python]\n").unwrap();
        let application = Config::read(&path).unwrap().settings.application;
        assert_eq!(application.namespaces, Namespaces::Folders);

        fs::write(&path, "[application]\nnamespaces = \"paths\"\n[python]\n").unwrap();
        let error = format!("{:#}", Config::read(&path).unwrap_err());
        assert!(
            error.contains("unknown variant `paths`, expected `packages` or `folders`"),
            "{error}"
        );
    }

    #[test]
    fn records_that_bimetal_did_not_write_say_how_to_recover() {
        for record in [
            "files = [\"app/api.js\"]\ndependencies = []\n",
            "project = \"typescript\"\nfiles = []\ndependencies = []\npeer_dependencies = []\n",
        ] {
            let (_directory, path) =
                write_config(&format!("[typescript]\n\n[generated.typescript]\n{record}"));

            let error = format!("{:#}", Config::read(&path).unwrap_err());

            assert!(
                error.contains(
                    "[generated] tables of bimetal.toml are not what this Bimetal writes"
                ),
                "{error}"
            );
        }

        let (_directory, path) = write_config("[application]\npython_package = \"app\"\n");
        let error = format!("{:#}", Config::read(&path).unwrap_err());
        assert!(error.contains("unknown field `python_package`"), "{error}");
        assert!(!error.contains("[generated] tables"), "{error}");
    }

    #[test]
    fn the_settings_fingerprint_covers_every_user_table() {
        let fingerprint = |source: &str| {
            let (_directory, path) = write_config(source);
            Config::read(&path)
                .unwrap()
                .settings_fingerprint_source()
                .unwrap()
        };
        let python = fingerprint("[python]\n");

        for other in [
            "[python]\npackage = \"other\"\n",
            "[python]\nproject = \"python\"\n",
            "[python]\n[typescript]\n",
            "[python]\n[typescript]\nbackend = \"node\"\n",
            "[application]\nname = \"other\"\n[python]\n",
            "[application]\nnamespaces = \"folders\"\n[python]\n",
        ] {
            assert_ne!(fingerprint(other), python, "{other}");
        }
        // Comments, formatting, and spelling out a default are not settings.
        assert_eq!(fingerprint("# Python\n[python]   \n"), python);
        assert_eq!(
            fingerprint("[application]\nnamespaces = \"packages\"\n[python]\n"),
            python
        );
    }
}
