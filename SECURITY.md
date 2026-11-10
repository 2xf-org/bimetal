# Security

## Reporting a vulnerability

Report a vulnerability in private, through [the form on GitHub](https://github.com/2xf-org/bimetal/security/advisories/new). Please don't open a public issue for it. You should get an answer within a week.

A report is most useful with the Bimetal version, the host (Python, the Wasm module or the Node.js addon), and a small Rust API that shows the problem.

## What counts

Bimetal generates the code between a host and your Rust, so these are in scope:

- memory unsafety that safe Rust reaches through the runtime or the generated code;
- a value that crosses a boundary without the checks the contract promises;
- a build or a generated package that runs or includes something the application did not ask for.

A bug in your own exported Rust is not in scope, and neither is a host program that passes hostile input to an API that accepts it.

## Supported versions

Fixes go into the newest release.
