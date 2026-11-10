//! The grammar of the names that `bimetal.toml` and Cargo give: the
//! application, the Python package, and the npm package.

use anyhow::{Result, bail};
use bimetal_ir::python_names;

use crate::dependencies::APACHE_ARROW;
use crate::literal;

/// The top-level modules of Python's standard library in every supported
/// version, 3.11 to 3.14: the union of each version's
/// `sys.stdlib_module_names`, and `test`, the regression tests, which those
/// leave out. A package of the same name would hide one.
const PYTHON_STANDARD_LIBRARY: &str = "
test
__future__ _abc _aix_support _android_support _apple_support _ast _ast_unparse _asyncio _bisect
_blake2 _bootsubprocess _bz2 _codecs _codecs_cn _codecs_hk _codecs_iso2022 _codecs_jp _codecs_kr
_codecs_tw _collections _collections_abc _colorize _compat_pickle _compression _contextvars _crypt
_csv _ctypes _curses _curses_panel _datetime _dbm _decimal _elementtree _frozen_importlib
_frozen_importlib_external _functools _gdbm _hashlib _heapq _hmac _imp _interpchannels
_interpqueues _interpreters _io _ios_support _json _locale _lsprof _lzma _markupbase _md5 _msi
_multibytecodec _multiprocessing _opcode _opcode_metadata _operator _osx_support _overlapped
_pickle _posixshmem _posixsubprocess _py_abc _py_warnings _pydatetime _pydecimal _pyio _pylong
_pyrepl _queue _random _remote_debugging _scproxy _sha1 _sha2 _sha256 _sha3 _sha512 _signal
_sitebuiltins _socket _sqlite3 _sre _ssl _stat _statistics _string _strptime _struct _suggestions
_symtable _sysconfig _thread _threading_local _tkinter _tokenize _tracemalloc _types _typing _uuid
_warnings _weakref _weakrefset _winapi _wmi _zoneinfo _zstd abc aifc annotationlib antigravity
argparse array ast asynchat asyncio asyncore atexit audioop base64 bdb binascii bisect builtins bz2
cProfile calendar cgi cgitb chunk cmath cmd code codecs codeop collections colorsys compileall
compression concurrent configparser contextlib contextvars copy copyreg crypt csv ctypes curses
dataclasses datetime dbm decimal difflib dis distutils doctest email encodings ensurepip enum errno
faulthandler fcntl filecmp fileinput fnmatch fractions ftplib functools gc genericpath getopt
getpass gettext glob graphlib grp gzip hashlib heapq hmac html http idlelib imaplib imghdr imp
importlib inspect io ipaddress itertools json keyword lib2to3 linecache locale logging lzma mailbox
mailcap marshal math mimetypes mmap modulefinder msilib msvcrt multiprocessing netrc nis nntplib nt
ntpath nturl2path numbers opcode operator optparse os ossaudiodev pathlib pdb pickle pickletools
pipes pkgutil platform plistlib poplib posix posixpath pprint profile pstats pty pwd py_compile
pyclbr pydoc pydoc_data pyexpat queue quopri random re readline reprlib resource rlcompleter runpy
sched secrets select selectors shelve shlex shutil signal site smtpd smtplib sndhdr socket
socketserver spwd sqlite3 sre_compile sre_constants sre_parse ssl stat statistics string stringprep
struct subprocess sunau symtable sys sysconfig syslog tabnanny tarfile telnetlib tempfile termios
textwrap this threading time timeit tkinter token tokenize tomllib trace traceback tracemalloc tty
turtle turtledemo types typing unicodedata unittest urllib uu uuid venv warnings wave weakref
webbrowser winreg winsound wsgiref xdrlib xml xmlrpc zipapp zipfile zipimport zlib zoneinfo
";

/// The top-level modules that generated Python packages import, directly or
/// through Pydantic. A package of the same name would hide one.
const PYTHON_DEPENDENCIES: [&str; 7] = [
    "annotated_types",
    "numpy",
    "pyarrow",
    "pydantic",
    "pydantic_core",
    "typing_extensions",
    "typing_inspection",
];

/// Validate a dot-separated Python import package.
pub(crate) fn validate_python_package(value: &str) -> Result<()> {
    if value.split('.').any(str::is_empty) {
        bail!("Python package `{value}` must be dot-separated Python names, such as `my_app`");
    }
    for segment in value.split('.') {
        if let Some(reason) = python_name_problem(segment) {
            bail!(
                "Python package `{value}` cannot be imported, because `{segment}` is {reason}; choose another name"
            );
        }
    }
    let top = value.split('.').next().unwrap_or(value);
    if PYTHON_STANDARD_LIBRARY
        .split_whitespace()
        .any(|module| module == top)
    {
        bail!(
            "Python package `{value}` would hide the module `{top}` of Python's standard library, or be hidden by it; choose another name"
        );
    }
    if PYTHON_DEPENDENCIES.contains(&top) {
        bail!(
            "Python package `{value}` would hide the package `{top}`, which the generated package imports; choose another name"
        );
    }
    Ok(())
}

