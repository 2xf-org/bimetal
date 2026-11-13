"""The crossing lab: build the probe, time every crossing on each host, and
compare against a base version of Bimetal or against the gate's limits.

Usage: lab.py [--hosts python,wasm,node] [--base <git-ref>] [--rounds <n>]
              [--samples <n>] [--cases <case,...>] [--gate <file>]
              [--output <file>]

The probe (apps/benchmark/probe) has one export for each kind of value and
call that crosses a host boundary. The lab copies it into target/lab, builds
it there with this checkout's CLI and runtime, and runs lab_python.py on the
Python package and lab_node.mjs on the npm package, once on the Wasm module
and once on the Node.js addon.

With --base, it also extracts that git ref into target/lab/base, builds the
CLI there and the same probe with it, and runs the two builds in turns, round
after round, so that a change in the machine's load reaches both. Each round
is one process of each build, in an order that alternates. The report then
gives each case's time in both, and their ratio with its spread across rounds.

Each case's time is the median, over rounds, of the median nanoseconds per
call of the round's samples. A `-raw` case calls the native function alone,
so the report gives the wrapper's share as the difference. Every time is also
a multiple of `noop-raw` in the same build, which a change in load scales
alike. --gate checks the current build against a JSON file of limits by host
and case: a multiple of `noop-raw`, or for a key `<case>/<other>`, a multiple
of the other case, and exits with an error when one is exceeded.

The raw samples go to target/lab/results.json, and the report to standard
output and to --output if given.
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tarfile
import io
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
LAB = ROOT / "target" / "lab"
PROBE = Path("apps") / "benchmark" / "probe"
SHOWCASE = ROOT / "apps" / "examples" / "showcase"
HOSTS = ("python", "wasm", "node")
REFERENCE = "noop-raw"


def run(command: list[str | Path], cwd: Path = ROOT, env: dict[str, str] | None = None) -> str:
    """Run a command and return its stdout; on failure, show its output and
    exit."""
    result = subprocess.run(
        [str(part) for part in command],
        cwd=cwd,
        env={**os.environ, **(env or {})},
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        sys.exit(f"{' '.join(map(str, command))} failed:\n{result.stdout}\n{result.stderr}")
    return result.stdout


def extract(ref: str) -> Path:
    """The checkout of `ref` in target/lab/base, extracted again only when
    the ref names another commit, so its builds stay incremental."""
    commit = run(["git", "rev-parse", "--verify", f"{ref}^{{commit}}"]).strip()
    base = LAB / "base"
    marker = base / ".lab-commit"
    if marker.is_file() and marker.read_text() == commit:
        return base
    shutil.rmtree(base, ignore_errors=True)
    base.mkdir(parents=True)
    archive = subprocess.run(["git", "archive", "--format=tar", commit], cwd=ROOT, check=True, capture_output=True).stdout
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(base, filter="data")
    marker.write_text(commit)
    return base


def config(hosts: list[str]) -> str:
    """The probe's bimetal.toml for the hosts of one copy."""
    tables = []
    if "python" in hosts:
        tables.append("[python]\n")
    if "wasm" in hosts:
        tables.append("[typescript]\n")
    elif "node" in hosts:
        tables.append('[typescript]\nbackend = "node"\n')
    return "\n".join(tables)


