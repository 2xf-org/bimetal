//! Python package emission.
//!
//! Generated modules are rendered into a staging tree. The user-owned
//! `pyproject.toml` is never rewritten; `project::validation` checks it.
//!
//! - `package` writes the package's files, among them `bimetal.py`, the
//!   runtime, from `runtime.py`;
//! - `models` and `api` render each namespace's `models.py` and `api.py`;
//! - `render` holds the spelling decisions that they share, and `context`
//!   the state they render with, which records what each module imports.

mod api;
mod context;
mod models;
mod package;
mod render;

pub(crate) use api::RESOURCE_MEMBERS;
pub(crate) use models::python_field_name;
pub(crate) use package::{API_MODULE, MODELS_MODULE, emit};
pub(crate) use render::{
    namespace_buffers, python_buffer_alias, python_model_names, python_namespace_module,
    python_package_directory, python_package_name, python_param_names,
};
