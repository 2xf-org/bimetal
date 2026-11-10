"""Measure Bimetal on this machine and write the results as Markdown.

Usage: run.py [--steps <step,...>] [--examples <name,...>] [--output <file>]
              [--warm-runs <n>] [--noop-runs <n>] [--max-load <load>]

Steps, all by default:

- builds: the wall and CPU time of `bimetal build --force` for each example,
  cold (a new CARGO_TARGET_DIR) and warm (the same one again), then of a
  no-op `bimetal build` and of `bimetal check`. During each forced build, `ps`
  samples how many rustc and cargo processes run at once.
- sizes: the size of every native artifact and Wasm module that the examples
  install, and of the showcase's Node.js addon.
- calls: the time per call into the showcase from Python, and from TypeScript
  on the Wasm module and on the Node.js addon (calls.py and calls.mjs), at the
  median and the 90th percentile. Each host times the cases of CALL_CASES that
  it defines, and prints the time of each batch.
- memory: the peak resident size of one process per host and case, through
  `/usr/bin/time` (`-l` on macOS, `-v` on Linux), less that of the same
  process making no call.

The showcase on the Node.js backend is the copy that
apps/examples/showcase_node.py builds, as apps/examples/run uses it. Everything
else lives in target/bench.
The raw numbers go beside the report, in a file of the same name ending in
.json. CI does not run this.
"""

import argparse
import json
import math
import os
import platform
import re
import resource
import shutil
import statistics
import subprocess
import sys
import threading
import time
from collections.abc import Sequence
from datetime import datetime
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "apps" / "examples"))
import showcase_node

BENCH = Path(__file__).resolve().parent
WORK = ROOT / "target" / "bench"
BIMETAL = ROOT / "target" / "release" / "bimetal"
SHOWCASE = ROOT / "apps" / "examples" / "showcase"
NODE_COPY = showcase_node.COPY
STEPS = ("builds", "sizes", "calls", "memory")
ARTIFACTS = (".wasm", ".node", ".so", ".pyd")
MIB = 1 << 20
# The per-call cases, in the report's order: the calls that a sample times,
# and the samples.
CALL_CASES = {
    "ping": (1000, 200),
    "ping-raw": (1000, 200),
    "bytes-1mb": (1, 200),
    "models-in-10k": (1, 30),
    "models-in-10k-raw": (1, 30),
    "models-out-10k": (1, 30),
    "models-out-10k-raw": (1, 30),
    "arrow-1m": (1, 30),
    "stream-out-1m": (1, 30),
    "async": (100, 100),
    "stream-through-1m": (1, 30),
}
# The memory cases: the calls each makes, and the bytes its input holds. The
# 1M-row batch has a float64 and an int64 column.
MEMORY_CASES = {"bytes-1mb": (100, MIB), "bytes-100mb": (1, 100 * MIB), "arrow-1m": (1, 16_000_000)}
# The most payload copies a case may hold at once: the copies that each host's
# boundary makes into Rust plus those out of it, and for bytes the result that
# `reverse` allocates.
EXPECTED_COPIES = {
    "python": {"bytes": 1 + 1 + 1, "arrow": 0 + 0},
    "wasm": {"bytes": 1 + 1 + 1, "arrow": 2 + 2},
    "node": {"bytes": 1 + 1 + 1, "arrow": 2 + 2},
}


def run(command: Sequence[str | Path], cwd: Path = ROOT, env: dict[str, str] | None = None) -> str:
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


def load_average() -> float:
    return round(os.getloadavg()[0], 2)


def settle(limit: float, timeout: float = 600) -> None:
    """Wait up to `timeout` seconds for the 1-minute load average to fall below
    `limit`, which takes a minute or two after a build."""
    deadline = time.monotonic() + timeout
    if load_average() >= limit:
        print(f"waiting for the load average, {load_average()}, to fall below {limit}", flush=True)
    while load_average() >= limit and time.monotonic() < deadline:
        time.sleep(5)


def first_line(command: list[str]) -> str:
    return run(command).strip().splitlines()[0]


