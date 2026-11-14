# Releases

The four crates release together at one version, which every crate takes from `workspace.package.version`. The command refuses a runtime of another version, so each release publishes all four, even when only one changed.

## Versions

A version is the date of its release, as `YYYYMM.DD.N`: the year and month, the day, and the count of that day's releases, from 0. The first release on 10 November 2026 is `202611.10.0`, and a second that day is `202611.10.1`. Cargo allows no leading zero, so 5 January 2027 is `202701.5.0`.

Cargo reads the first number as a major version, so it takes the releases of one month for compatible and those of two months for not. Neither is a promise. An application names one exact version, and changes it when it changes the command.

## Before the tag

1. Change `version` in the root `Cargo.toml` to the date of the release, and the three `version = "=YYYYMM.DD.N"` requirements under `[workspace.dependencies]` in the same file.
2. Change the version in the `bimetal = { version = "=YYYYMM.DD.N" }` sample in [Arrow](../guides/arrow.md).
3. Run [the checks](testing.md#the-commands), which regenerate the examples for the new version, and commit.
4. Wait for CI to pass on that commit on `main`.

## The tag

Tag the commit `vYYYYMM.DD.N`, such as `v202611.10.0`, and push the tag. `.github/workflows/release.yml` then runs three jobs in order:

| Job | What it does |
| --- | --- |
| Check the version | Fails unless the tag, the workspace version and the three requirements agree |
| One per platform | Builds the command for Linux x86_64 and arm64, macOS arm64 and x86_64, and Windows x86_64, and packs each with the licence |
| Publish | Publishes `bimetal-ir`, `bimetal-macros`, `bimetal` and `bimetal-cli` to crates.io, then creates the GitHub release, named after the tag, with the archives, their `SHA256SUMS`, how to install the version, and GitHub's list of what changed since the release before |

crates.io keeps a version for ever, so publishing comes after every build. The crates.io token is the `CARGO_REGISTRY_TOKEN` secret of the `release` environment, which only the publish job names, so no other job or workflow can read it. If it fails after the crates are published, create the GitHub release by hand from the run's archives, because a second run can't publish the same version.

## What users install

`cargo install --locked bimetal-cli` builds the command from crates.io, and each release has it built. Both hash the same generator sources, as [The crates](crates.md) says, so they agree on every application's fingerprint. An application names the runtime as `bimetal = "=YYYYMM.DD.N"`, which `bimetal new` writes.
