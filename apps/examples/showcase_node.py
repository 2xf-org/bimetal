"""Build a copy of the showcase whose npm package runs on the Node.js
backend, with this checkout's release CLI.

Usage: showcase_node.py

The copy goes in target/examples/showcase-node, at the showcase's depth in
the checkout, so its path dependencies and rust-toolchain.toml resolve, and
builds into target/examples/showcase-node-build, which later runs reuse. The
copy leaves out the Python project, and its bimetal.toml that project's table
and record; it sets `backend = "node"` in [typescript], and its npm project
links the showcase's node_modules. apps/examples/run runs the showcase's
TypeScript tests in it, and apps/benchmark/run.py times its calls.
"""

import os
import shutil
import subprocess
from collections.abc import Sequence
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SHOWCASE = ROOT / "apps" / "examples" / "showcase"
COPY = ROOT / "target" / "examples" / "showcase-node"


def copy_files(pathspecs: Sequence[str], destination: Path, base: Path) -> None:
    """Replace `destination` with the files under `pathspecs` that git tracks
    or would track, at their paths relative to `base`."""
    shutil.rmtree(destination, ignore_errors=True)
    listed = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", *pathspecs],
        check=True,
        stdout=subprocess.PIPE,
        cwd=ROOT,
        text=True,
    ).stdout
    for name in filter(None, listed.split("\0")):
        source = ROOT / name
        if source.is_file():
            copy = destination / source.relative_to(base)
            copy.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, copy)


def drop_hosts(config: Path, hosts: Sequence[str], backend: str) -> None:
    """Remove the tables of `hosts`, and their `[generated]` records, from a
    copied bimetal.toml, and set the TypeScript backend to `backend`."""
    lines = []
    skip = False
    for line in config.read_text(encoding="utf-8").splitlines():
        if line.startswith("["):
            skip = line.strip("[]").removeprefix("generated.") in hosts
        if not skip:
            lines.append(line)
        if line == "[typescript]":
            lines.append(f'backend = "{backend}"')
    config.write_text("\n".join(lines) + "\n", encoding="utf-8")


def build() -> Path:
    """Build the copy, and return its npm project."""
    copy_files(
        [
            "apps/examples/showcase",
            ":(exclude)apps/examples/showcase/python",
        ],
        COPY,
        SHOWCASE,
    )
    drop_hosts(COPY / "bimetal.toml", ("python",), backend="node")
    project = COPY / "typescript"
    (project / "node_modules").symlink_to(SHOWCASE / "typescript" / "node_modules", target_is_directory=True)
    subprocess.run(
        [ROOT / "target" / "release" / "bimetal", "build", "--locked"],
        check=True,
        cwd=COPY,
        env={**os.environ, "CARGO_TARGET_DIR": str(ROOT / "target" / "examples" / "showcase-node-build")},
    )
    return project


if __name__ == "__main__":
    build()