/// Validate an unscoped or npm-scoped TypeScript package name.
pub(crate) fn validate_typescript_package(value: &str) -> Result<()> {
    let name = value.strip_prefix('@').unwrap_or(value);
    let parts: Vec<&str> = name.split('/').collect();
    let expected = if value.starts_with('@') { 2 } else { 1 };
    if value.is_empty()
        || parts.len() != expected
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.chars().all(|character| {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || matches!(character, '-' | '_' | '.')
                })
        })
    {
        bail!("invalid TypeScript package `{value}`");
    }
    // A package that imports its own name imports itself.
    if value == APACHE_ARROW.name {
        bail!(
            "npm package `{value}` would hide the package `{value}`, which the generated package imports for Arrow values; choose another name"
        );
    }
    Ok(())
}

/// Validate the portable application-name grammar used by generated symbols.
pub(crate) fn validate_application_name(value: &str) -> Result<()> {
    if is_application_name(value) {
        return Ok(());
    }
    let rule = "must use lower-case letters, numbers, and single hyphens, and start with a letter";
    let suggestion = value.to_ascii_lowercase().replace('_', "-");
    if is_application_name(&suggestion) {
        bail!("application name `{value}` {rule}, such as `{suggestion}`");
    }
    bail!("application name `{value}` {rule}");
}

/// Return whether `value` follows the application-name grammar.
#[must_use]
fn is_application_name(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && !value.ends_with('-')
        && !value.contains("--")
}

/// Explain why `name` cannot be a Python name, if it cannot.
#[must_use]
pub(super) fn python_name_problem(name: &str) -> Option<&'static str> {
    if python_names::is_keyword(name) {
        Some("a Python keyword")
    } else if !literal::is_identifier(name) {
        Some("not a valid Python name")
    } else if is_python_package_attribute(name) {
        Some("a name that Python reserves for its own module attributes")
    } else {
        None
    }
}

/// Return whether Python reserves `value` as a package protocol attribute.
#[must_use]
fn is_python_package_attribute(value: &str) -> bool {
    value != "__version__" && value.starts_with("__") && value.ends_with("__")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_names_reject_keywords() {
        assert_eq!(python_name_problem("class"), Some("a Python keyword"));
        assert_eq!(python_name_problem("Model"), None);
    }

    #[test]
    fn python_packages_must_be_importable_and_leave_the_standard_library_visible() {
        assert!(validate_python_package("my_app").is_ok());
        assert!(validate_python_package("company.my_app").is_ok());
        for (package, message) in [
            (
                "import",
                "Python package `import` cannot be imported, because `import` is a Python keyword; choose another name",
            ),
            (
                "company.class",
                "Python package `company.class` cannot be imported, because `class` is a Python keyword; choose another name",
            ),
            (
                "my-app",
                "Python package `my-app` cannot be imported, because `my-app` is not a valid Python name; choose another name",
            ),
            (
                "my_app.",
                "Python package `my_app.` must be dot-separated Python names, such as `my_app`",
            ),
            (
                "email",
                "Python package `email` would hide the module `email` of Python's standard library, or be hidden by it; choose another name",
            ),
            (
                "test.tools",
                "Python package `test.tools` would hide the module `test` of Python's standard library, or be hidden by it; choose another name",
            ),
            (
                "typing_extensions",
                "Python package `typing_extensions` would hide the package `typing_extensions`, which the generated package imports; choose another name",
            ),
        ] {
            assert_eq!(
                validate_python_package(package)
                    .expect_err("the package is invalid")
                    .to_string(),
                message
            );
        }
        assert_eq!(
            validate_typescript_package("apache-arrow")
                .expect_err("the package would import itself")
                .to_string(),
            "npm package `apache-arrow` would hide the package `apache-arrow`, which the generated package imports for Arrow values; choose another name"
        );
    }

    #[test]
    fn application_names_suggest_a_valid_spelling() {
        assert!(validate_application_name("my-app").is_ok());
        assert_eq!(
            validate_application_name("my_app")
                .expect_err("underscores are invalid")
                .to_string(),
            "application name `my_app` must use lower-case letters, numbers, and single hyphens, and start with a letter, such as `my-app`"
        );
        assert_eq!(
            validate_application_name("2app")
                .expect_err("a digit cannot start the name")
                .to_string(),
            "application name `2app` must use lower-case letters, numbers, and single hyphens, and start with a letter"
        );
    }
}
