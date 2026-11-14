"""
Measure Bimetal on this machine and write the results as Markdown.

Usage: run.py [--steps <step,...>] [--examples <name,...>] [--output <file>]
              [--warm-runs <n>] [--noop-runs <n>] [--max-load <load>]

Steps, all by default:

- builds: the wall and CPU time of `bimetal build --force` for each example,
  cold (a new CARGO_TARGET_DIR) and warm (the same one again), then of a
  no-op `bimetal build` and of `bimetal check`. During each forced build, `ps`
  samples how many rustc and cargo processes run at once.
- sizes: the size of every native artifact and Wasm module that the examples
  install, and of the showcase's Node.js addon.
- memory: the peak resident size of one process per host and case, through
  `/usr/bin/time` (`-l` on macOS, `-v` on Linux), less that of the same
  process making no call, which memory.py and memory.mjs make on the
  showcase.

The time per call is the crossing lab's: apps/benchmark/lab.py.

The showcase on the Node.js backend is the copy that
apps/examples/showcase_node.py builds, as apps/examples/run uses it. Everything
else lives in target/bench.
The raw numbers go beside the report, in a file of the same name ending in
.json. CI does not run this.
"""

import argparse
import collections.abc
import datetime
import json
import os
import pathlib
import platform
import re
import resource
import shutil
import statistics
import subprocess
import sys
import threading
import time
import typing

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "examples"))
import showcase_node

ROOT = pathlib.Path(__file__).resolve().parents[2]
BENCH = pathlib.Path(__file__).resolve().parent
WORK = ROOT / "target" / "bench"
BIMETAL = ROOT / "target" / "release" / "bimetal"
SHOWCASE = ROOT / "apps" / "examples" / "showcase"
NODE_COPY = showcase_node.COPY
STEPS = ("builds", "sizes", "memory")
ARTIFACTS = (".wasm", ".node", ".so", ".pyd")
MIB = 1 << 20
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


# ---------------------------------------------------------------------------
# Commands and the machine
# ---------------------------------------------------------------------------


def run(
    command: collections.abc.Sequence[str | pathlib.Path],
    cwd: pathlib.Path = ROOT,
    env: dict[str, str] | None = None,
) -> str:
    """
    Run a command, and exit with its output if it fails.

    Args:
        command: The program and its arguments.
        cwd: The directory to run it in.
        env: Variables to set on top of this process's environment.

    Returns:
        What the command printed to stdout.
    """
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
    """
    Read the 1-minute load average.

    Returns:
        The load average, to two decimals.
    """
    return round(os.getloadavg()[0], 2)


def settle(limit: float, timeout: float = 600) -> None:
    """
    Wait for the 1-minute load average to fall below a limit.

    Notes:
        That takes a minute or two after a build.

    Args:
        limit: The load average to wait for.
        timeout: The most seconds to wait.
    """
    deadline = time.monotonic() + timeout
    if load_average() >= limit:
        print(f"waiting for the load average, {load_average()}, to fall below {limit}", flush=True)
    while load_average() >= limit and time.monotonic() < deadline:
        time.sleep(5)


def first_line(command: list[str]) -> str:
    """
    Run a command and keep the first line it prints, such as a version.

    Args:
        command: The program and its arguments.

    Returns:
        The first line of its stdout.
    """
    return run(command).strip().splitlines()[0]


