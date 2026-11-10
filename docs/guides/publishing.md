# Publishing

The projects are ordinary Python and npm projects, so you publish them with the usual tools. `bimetal new` marks both private so that nothing leaves by accident.

## npm

Remove `"private": true` from `package.json`. With the Wasm backend the package contains the module, so one tarball serves every platform:

```bash
npm pack ./typescript
```

With the Node.js backend it contains the addon of the platform that ran `bimetal build-native` before `npm pack`, so publish one package per platform. Arrow contracts list `apache-arrow` as a peer dependency, so consumers install it beside the package.

## Python

Wheels contain the extension, so build one per platform:

```bash
bimetal build-native
```

```bash
uv build --wheel
```

Run those in the project on each platform. Any PEP 517 frontend works. `pdm_build.py`, which `bimetal new` writes, tags the wheel for CPython's stable ABI from 3.11 (`cp311-abi3`) and, on macOS, for the deployment target that the extension records, so one wheel per platform serves every supported Python. `MACOSX_DEPLOYMENT_TARGET` sets that target when the extension compiles. On Linux, run `auditwheel repair` before uploading to PyPI.

Building a wheel without the extension fails with a message that says to run `bimetal build-native`.

The project builds wheels only, because a source distribution carries no binary and can't build one. A plain `uv build` or `python -m build`, which starts with a source distribution, fails at once and says to run `uv build --wheel`.

The `Private :: Do Not Upload` classifier that `bimetal new` adds makes PyPI reject every upload. Remove it to publish.

## Your own files

The files that `bimetal new` writes meet the rules `bimetal build` checks. If you edit them, keep the rules true, or the build stops with an error that names the rule. [Commands](../reference/cli.md#what-a-build-checks) lists them.
