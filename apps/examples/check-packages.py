"""Check the packages that one example would publish, and install each one
into a new project, where it must load; docs/internals/testing.md#the-examples says what
each host's check covers.

Usage: check-packages.py <report> <directory>

<report> holds what `bimetal check --json` printed for the example: its
hosts, with their packages, projects, and files. <directory> is an empty
directory outside the repository, so that no setting of the repository
applies to the new projects.
"""

import json
import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path


def fail(message: str) -> None:
    sys.exit(f"package check failed: {message}")


def run(command: list[str], cwd: Path) -> str:
    """Run a command, which on Windows may be a `.cmd` file such as npm's,
    and return what it prints to stdout. Its stderr shows as it runs."""
    executable = shutil.which(command[0]) or command[0]
    return subprocess.run([executable, *command[1:]], check=True, stdout=subprocess.PIPE, cwd=cwd, text=True).stdout


def check_python(project: Path, package: str, files: list[str], directory: Path) -> str:
    """Build and check the wheel, and import it from a new environment."""
    dist = directory / "dist"
    run(["uv", "build", "--quiet", "--wheel", "--out-dir", str(dist)], project)
    wheels = list(dist.glob("*.whl"))
    if len(wheels) != 1:
        fail(f"expected one wheel, found {wheels}")
    wheel = wheels[0]
    # The extension uses CPython's stable ABI from 3.11, so one wheel per
    # platform must serve every later Python.
    if "-cp311-abi3-" not in wheel.name:
        fail(f"{wheel.name} is not tagged for the stable ABI")
    contents = set(zipfile.ZipFile(wheel).namelist())
    for required in files:
        if required not in contents:
            fail(f"{wheel.name} lacks {required}")

    environment = directory / "environment"
    run(["uv", "venv", "--quiet", str(environment)], directory)
    python = environment / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    run(["uv", "pip", "install", "--quiet", "--python", str(python), str(wheel)], directory)
    # The environment's directory holds no source tree, so the import finds the
    # installed package.
    imported = run([str(python), "-c", f"import {package}; print({package}.__file__)"], environment).strip()
    if not Path(imported).resolve().is_relative_to(environment.resolve()):
        fail(f"{package} was imported from {imported}, not from the installed wheel")
    return wheel.name


def check_typescript(project: Path, package: str, files: list[str], directory: Path) -> str:
    """Pack and check the tarball, and import it from a new npm project."""
    [pack] = json.loads(run(["npm", "pack", "--json", "--ignore-scripts", "--pack-destination", str(directory)], project))
    contents = {entry["path"] for entry in pack["files"]}
    for required in ["package.json", *files]:
        if required not in contents:
            fail(f"the npm tarball lacks {required}")
    stray = sorted(path for path in contents if path.startswith(("tests/", "node_modules/")))
    if stray:
        fail(f"the npm tarball ships {stray}")

    consumer = directory / "project"
    consumer.mkdir()
    (consumer / "package.json").write_text('{"private": true, "type": "module"}\n')
    # npm also installs the package's peer dependencies, such as apache-arrow.
    run(["npm", "install", "--no-audit", "--no-fund", str(directory / pack["filename"])], consumer)
    script = (
        "const [, name] = process.argv;"
        "const { fileURLToPath } = await import('node:url');"
        "const loaded = await import(name);"
        "await loaded.nativeReady;"
        "console.log(fileURLToPath(import.meta.resolve(name)));"
    )
    imported = run(["node", "--input-type=module", "-e", script, package], consumer).strip()
    if not Path(imported).resolve().is_relative_to(consumer.resolve()):
        fail(f"{package} was imported from {imported}, not from the installed tarball")
    return pack["filename"]


def main() -> None:
    report = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    directory = Path(sys.argv[2])
    checked = []
    for host in report["hosts"]:
        project = Path(host["directory"])
        destination = directory / host["host"]
        destination.mkdir(parents=True)
        check = {"python": check_python, "typescript": check_typescript}[host["host"]]
        checked.append(check(project, host["package"], host["files"], destination))
    print(f"checked and installed {', '.join(checked)}")


if __name__ == "__main__":
    main()
