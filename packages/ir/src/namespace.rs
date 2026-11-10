//! Generated namespaces: where each declaration appears in the host packages.

use crate::CargoPackageId;

/// Derives generated namespaces for the declarations of one manifest.
///
/// A namespace keeps the declaring package name without the `-`-separated
/// prefix shared by every package, so `app-hardware` becomes `hardware` beside
/// `app`, followed by the Rust module path below the crate root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamespaceRule {
    shared_prefix: usize,
}

impl NamespaceRule {
    /// Derive the rule from every declaring package of one manifest.
    pub(crate) fn from_packages<'name>(packages: impl IntoIterator<Item = &'name str>) -> Self {
        let parts: Vec<Vec<&str>> = packages
            .into_iter()
            .map(|package| package.split('-').collect())
            .collect();
        let shortest = parts.iter().map(Vec::len).min().unwrap_or(0);
        let shared_prefix = (0..shortest)
            .take_while(|index| {
                parts
                    .iter()
                    .all(|package| package[*index] == parts[0][*index])
            })
            .count();
        Self { shared_prefix }
    }

    /// Return the Rust-shaped public namespace for one exported declaration.
    #[must_use]
    pub fn namespace(self, owner: &CargoPackageId, rust_module: &str) -> Namespace {
        let segments: Vec<&str> = owner.0.split('-').collect();
        let package = segments
            .get(self.shared_prefix..)
            .filter(|segments| !segments.is_empty())
            .map(|segments| segments.join("-"));
        Namespace::new(package.into_iter().collect(), rust_module)
    }
}

/// A generated namespace derived from one Cargo package and Rust module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Namespace {
    /// Segments that name the declaring package, none for a package whose
    /// declarations take the root. [`NamespaceRule`] gives at most one, the
    /// package name without the shared prefix, and none for the package
    /// whose name is that prefix, such as `app` beside `app-hardware`.
    pub package: Vec<String>,
    /// Rust module segments below the declaring crate root.
    pub modules: Vec<String>,
}

impl Namespace {
    /// Return the namespace of the Rust module `rust_module`, such as
    /// `app_hardware::annotation`, in a package that `package` names.
    #[must_use]
    pub fn new(package: Vec<String>, rust_module: &str) -> Self {
        let modules = rust_module.split("::").skip(1).map(str::to_owned).collect();
        Self { package, modules }
    }

    /// Return the aggregate package root.
    #[must_use]
    pub const fn root() -> Self {
        Self {
            package: Vec::new(),
            modules: Vec::new(),
        }
    }

    /// Return the namespace segments used in a Python package.
    #[must_use]
    pub fn python_segments(&self) -> Vec<String> {
        self.package
            .iter()
            .map(|segment| segment.replace('-', "_"))
            .chain(self.modules.iter().cloned())
            .collect()
    }

    /// Return the namespace segments used in a TypeScript package subpath.
    #[must_use]
    pub fn typescript_segments(&self) -> Vec<String> {
        self.package.iter().chain(&self.modules).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_lose_their_shared_prefix() {
        let rule = NamespaceRule::from_packages(["app", "app-hardware", "app-hardware-sensors"]);
        let namespace = |owner: &str, rust_module: &str| {
            rule.namespace(&CargoPackageId::new(owner), rust_module)
        };

        assert_eq!(
            namespace("app", "app::stats"),
            Namespace::new(Vec::new(), "app::stats")
        );
        let sensors = namespace("app-hardware-sensors", "app_hardware_sensors::probe");
        assert_eq!(sensors.package, ["hardware-sensors"]);
        assert_eq!(sensors.python_segments(), ["hardware_sensors", "probe"]);
        assert_eq!(sensors.typescript_segments(), ["hardware-sensors", "probe"]);
        assert_eq!(
            namespace("app-hardware", "app_hardware"),
            Namespace {
                package: vec!["hardware".to_owned()],
                modules: Vec::new(),
            }
        );
    }

    #[test]
    fn package_segments_keep_their_order_and_their_own_separator() {
        let package = ["sources", "venues", "poly-market"]
            .map(str::to_owned)
            .to_vec();
        let namespace = Namespace::new(package, "xf_polymarket::markets::fees");

        assert_eq!(
            namespace.python_segments(),
            ["sources", "venues", "poly_market", "markets", "fees"]
        );
        assert_eq!(
            namespace.typescript_segments(),
            ["sources", "venues", "poly-market", "markets", "fees"]
        );
        assert!(Namespace::root() < namespace);
    }
}
