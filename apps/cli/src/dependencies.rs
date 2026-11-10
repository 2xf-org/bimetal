//! The host packages that generated code imports, which the user's
//! manifests must declare and `bimetal.toml` records.

use crate::config::Host;
use crate::contract::Contract;

/// A host package that generated code imports, with the requirement that a
/// new project declares for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostDependency {
    pub(crate) name: &'static str,
    pub(crate) requirement: &'static str,
}

pub(crate) const PYDANTIC: HostDependency = HostDependency {
    name: "pydantic",
    // 2.7 added the context that a dump passes to its serializers, which
    // tells a JSON value that it crosses into Rust.
    requirement: "pydantic>=2.7,<3",
};
pub(crate) const NUMPY: HostDependency = HostDependency {
    name: "numpy",
    requirement: "numpy>=2,<3",
};
/// pyarrow 14 reads and writes the Arrow PyCapsule interface, but pyarrow 16
/// is the first that allows NumPy 2, which buffers need.
pub(crate) const PYARROW: HostDependency = HostDependency {
    name: "pyarrow",
    requirement: "pyarrow>=16",
};
pub(crate) const APACHE_ARROW: HostDependency = HostDependency {
    name: "apache-arrow",
    requirement: "^21.2.0",
};

/// The packages that each host's generated code imports, which the user's
/// manifests must declare.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostDependencies {
    /// The dependencies of the Python package.
    pub(crate) python: Vec<HostDependency>,
    /// The peer dependencies of the npm package.
    pub(crate) typescript: Vec<HostDependency>,
}

impl HostDependencies {
    /// Return the packages that the generated packages of `contract` import:
    /// Pydantic always, NumPy for buffers, and an Arrow library for Arrow
    /// values.
    #[must_use]
    pub(crate) fn new(contract: &Contract<'_>) -> Self {
        let arrow = contract.uses_arrow();
        Self {
            python: [PYDANTIC]
                .into_iter()
                .chain(contract.uses_buffer().then_some(NUMPY))
                .chain(arrow.then_some(PYARROW))
                .collect(),
            typescript: arrow.then_some(APACHE_ARROW).into_iter().collect(),
        }
    }

    /// Return the dependencies with the names that `bimetal.toml` records, or
    /// `None` if one is not a package that generated code imports.
    #[must_use]
    pub(crate) fn named(python: &[String], typescript: &[String]) -> Option<Self> {
        let known = |names: &[String]| -> Option<Vec<HostDependency>> {
            names
                .iter()
                .map(|name| {
                    [PYDANTIC, NUMPY, PYARROW, APACHE_ARROW]
                        .into_iter()
                        .find(|dependency| dependency.name == name)
                })
                .collect()
        };
        Some(Self {
            python: known(python)?,
            typescript: known(typescript)?,
        })
    }

    /// Return the dependencies of `host`'s package.
    #[must_use]
    pub(crate) fn of(&self, host: Host) -> &[HostDependency] {
        match host {
            Host::Python => &self.python,
            Host::TypeScript => &self.typescript,
        }
    }

    /// Return the names of dependencies, as `bimetal.toml` records them.
    #[must_use]
    pub(crate) fn names(dependencies: &[HostDependency]) -> Vec<String> {
        dependencies
            .iter()
            .map(|dependency| dependency.name.to_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_dependency_names_round_trip() {
        let dependencies = HostDependencies {
            python: vec![PYDANTIC, NUMPY, PYARROW],
            typescript: vec![APACHE_ARROW],
        };
        let python = HostDependencies::names(&dependencies.python);
        let typescript = HostDependencies::names(&dependencies.typescript);

        assert_eq!(python, ["pydantic", "numpy", "pyarrow"]);
        assert_eq!(
            HostDependencies::named(&python, &typescript),
            Some(dependencies)
        );
        assert_eq!(HostDependencies::named(&["leftpad".to_owned()], &[]), None);
    }
}
