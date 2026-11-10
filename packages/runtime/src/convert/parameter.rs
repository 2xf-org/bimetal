//! The parameter that every host's argument errors name, and how each host
//! names it.

#[cfg(any(
    target_arch = "wasm32",
    all(feature = "node", not(target_arch = "wasm32"))
))]
use bimetal_ir::typescript_names;

/// One parameter of a generated wrapper, which the errors about its argument
/// name.
#[derive(Debug, Clone, Copy)]
pub struct Parameter {
    /// The callable as the host's callers write it: `greet` or `Counter.add`,
    /// and for a constructor `Counter` in Python and `new Counter` in
    /// JavaScript.
    pub callable: &'static str,
    /// The parameter's name in the generated wrapper, which `bimetal_ir`
    /// derives for each host: `python_names::parameter_names` or
    /// `typescript_names::parameter_name`.
    pub name: &'static str,
}

impl Parameter {
    /// Name this parameter before `cause`, an error about its argument, as
    /// Python's own errors do: `greet() argument 'name' must be str, not
    /// int`.
    #[cfg(all(feature = "python", not(target_arch = "wasm32")))]
    #[cold]
    #[must_use]
    pub(crate) fn python_message(&self, cause: &str) -> String {
        format!(
            "{}() argument '{}'{}{cause}",
            self.callable,
            self.name,
            separator(cause)
        )
    }

    /// Name this parameter before `cause`, an error about its argument, as
    /// the generated `checkArgument` does: ``greet(): argument `name` must
    /// be a string``.
    #[cfg(any(
        target_arch = "wasm32",
        all(feature = "node", not(target_arch = "wasm32"))
    ))]
    #[cold]
    #[must_use]
    pub(crate) fn javascript_message(&self, cause: &str) -> String {
        format!(
            "{}{}{cause}",
            typescript_names::argument_label(self.callable, self.name),
            separator(cause)
        )
    }
}

/// Take a byte argument of exactly `N` bytes as a Rust array.
///
/// # Errors
///
/// Returns what the argument breaks, which the host names the parameter
/// before, when it has another length.
#[cfg(any(
    target_arch = "wasm32",
    all(any(feature = "python", feature = "node"), not(target_arch = "wasm32"))
))]
pub(crate) fn fixed_bytes<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N], String> {
    <[u8; N]>::try_from(bytes)
        .map_err(|bytes| format!("must contain exactly {N} bytes, got {}", bytes.len()))
}

/// Return what separates a parameter's name from `cause`: a space before a
/// requirement such as `must be a string`, which the name begins, and
/// otherwise a colon.
fn separator(cause: &str) -> &'static str {
    if cause.starts_with("must ") {
        " "
    } else {
        ": "
    }
}

/// Return the name of the resource type `T`, for errors about arguments of
/// another type.
pub(super) fn resource_name<T>() -> &'static str {
    let path = std::any::type_name::<T>();
    path.rsplit("::").next().unwrap_or(path)
}