def build(checkout: Path, hosts: list[str]) -> dict[str, Path]:
    """Build the CLI of `checkout` and the probe of `checkout` with it, and
    return each host's project. The addon needs a copy of its own, since one
    build has one TypeScript backend."""
    run(["cargo", "build", "--locked", "--release", "-p", "bimetal-cli"], cwd=checkout)
    copies = {"probe": [host for host in hosts if host in ("python", "wasm")], "probe-node": [host for host in hosts if host == "node"]}
    projects = {}
    for name, copy_hosts in copies.items():
        if not copy_hosts:
            continue
        # At the probe's depth in the checkout, so that its path dependency
        # on the runtime and the checkout's rust-toolchain.toml resolve.
        copy = checkout / "target" / "lab" / name
        source = checkout / PROBE
        for path in ("Cargo.toml", "Cargo.lock", "src", "python/pyproject.toml", "python/pdm_build.py", "typescript/package.json"):
            destination = copy / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            if (source / path).is_dir():
                shutil.copytree(source / path, destination, dirs_exist_ok=True)
            elif not destination.is_file() or destination.read_bytes() != (source / path).read_bytes():
                shutil.copy2(source / path, destination)
        settings = copy / "bimetal.toml"
        wanted = config(copy_hosts)
        # A new bimetal.toml drops the [generated] tables of the last build.
        if not settings.is_file() or not settings.read_text().startswith(wanted):
            settings.write_text(wanted)
        modules = copy / "typescript" / "node_modules"
        if not modules.exists():
            modules.symlink_to(SHOWCASE / "typescript" / "node_modules", target_is_directory=True)
        print(f"lab: building {name} for {', '.join(copy_hosts)} in {copy.relative_to(ROOT)}", flush=True)
        run(
            [checkout / "target" / "release" / "bimetal", "build", "--locked"],
            cwd=copy,
            env={"CARGO_TARGET_DIR": str(checkout / "target" / "lab" / f"{name}-build")},
        )
        for host in copy_hosts:
            projects[host] = copy / ("python" if host == "python" else "typescript")
    return projects


def prepare_hosts(hosts: list[str]) -> None:
    """Install the showcase's Python and npm projects, whose interpreter and
    node_modules the probe uses."""
    if "python" in hosts:
        run(["uv", "sync", "--locked", "--quiet", "--project", SHOWCASE / "python"])
    if any(host in hosts for host in ("wasm", "node")) and not (SHOWCASE / "typescript" / "node_modules").is_dir():
        run(["npm", "ci", "--no-audit", "--no-fund"], cwd=SHOWCASE / "typescript")


def driver(host: str, project: Path, samples: int, cases: list[str]) -> list[str | Path]:
    if host == "python":
        python = SHOWCASE / "python" / ".venv" / "bin" / "python"
        return [python, ROOT / "apps" / "benchmark" / "lab_python.py", project, 1, samples, *cases]
    node = shutil.which("node") or "node"
    return [node, ROOT / "apps" / "benchmark" / "lab_node.mjs", project, 1, samples, *cases]


def measure(builds: dict[str, dict[str, Path]], host: str, rounds: int, samples: int, cases: list[str]) -> dict[str, dict[str, list[float]]]:
    """Each build's per-round median of every case on `host`."""
    medians: dict[str, dict[str, list[float]]] = {build: {} for build in builds}
    order = list(builds)
    for index in range(rounds):
        print(f"lab: {host}, round {index + 1} of {rounds}", flush=True)
        for build in order if index % 2 == 0 else reversed(order):
            output = run(driver(host, builds[build][host], samples, cases))
            for line in output.splitlines():
                if line.startswith("{"):
                    timed = json.loads(line)
                    medians[build].setdefault(timed["case"], []).append(statistics.median(timed["ns"]))
    return medians


def duration(nanoseconds: float) -> str:
    if nanoseconds >= 1e6:
        return f"{nanoseconds / 1e6:.2f} ms"
    if nanoseconds >= 1e3:
        return f"{nanoseconds / 1e3:.2f} µs"
    return f"{nanoseconds:.0f} ns"


def summarize(per_round: dict[str, list[float]]) -> dict[str, dict[str, float]]:
    reference = per_round.get(REFERENCE)
    summary = {}
    for case, values in per_round.items():
        entry = {"median_ns": statistics.median(values)}
        if reference:
            entry["of_reference"] = statistics.median(value / ref for value, ref in zip(values, reference))
        summary[case] = entry
    return summary