def context() -> dict[str, Any]:
    if sys.platform == "darwin":
        cpu = run(["sysctl", "-n", "machdep.cpu.brand_string"]).strip()
        memory = int(run(["sysctl", "-n", "hw.memsize"]))
        system = f"macOS {run(['sw_vers', '-productVersion']).strip()} ({run(['sw_vers', '-buildVersion']).strip()})"
    else:
        cpu = next(
            (line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")),
            platform.processor(),
        )
        memory = int(Path("/proc/meminfo").read_text().split()[1]) * 1024
        system = platform.platform()
    dirty = bool(run(["git", "status", "--porcelain"]).strip())
    return {
        "date": datetime.now().astimezone().isoformat(timespec="minutes"),
        "cpu": cpu,
        "cores": os.cpu_count(),
        "memory_gib": round(memory / (1 << 30)),
        "system": f"{system}, {platform.machine()}",
        "commit": run(["git", "rev-parse", "--short", "HEAD"]).strip() + (" plus uncommitted changes" if dirty else ""),
        "rustc": first_line(["rustc", "--version"]),
        "cargo": first_line(["cargo", "--version"]),
        "node": first_line(["node", "--version"]),
        "python": first_line([sys.executable, "--version"]),
        "uv": first_line(["uv", "--version"]),
        "cc": first_line(["cc", "--version"]),
    }


def config_of(example: str) -> Path:
    """The one bimetal.toml of an example, which may lie in one of its
    packages."""
    listed = run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", f"apps/examples/{example}"])
    configs = [line for line in listed.splitlines() if Path(line).name == "bimetal.toml"]
    if len(configs) != 1:
        sys.exit(f"apps/examples/{example} must hold exactly one bimetal.toml, not {configs}")
    return ROOT / configs[0]


def target_of(example: str) -> Path:
    """The target directory of the example's builds in every step."""
    return WORK / f"target-{example}"


class ProcessSampler(threading.Thread):
    """Count, every 0.1 s, the rustc and cargo processes whose command line
    names `marker`, such as a build's target directory."""

    def __init__(self, marker: str) -> None:
        super().__init__(daemon=True)
        self.marker = marker
        self.done = threading.Event()
        self.rustc: list[int] = []
        self.cargo: list[int] = []

    def run(self) -> None:
        while not self.done.is_set():
            listing = subprocess.run(["ps", "-axww", "-o", "args="], capture_output=True, text=True).stdout
            programs = [Path(line.split()[0]).name for line in listing.splitlines() if self.marker in line and line.split()]
            self.rustc.append(programs.count("rustc"))
            self.cargo.append(programs.count("cargo"))
            self.done.wait(0.1)

    def summary(self) -> dict[str, Any]:
        busy = [count for count in self.rustc if count > 0]
        return {
            "peak_rustc": max(self.rustc, default=0),
            "mean_rustc": round(statistics.mean(busy), 1) if busy else 0,
            "peak_cargo": max(self.cargo, default=0),
        }


def bimetal(arguments: list[str | Path], example: str, cwd: Path = ROOT) -> str:
    """Run `bimetal` on an example, which builds into the example's target
    directory."""
    return run([BIMETAL, *arguments], cwd=cwd, env={"CARGO_TARGET_DIR": str(target_of(example))})


def timed_bimetal(arguments: list[str], example: str, sample: bool) -> dict[str, Any]:
    """Run `bimetal <arguments>` on an example and return its wall time and the
    CPU time of its process tree, with process counts if `sample`."""
    sampler = ProcessSampler(str(target_of(example))) if sample else None
    load = load_average()
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.perf_counter()
    if sampler:
        sampler.start()
    bimetal([*arguments, "--locked", "--config", config_of(example)], example)
    wall = time.perf_counter() - start
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    result: dict[str, Any] = {
        "wall_s": round(wall, 3),
        "cpu_s": round(after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, 2),
        "load": load,
    }
    if sampler:
        sampler.done.set()
        sampler.join()
        result.update(sampler.summary())
    return result


def measure_builds(examples: list[str], warm_runs: int, noop_runs: int, max_load: float) -> dict[str, Any]:
    results: dict[str, Any] = {}
    for example in examples:
        shutil.rmtree(target_of(example), ignore_errors=True)
        settle(max_load)
        print(f"builds: {example}", flush=True)
        cold = timed_bimetal(["build", "--force"], example, sample=True)
        settle(max_load)
        warm = [timed_bimetal(["build", "--force"], example, sample=True) for _ in range(warm_runs)]
        noop = [timed_bimetal(["build"], example, sample=False) for _ in range(noop_runs)]
        check = [timed_bimetal(["check"], example, sample=False) for _ in range(noop_runs)]
        results[example] = {"cold": cold, "warm": warm, "noop": noop, "check": check}
    return results


def prepare_showcase() -> dict[str, list[str | Path]]:
    """Build the showcase for every host, and return the command that runs
    each host's benchmark."""
    print("calls: building the showcase", flush=True)
    bimetal(["build", "--locked", "--config", SHOWCASE / "bimetal.toml"], "showcase")
    run(["uv", "sync", "--locked", "--quiet", "--project", SHOWCASE / "python"])
    if not (SHOWCASE / "typescript" / "node_modules").is_dir():
        run(["npm", "ci", "--no-audit", "--no-fund"], cwd=SHOWCASE / "typescript")
    showcase_node.build()
    node = shutil.which("node") or "node"
    return {
        "python": [SHOWCASE / "python" / ".venv" / "bin" / "python", BENCH / "calls.py", SHOWCASE / "python"],
        "wasm": [node, BENCH / "calls.mjs", SHOWCASE / "typescript"],
        "node": [node, BENCH / "calls.mjs", NODE_COPY / "typescript"],
    }


def warmup(samples: int) -> int:
    """The samples that a case times first and drops, a tenth as many."""
    return samples // 10 + 1


def percentile(ordered: list[float], quantile: float) -> float:
    """The nearest-rank percentile of sorted samples."""
    return ordered[max(1, math.ceil(quantile * len(ordered))) - 1]


def measure_calls(hosts: dict[str, list[str | Path]], max_load: float) -> dict[str, Any]:
    """Time every case on each host, and reduce its batch times, without the
    warm-up, to the time per call at the median and the 90th percentile."""
    counts = [
        str(value) for name, (batch, samples) in CALL_CASES.items() for value in (name, batch, warmup(samples) + samples)
    ]
    results: dict[str, Any] = {}
    for host, command in hosts.items():
        settle(max_load)
        print(f"calls: {host}", flush=True)
        load = load_average()
        cases = []
        for line in run([*command, "time", *counts]).splitlines():
            if not line.startswith("{"):
                continue
            timed = json.loads(line)
            batch, samples = CALL_CASES[timed["case"]]
            ordered = sorted(timed["batch_ns"][warmup(samples):])
            cases.append(
                {
                    "case": timed["case"],
                    "batch": batch,
                    "samples": samples,
                    "median_ns": round(percentile(ordered, 0.5) / batch, 1),
                    "p90_ns": round(percentile(ordered, 0.9) / batch, 1),
                }
            )
        results[host] = {"load": load, "cases": cases}
    return results


def peak_rss(command: list[str | Path]) -> int:
    """The peak resident size of a command, in bytes."""
    flag, pattern, scale = ("-l", r"(\d+)\s+maximum resident set size", 1) if sys.platform == "darwin" else (
        "-v",
        r"Maximum resident set size \(kbytes\): (\d+)",
        1024,
    )
    result = subprocess.run(["/usr/bin/time", flag, *map(str, command)], capture_output=True, text=True, cwd=ROOT)
    found = re.search(pattern, result.stderr)
    if result.returncode != 0 or not found:
        sys.exit(f"{command} failed:\n{result.stderr}")
    return int(found[1]) * scale


def measure_memory(hosts: dict[str, list[str | Path]], max_load: float, repeats: int = 3) -> dict[str, Any]:
    results: dict[str, Any] = {}
    for host, command in hosts.items():
        settle(max_load)
        print(f"memory: {host}", flush=True)
        load = load_average()
        cases = {}
        for case, (calls, payload) in MEMORY_CASES.items():
            idle = statistics.median(peak_rss([*command, "memory", case, "0"]) for _ in range(repeats))
            busy = statistics.median(peak_rss([*command, "memory", case, str(calls)]) for _ in range(repeats))
            cases[case] = {"calls": calls, "payload": payload, "idle": idle, "busy": busy}
        results[host] = {"load": load, "cases": cases}
    return results


def measure_sizes(examples: list[str]) -> list[dict[str, Any]]:
    """Every artifact in the hosts' projects that `bimetal check --json`
    reports, and the showcase copy's Node.js addon."""
    found = []
    for example in examples:
        report = json.loads(bimetal(["check", "--json", "--config", config_of(example)], example))
        directories = [Path(host["directory"]) for host in report["hosts"]]
        if example == "showcase" and NODE_COPY.is_dir():
            directories.append(NODE_COPY / "typescript")
        for directory in directories:
            for path in sorted(directory.rglob("*")):
                parts = set(path.relative_to(directory).parts)
                if path.is_file() and path.suffix in ARTIFACTS and not parts & {"node_modules", ".venv", "tests"}:
                    label = "showcase (Node.js copy)" if NODE_COPY in path.parents else example
                    found.append({"example": label, "path": str(path.relative_to(directory)), "bytes": path.stat().st_size})
    return found


def duration(nanoseconds: float) -> str:
    if nanoseconds < 1e3:
        return f"{nanoseconds:.0f} ns"
    if nanoseconds < 1e6:
        return f"{nanoseconds / 1e3:.1f} µs"
    return f"{nanoseconds / 1e6:.2f} ms"


def render(results: dict[str, Any]) -> str:
    info = results["context"]
    lines = [
        "# Bimetal benchmarks",
        "",
        "## Machine",
        "",
        "| | |",
        "| --- | --- |",
        f"| Date | {info['date']} |",
        f"| CPU | {info['cpu']}, {info['cores']} cores, {info['memory_gib']} GiB |",
        f"| OS | {info['system']} |",
        f"| Commit | {info['commit']} |",
        f"| Toolchain | {info['rustc']}; {info['cargo']} |",
        f"| Hosts | Node.js {info['node']}; {info['python']}, uv {info['uv'].split()[1]}; {info['cc']} |",
        "",
        "## Method",
        "",
        "`mise run bench` (apps/benchmark/run.py) produced every number below. Before each example's builds",
        "and each host's measurements, it waits up to ten minutes for the 1-minute load average to fall below",
        f"{results['max_load']}; the load it started at sits beside the numbers. The per-call cases call the showcase",
        "example's exports. A sample times a batch of calls, after a warm-up of a tenth as many samples, and",
        "the table gives the time per call at the median and the 90th percentile (nearest rank) of the samples.",
        "Peak memory is the maximum resident set size that `/usr/bin/time` reports for one process, the median",
        "of three runs, less that of the same process making no call.",
        "",
    ]
    if "builds" in results:
        cores = info["cores"]
        lines += [
            "## Builds",
            "",
            f"`bimetal build --force --locked` with its own `CARGO_TARGET_DIR`: cold is the first build into an empty one,",
            "warm the next builds into it. CPU is the user and system time of the whole process tree, and",
            f"utilization divides it by wall time and {cores} cores. Rustc and cargo counts come from `ps` every 0.1 s.",
            "",
            "| Example | Build | Wall | CPU | Utilization | Peak rustc | Mean rustc | Peak cargo | Load |",
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- |",
        ]
        for example, runs in results["builds"].items():
            for label, entry in [("cold", runs["cold"]), *((f"warm {n + 1}", warm) for n, warm in enumerate(runs["warm"]))]:
                utilization = entry["cpu_s"] / (entry["wall_s"] * cores)
                lines.append(
                    f"| {example} | {label} | {entry['wall_s']:.1f} s | {entry['cpu_s']:.1f} s | {utilization:.0%} | "
                    f"{entry['peak_rustc']} | {entry['mean_rustc']} | {entry['peak_cargo']} | {entry['load']} |"
                )
        lines += [
            "",
            "| Example | No-op `bimetal build` (median, min to max) | `bimetal check` (median, min to max) | Runs | Load |",
            "| --- | --- | --- | --- | --- |",
        ]
        for example, runs in results["builds"].items():
            cells = []
            for key in ("noop", "check"):
                walls = [entry["wall_s"] * 1000 for entry in runs[key]]
                cells.append(f"{statistics.median(walls):.0f} ms ({min(walls):.0f} to {max(walls):.0f})")
            lines.append(f"| {example} | {cells[0]} | {cells[1]} | {len(runs['noop'])} | {runs['noop'][0]['load']} |")
        lines.append("")
    if "calls" in results:
        hosts = list(results["calls"])
        cases: dict[str, dict[str, Any]] = {}
        for host in hosts:
            for case in results["calls"][host]["cases"]:
                cases.setdefault(case["case"], {})[host] = case
        descriptions = {
            "ping": "`ping()`, a sync call with no arguments or result",
            "bytes-1mb": "`reverse(bytes)` of 1 MiB",
            "models-in-10k": "`count_tags(records)` of 10,000 `Record` models",
            "models-out-10k": "`Ledger.history()` returning 10,000 `Entry` models",
            "arrow-1m": "`columns.split(batch, 1000000)`: a 1,000,000-row batch in, a one-batch table out",
            "stream-out-1m": "`columns.batches(table)` read to its end: a table of 100 batches of 10,000 rows in, a stream of them out",
            "stream-through-1m": "`await columns.sum_column(columns.batches(table), \"price\")`: that stream back into Rust, one batch at a time",
            "async": "`await count_words(\"a b c\")`, one at a time",
        }
        header = " | ".join(f"{host} median / p90" for host in hosts)
        lines += [
            "## Per-call time",
            "",
            "Iterations are batch × samples per host. The load average before each host's run: "
            + ", ".join(f"{host} {results['calls'][host]['load']}" for host in hosts)
            + ".",
            "",
            f"| Case | Iterations | {header} |",
            "| --- | --- | " + " | ".join("---" for _ in hosts) + " |",
        ]
        for name, by_host in cases.items():
            sample = next(iter(by_host.values()))
            cells = [
                f"{duration(by_host[host]['median_ns'])} / {duration(by_host[host]['p90_ns'])}" if host in by_host else "n/a"
                for host in hosts
            ]
            base = name.removesuffix("-raw")
            description = descriptions.get(base, base) + (", native function only" if name != base else "")
            lines.append(f"| {description} | {sample['batch']} × {sample['samples']} | " + " | ".join(cells) + " |")
        lines.append("")
    if "memory" in results:
        tool = "`/usr/bin/time -l`" if sys.platform == "darwin" else "`/usr/bin/time -v`"
        lines += [
            "## Peak memory",
            "",
            f"Peak resident size from {tool}. Idle is the same process with its input set up and no call. Copies",
            "divides the difference by the payload: 1 MiB, 100 MiB, or the batch's 16,000,000 bytes. The expected",
            "maximum adds the copies that the host's boundary makes in and out to the result that `reverse`",
            "allocates in Rust; `split` returns slices of its input. Flagged rows exceed it by more than half a",
            "payload.",
            "",
            "| Host | Case | Calls | Idle | Peak | Difference | Copies | Expected at most | Flag | Load |",
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |",
        ]
        for host, entry in results["memory"].items():
            for case, data in entry["cases"].items():
                difference = data["busy"] - data["idle"]
                copies = difference / data["payload"]
                expected = EXPECTED_COPIES[host]["arrow" if case.startswith("arrow") else "bytes"]
                single_call = data["calls"] == 1
                flag = "extra copy?" if single_call and copies > expected + 0.5 else ""
                lines.append(
                    f"| {host} | {case} | {data['calls']} | {data['idle'] / MIB:.1f} MiB | {data['busy'] / MIB:.1f} MiB | "
                    f"{difference / MIB:.1f} MiB | {copies:.2f} | {expected if single_call else 'n/a'} | {flag} | {entry['load']} |"
                )
        lines += [
            "",
            "On Linux the same step runs `/usr/bin/time -v`, from GNU time (`apt install time`), and reads",
            "`Maximum resident set size (kbytes)`.",
            "",
        ]
    if "sizes" in results:
        lines += ["## Artifact sizes", "", "| Example | Artifact | Size |", "| --- | --- | --- |"]
        for artifact in results["sizes"]:
            lines.append(f"| {artifact['example']} | `{artifact['path']}` | {artifact['bytes'] / MIB:.2f} MiB ({artifact['bytes']:,} bytes) |")
        lines.append("")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--steps", default=",".join(STEPS))
    parser.add_argument("--examples", default="greeter,showcase,trading")
    parser.add_argument("--output", type=Path, default=WORK / "report.md")
    parser.add_argument("--warm-runs", type=int, default=2)
    parser.add_argument("--noop-runs", type=int, default=5)
    parser.add_argument("--max-load", type=float, default=2.0, help="the load average to wait for before each step")
    options = parser.parse_args()
    steps = options.steps.split(",")
    unknown = set(steps) - set(STEPS)
    if unknown:
        sys.exit(f"unknown steps: {', '.join(sorted(unknown))}")
    examples = options.examples.split(",")
    WORK.mkdir(parents=True, exist_ok=True)
    run(["cargo", "build", "--locked", "--release", "-p", "bimetal-cli"])
    results: dict[str, Any] = {"context": context(), "max_load": options.max_load}
    if "builds" in steps:
        results["builds"] = measure_builds(examples, options.warm_runs, options.noop_runs, options.max_load)
    if "calls" in steps or "memory" in steps:
        hosts = prepare_showcase()
        if "calls" in steps:
            results["calls"] = measure_calls(hosts, options.max_load)
        if "memory" in steps:
            results["memory"] = measure_memory(hosts, options.max_load)
    if "sizes" in steps:
        results["sizes"] = measure_sizes(examples)
    raw = options.output.with_suffix(".json")
    options.output.parent.mkdir(parents=True, exist_ok=True)
    raw.write_text(json.dumps(results, indent=2) + "\n")
    options.output.write_text(render(results) + "\n")
    print(f"wrote {options.output} and {raw}")


if __name__ == "__main__":
    main()
