//! Cargo project resolution, link crate compilation, and package generation.
//!
//! A [`Project`] is an immutable snapshot of Cargo metadata and application
//! settings for one command invocation. Builds use a synthetic `cdylib` link
//! crate outside the source tree to link every selected package. The link
//! crate is compiled concurrently for native contract discovery, the Wasm
//! module, and the native artifacts of the enabled hosts, so each artifact
//! uses the correct platform link contract.
//!
//! - `snapshot` resolves the [`Project`], its hosts, and the native artifacts
//!   that their builds install;
//! - `packages` selects the Cargo packages and the `bimetal` source, and the
//!   folders that name the packages when `namespaces` asks for them;
//! - `checkout` computes the source fingerprint, and splits what the builds
//!   read into the checkout's files and this machine's;
//! - `link` writes the link crate, and `compile` compiles its targets;
//! - `discovery` reads the contract from the discovery build;
//! - `exports` checks that the Wasm module exports what the contract describes;
//! - `validation` checks names for every enabled host, and the hosts'
//!   manifests;
//! - `pipeline` runs the build and `check` steps, and `reports` holds what
//!   they return.

mod checkout;
mod compile;
mod discovery;
mod exports;
mod link;
mod packages;
mod pipeline;
mod reports;
mod snapshot;
mod validation;

pub(crate) use packages::{BimetalDependency, GitReference, SourceRoot};
pub(crate) use pipeline::{build, check};
pub(crate) use reports::{BuildReport, BuildStatus, CheckReport, HostReport};
pub(crate) use snapshot::Project;
pub(crate) use validation::{
    validate_application_name, validate_python_package, validate_typescript_package,
};
