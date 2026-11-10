//! Reading the contract from the discovery build of the link crate.

use std::ffi::{CStr, c_char};
use std::path::Path;

use anyhow::{Context, Result, bail};
use bimetal_ir::Manifest;
use bimetal_ir::discovery::{self, DiscoveryResult};
use libloading::{Library, Symbol};

/// A contract that discovery returned.
pub(super) struct DiscoveredContract {
    pub(super) manifest: Manifest,
    /// The hash of the contract JSON, which the Python extension computes
    /// too; see [`discovery::contract_hash`].
    pub(super) hash: String,
}

/// Load the discovery build's library and deserialize the contract of the
/// application `package_name`.
pub(super) fn read_contract(library_path: &Path, package_name: &str) -> Result<DiscoveredContract> {
    type ContractFn = unsafe extern "C" fn() -> DiscoveryResult;
    type FreeFn = unsafe extern "C" fn(*mut c_char);

    // SAFETY: the library remains live while both symbols and the returned payload are used.
    let library = unsafe { Library::new(library_path) }
        .with_context(|| format!("failed to load {}", library_path.display()))?;
    let contract_name = format!("{}{package_name}\0", discovery::CONTRACT_SYMBOL_PREFIX);
    let free_name = format!("{}{package_name}\0", discovery::FREE_SYMBOL_PREFIX);
    // SAFETY: the application macro emits these exact ABI signatures.
    let contract: Symbol<'_, ContractFn> = unsafe { library.get(contract_name.as_bytes()) }
        .with_context(|| {
            format!(
                "generated Bimetal link crate is missing `{}`",
                contract_name.trim_end_matches('\0')
            )
        })?;
    // SAFETY: the application macro emits these exact ABI signatures.
    let free: Symbol<'_, FreeFn> = unsafe { library.get(free_name.as_bytes()) }
        .with_context(|| format!("missing `{}`", free_name.trim_end_matches('\0')))?;
    // SAFETY: the loaded function follows the discovery ABI.
    let result = unsafe { contract() };
    if result.payload.is_null() {
        bail!("the generated Bimetal link crate panicked during contract discovery");
    }
    // SAFETY: the discovery ABI returns a live NUL-terminated string.
    let payload = unsafe { CStr::from_ptr(result.payload) }
        .to_bytes()
        .to_vec();
    // SAFETY: the payload came from this library and is freed once.
    unsafe { free(result.payload) };
    match result.status {
        discovery::SUCCESS => Ok(DiscoveredContract {
            manifest: serde_json::from_slice(&payload)?,
            hash: discovery::contract_hash(&payload),
        }),
        // The payload lists every problem in the user's words.
        discovery::ERROR => bail!("{}", String::from_utf8_lossy(&payload)),
        discovery::PANIC => bail!(
            "the generated Bimetal link crate panicked during contract discovery: {}",
            String::from_utf8_lossy(&payload)
        ),
        status => bail!("bimetal discovery returned unknown status {status}"),
    }
}
