# Names

Python names stay in snake case. TypeScript functions, methods and parameters use lower camel case. Model fields follow Serde on the wire and in TypeScript.

## Parameters a host can't use

| Rust name | Python | TypeScript |
| --- | --- | --- |
| A reserved word of the host, such as `class` | `class_value` | `class_` |
| A name the generated wrapper uses itself, such as `bytes`, `str` or `int` | `bytes_`, `str_`, `int_` | Unchanged |

## Model fields in Python

A field whose name is a Python keyword or a member of Pydantic's `BaseModel`, such as `class`, `json` or `model_dump`, gains `_value` in Python (`json_value`) and keeps its wire name. So does a field whose name starts with an underscore, which Pydantic would take for a private attribute, without the underscores: `_id` becomes `id_value`.

A field's Python name can't be the wire name of another field of the model, such as two fields whose renames swap their names, because Pydantic would read the one for the other.

Exports and fields can have the names of other Python builtins, such as a function `list`, and an export can have the name of its own package. Exports can also have the names of JavaScript's globals, such as an error named `TypeError`.

## What Bimetal refuses

`bimetal build` reports all of these at once, and names the Rust items involved. It checks only the enabled hosts.

- An export whose name the generated package defines itself, such as `PanicException`, Pydantic's `Field`, `nativeReady` or `JsonValue`.
- A TypeScript export named `then`, which would make the module a thenable that `await import(...)` calls.
- A TypeScript export named `eval` or `arguments`, which a module can't bind.
- A parameter with the name of a model or error of its own module, which the Python wrapper needs.
- A package or folder name that can't be a Python module after `-` becomes `_`, such as `2024-feeds`.
- A Rust module or folder that becomes a Python module the generated package defines itself, such as one named `models` or `api`.
- Two namespaces that become one module of a host, such as the Rust module `xf_sources::venues` and the folder `sources/venues`.
- Names that collide after renaming.

## Packages

The application name uses lower-case letters, digits and single hyphens. The Python package can't be a Python keyword, the name of a standard-library module, or the name of a package the generated code imports. The npm package name is the `name` of `package.json`.

## Namespaces

A Rust module is a namespace, as [Functions](../guides/functions.md#namespaces) shows. A package linked through `additional_packages` comes first, without the `-`-separated prefix that all the packages share. With `namespaces = "folders"` in `[application]`, the package's folders below the deepest folder that holds every linked package, the application package's included, come first instead, one namespace per folder, and each folder above a package is a namespace of its own. [Workspaces](../guides/workspaces.md#linking-more-packages) shows both.

In Python, a `-` in a package's or a folder's name becomes `_`. With `namespaces = "folders"`, the application package's declarations are at the root. With the default, they are at the root only when the application package's name is the prefix that all the packages share, as `app` is beside `app-hardware`.