def render(host: str, medians: dict[str, dict[str, list[float]]]) -> list[str]:
    current = summarize(medians["current"])
    lines = [f"### {host}", ""]
    if "base" in medians:
        base = summarize(medians["base"])
        lines += ["| Case | Base | Current | Current / base | Spread | |", "| --- | --- | --- | --- | --- | --- |"]
        for case, entry in current.items():
            if case not in base:
                lines.append(f"| `{case}` | | {duration(entry['median_ns'])} | | | |")
                continue
            ratios = sorted(now / then for now, then in zip(medians["current"][case], medians["base"][case]))
            ratio = statistics.median(ratios)
            low, high = ratios[len(ratios) // 4], ratios[(3 * len(ratios)) // 4]
            verdict = "faster" if high < 0.98 else "slower" if low > 1.02 else ""
            lines.append(
                f"| `{case}` | {duration(base[case]['median_ns'])} | {duration(entry['median_ns'])} | "
                f"{ratio:.2f} | {low:.2f} to {high:.2f} | {verdict} |"
            )
    else:
        lines += ["| Case | Time | Wrapper | × noop-raw |", "| --- | --- | --- | --- |"]
        for case, entry in current.items():
            raw = current.get(f"{case}-raw")
            wrapper = duration(entry["median_ns"] - raw["median_ns"]) if raw else ""
            multiple = f"{entry['of_reference']:.1f}" if "of_reference" in entry else ""
            lines.append(f"| `{case}` | {duration(entry['median_ns'])} | {wrapper} | {multiple} |")
    return [*lines, ""]


def gate(limits_file: Path, results: dict[str, Any]) -> list[str]:
    """The cases whose time exceeds its limit: a multiple of noop-raw, or of
    another case for a key `<case>/<other>`, the median of the rounds'
    ratios."""
    limits = json.loads(limits_file.read_text())
    failures = []
    for host, cases in limits.items():
        if host not in results:
            continue
        per_round = results[host]["current"]
        for key, limit in cases.items():
            case, _, other = key.partition("/")
            other = other or REFERENCE
            if case not in per_round or other not in per_round:
                failures.append(f"{host} {key}: not measured")
                continue
            found = statistics.median(value / base for value, base in zip(per_round[case], per_round[other]))
            if found > limit:
                failures.append(f"{host} {case}: {found:.2f} × {other}, above the limit of {limit}")
    return failures


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--hosts", default=",".join(HOSTS))
    parser.add_argument("--base", help="a git ref to compare against")
    parser.add_argument("--rounds", type=int, default=6)
    parser.add_argument("--samples", type=int, default=15)
    parser.add_argument("--cases", default="", help="the cases to time; all by default")
    parser.add_argument("--gate", type=Path, help="a JSON file of limits, as multiples of noop-raw, by host and case")
    parser.add_argument("--output", type=Path)
    options = parser.parse_args()
    hosts = options.hosts.split(",")
    unknown = set(hosts) - set(HOSTS)
    if unknown:
        sys.exit(f"unknown hosts: {', '.join(sorted(unknown))}")
    cases = [case for case in options.cases.split(",") if case]
    if options.gate and cases:
        for limits in json.loads(options.gate.read_text()).values():
            for key in limits:
                case, _, other = key.partition("/")
                cases += [name for name in (case, other) if name and name not in cases]
    if cases and REFERENCE not in cases:
        cases.append(REFERENCE)
    prepare_hosts(hosts)
    builds = {"current": build(ROOT, hosts)}
    if options.base:
        builds["base"] = build(extract(options.base), hosts)
    results: dict[str, Any] = {}
    report = ["# Crossing lab", ""]
    for host in hosts:
        medians = measure(builds, host, options.rounds, options.samples, cases)
        results[host] = medians
        report += render(host, medians)
    LAB.mkdir(parents=True, exist_ok=True)
    (LAB / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    text = "\n".join(report)
    print(text)
    if options.output:
        options.output.write_text(text + "\n")
    if options.gate:
        failures = gate(options.gate, results)
        if failures:
            sys.exit("the lab's gate failed:\n" + "\n".join(failures))
        print("the lab's gate passed")


if __name__ == "__main__":
    main()