def context() -> dict[str, typing.Any]:
    """
    Describe the machine, the commit, and the tools that the numbers come from.

    Returns:
        The report's machine table, by row.
    """
    if sys.platform == "darwin":
        cpu = run(["sysctl", "-n", "machdep.cpu.brand_string"]).strip()
        memory = int(run(["sysctl", "-n", "hw.memsize"]))
        system = f"macOS {run(['sw_vers', '-productVersion']).strip()} ({run(['sw_vers', '-buildVersion']).strip()})"
    else:
        lines = pathlib.Path("/proc/cpuinfo").read_text().splitlines()
        cpu = next(
            (line.split(":", 1)[1].strip() for line in lines if line.startswith("model name")),
            platform.processor(),
        )
        memory = int(pathlib.Path("/proc/meminfo").read_text().split()[1]) * 1024
        system = platform.platform()
    dirty = bool(run(["git", "status", "--porcelain"]).strip())
    return {
        "date": datetime.datetime.now().astimezone().isoformat(timespec="minutes"),
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


# ---------------------------------------------------------------------------
# Builds
# ---------------------------------------------------------------------------


def config_of(example: str) -> pathlib.Path:
    """
    Find the one bimetal.toml of an example, which may lie in one of its packages.

    Args:
        example: The example's directory name.

    Returns:
        The file's path.
    """
    listed = run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", f"apps/examples/{example}"])
    configs = [line for line in listed.splitlines() if pathlib.Path(line).name == "bimetal.toml"]
    if len(configs) != 1:
        sys.exit(f"apps/examples/{example} must hold exactly one bimetal.toml, not {configs}")
    return ROOT / configs[0]


def target_of(example: str) -> pathlib.Path:
    """
    Name the target directory of the example's builds in every step.

    Args:
        example: The example's directory name.

    Returns:
        The directory.
    """
    return WORK / f"target-{example}"


class ProcessSampler(threading.Thread):
    """
    Count, every 0.1 s, the rustc and cargo processes whose command line names a marker.
    """

    def __init__(self, marker: str) -> None:
        """
        Set up the sampler.

        Args:
            marker: Text that the command lines hold, such as a build's target directory.
        """
        super().__init__(daemon=True)
        self.marker = marker
        self.done = threading.Event()
        self.rustc: list[int] = []
        self.cargo: list[int] = []

    def run(self) -> None:
        while not self.done.is_set():
            listing = subprocess.run(["ps", "-axww", "-o", "args="], capture_output=True, text=True).stdout
            programs = [
                pathlib.Path(line.split()[0]).name
                for line in listing.splitlines()
                if self.marker in line and line.split()
            ]
            self.rustc.append(programs.count("rustc"))
            self.cargo.append(programs.count("cargo"))
            self.done.wait(0.1)

    def summary(self) -> dict[str, typing.Any]:
        """
        Reduce the samples to the peak and mean counts.

        Returns:
            The peak rustc count, the mean while any rustc runs, and the peak cargo count.
        """
        busy = [count for count in self.rustc if count > 0]
        return {
            "peak_rustc": max(self.rustc, default=0),
            "mean_rustc": round(statistics.mean(busy), 1) if busy else 0,
            "peak_cargo": max(self.cargo, default=0),
        }


def bimetal(arguments: list[str | pathlib.Path], example: str, cwd: pathlib.Path = ROOT) -> str:
    """
    Run `bimetal` on an example, which builds into the example's target directory.

    Args:
        arguments: The arguments to `bimetal`.
        example: The example's directory name.
        cwd: The directory to run it in.

    Returns:
        What `bimetal` printed to stdout.
    """
    return run([BIMETAL, *arguments], cwd=cwd, env={"CARGO_TARGET_DIR": str(target_of(example))})


def timed_bimetal(arguments: list[str], example: str, sample: bool) -> dict[str, typing.Any]:
    """
    Time `bimetal <arguments>` on an example.

    Args:
        arguments: The arguments to `bimetal`, before the lock and config flags.
        example: The example's directory name.
        sample: Whether to count rustc and cargo processes during the run.

    Returns:
        The wall time, the CPU time of the process tree, the load at the start, and the process counts if sampled.
    """
    sampler = ProcessSampler(str(target_of(example))) if sample else None
    load = load_average()
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.perf_counter()
    if sampler:
        sampler.start()
    bimetal([*arguments, "--locked", "--config", config_of(example)], example)
    wall = time.perf_counter() - start
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    result: dict[str, typing.Any] = {
        "wall_s": round(wall, 3),
        "cpu_s": round(after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, 2),
        "load": load,
    }
    if sampler:
        sampler.done.set()
        sampler.join()
        result.update(sampler.summary())
    return result


def measure_builds(examples: list[str], warm_runs: int, noop_runs: int, max_load: float) -> dict[str, typing.Any]:
    """
    Time each example's cold, warm, and no-op builds, and its checks.

    Args:
        examples: The examples' directory names.
        warm_runs: The forced builds after the cold one.
        noop_runs: The no-op builds, and the checks.
        max_load: The load average to wait for before each example's builds.

    Returns:
        Each example's timed runs, by kind.
    """
    results: dict[str, typing.Any] = {}
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


# ---------------------------------------------------------------------------
# Memory
# ---------------------------------------------------------------------------


def prepare_showcase() -> dict[str, list[str | pathlib.Path]]:
    """
    Build the showcase for every host.

    Returns:
        The command that runs each host's memory cases.
    """
    print("memory: building the showcase", flush=True)
    bimetal(["build", "--locked", "--config", SHOWCASE / "bimetal.toml"], "showcase")
    run(["uv", "sync", "--locked", "--quiet", "--project", SHOWCASE / "python"])
    if not (SHOWCASE / "typescript" / "node_modules").is_dir():
        run(["npm", "ci", "--no-audit", "--no-fund"], cwd=SHOWCASE / "typescript")
    showcase_node.build()
    node = shutil.which("node") or "node"
    return {
        "python": [SHOWCASE / "python" / ".venv" / "bin" / "python", BENCH / "memory.py", SHOWCASE / "python"],
        "wasm": [node, BENCH / "memory.mjs", SHOWCASE / "typescript"],
        "node": [node, BENCH / "memory.mjs", NODE_COPY / "typescript"],
    }


def peak_rss(command: list[str | pathlib.Path]) -> int:
    """
    Measure the peak resident size of a command.

    Args:
        command: The program and its arguments.

    Returns:
        The peak resident size, in bytes.
    """
    if sys.platform == "darwin":
        flag, pattern, scale = "-l", r"(\d+)\s+maximum resident set size", 1
    else:
        flag, pattern, scale = "-v", r"Maximum resident set size \(kbytes\): (\d+)", 1024
    result = subprocess.run(["/usr/bin/time", flag, *map(str, command)], capture_output=True, text=True, cwd=ROOT)
    found = re.search(pattern, result.stderr)
    if result.returncode != 0 or not found:
        sys.exit(f"{command} failed:\n{result.stderr}")
    return int(found[1]) * scale


def measure_memory(
    hosts: dict[str, list[str | pathlib.Path]],
    max_load: float,
    repeats: int = 3,
) -> dict[str, typing.Any]:
    """
    Measure each memory case's peak resident size on each host, idle and busy.

    Args:
        hosts: The command that runs each host's memory cases.
        max_load: The load average to wait for before each host.
        repeats: The runs of each measurement, whose median counts.

    Returns:
        Each host's load at the start and its cases.
    """
    results: dict[str, typing.Any] = {}
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


# ---------------------------------------------------------------------------
# Sizes
# ---------------------------------------------------------------------------


def measure_sizes(examples: list[str]) -> list[dict[str, typing.Any]]:
    """
    Measure every artifact in the hosts' projects, and the showcase copy's Node.js addon.

    Args:
        examples: The examples' directory names.

    Returns:
        Each artifact's example, path in its project, and size in bytes.
    """
    found = []
    for example in examples:
        report = json.loads(bimetal(["check", "--json", "--config", config_of(example)], example))
        directories = [pathlib.Path(host["directory"]) for host in report["hosts"]]
        if example == "showcase" and NODE_COPY.is_dir():
            directories.append(NODE_COPY / "typescript")
        for directory in directories:
            for path in sorted(directory.rglob("*")):
                parts = set(path.relative_to(directory).parts)
                if path.is_file() and path.suffix in ARTIFACTS and not parts & {"node_modules", ".venv", "tests"}:
                    label = "showcase (Node.js copy)" if NODE_COPY in path.parents else example
                    found.append(
                        {
                            "example": label,
                            "path": str(path.relative_to(directory)),
                            "bytes": path.stat().st_size,
                        },
                    )
    return found


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------


def render(results: dict[str, typing.Any]) -> str:
    """
    Render the report.

    Args:
        results: The machine, the load limit, and each step's results.

    Returns:
        The report's Markdown.
    """
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
        f"{results['max_load']}; the load it started at sits beside the numbers. The memory cases call the showcase",
        "example's exports. `mise run lab` (apps/benchmark/lab.py) measures the time per call.",
        "Peak memory is the maximum resident set size that `/usr/bin/time` reports for one process, the median",
        "of three runs, less that of the same process making no call.",
        "",
    ]
    if "builds" in results:
        cores = info["cores"]
        lines += [
            "## Builds",
            "",
            "`bimetal build --force --locked` with its own `CARGO_TARGET_DIR`: cold is the first build into an empty",
            "one, warm the next builds into it. CPU is the user and system time of the whole process tree, and",
            f"utilization divides it by wall time and {cores} cores. Rustc and cargo counts come from `ps` every",
            "0.1 s.",
            "",
            "| Example | Build | Wall | CPU | Utilization | Peak rustc | Mean rustc | Peak cargo | Load |",
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- |",
        ]
        for example, runs in results["builds"].items():
            labelled = [("cold", runs["cold"]), *((f"warm {n + 1}", warm) for n, warm in enumerate(runs["warm"]))]
            for label, entry in labelled:
                utilization = entry["cpu_s"] / (entry["wall_s"] * cores)
                lines.append(
                    f"| {example} | {label} | {entry['wall_s']:.1f} s | {entry['cpu_s']:.1f} s | {utilization:.0%} | "
                    f"{entry['peak_rustc']} | {entry['mean_rustc']} | {entry['peak_cargo']} | {entry['load']} |"
                )
        lines += [
            "",
            (
                "| Example | No-op `bimetal build` (median, min to max) | `bimetal check` (median, min to max) "
                "| Runs | Load |"
            ),
            "| --- | --- | --- | --- | --- |",
        ]
        for example, runs in results["builds"].items():
            cells = []
            for key in ("noop", "check"):
                walls = [entry["wall_s"] * 1000 for entry in runs[key]]
                cells.append(f"{statistics.median(walls):.0f} ms ({min(walls):.0f} to {max(walls):.0f})")
            lines.append(f"| {example} | {cells[0]} | {cells[1]} | {len(runs['noop'])} | {runs['noop'][0]['load']} |")
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
                limit = expected if single_call else "n/a"
                lines.append(
                    f"| {host} | {case} | {data['calls']} | {data['idle'] / MIB:.1f} MiB | "
                    f"{data['busy'] / MIB:.1f} MiB | {difference / MIB:.1f} MiB | {copies:.2f} | {limit} | {flag} | "
                    f"{entry['load']} |"
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
            size = artifact["bytes"]
            lines.append(f"| {artifact['example']} | `{artifact['path']}` | {size / MIB:.2f} MiB ({size:,} bytes) |")
        lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Command line
# ---------------------------------------------------------------------------


def main() -> None:
    """
    Run the chosen steps, and write the report and its raw numbers.
    """
    parser = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    parser.add_argument("--steps", default=",".join(STEPS))
    parser.add_argument("--examples", default="bank,showcase,streams")
    parser.add_argument("--output", type=pathlib.Path, default=WORK / "report.md")
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
    results: dict[str, typing.Any] = {"context": context(), "max_load": options.max_load}
    if "builds" in steps:
        results["builds"] = measure_builds(examples, options.warm_runs, options.noop_runs, options.max_load)
    if "memory" in steps:
        results["memory"] = measure_memory(prepare_showcase(), options.max_load)
    if "sizes" in steps:
        results["sizes"] = measure_sizes(examples)
    raw = options.output.with_suffix(".json")
    options.output.parent.mkdir(parents=True, exist_ok=True)
    raw.write_text(json.dumps(results, indent=2) + "\n")
    options.output.write_text(render(results) + "\n")
    print(f"wrote {options.output} and {raw}")


if __name__ == "__main__":
    main()
