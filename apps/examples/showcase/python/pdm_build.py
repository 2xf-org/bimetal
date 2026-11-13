"""Build only wheels, tagged for the extension's stable ABI and platform.

The native extension uses CPython's stable ABI from 3.11, so one wheel per
platform serves every later Python. Without this hook, pdm-backend would tag
the wheel for the interpreter and the OS release that happen to build it, and
build a source distribution that cannot build the extension.
"""

import struct
import sys

# The oldest Python whose stable ABI the extension targets (pyo3's abi3-py311).
STABLE_ABI_PYTHON = "cp311"
EXTENSION_SUFFIXES = (".abi3.so", ".pyd")
# Mach-O constants from <mach-o/loader.h> and <mach/machine.h>.
MACH_O_64_MAGIC = 0xFEEDFACF
LC_BUILD_VERSION = 0x32
MACHINES = {0x01000007: "x86_64", 0x0100000C: "arm64"}


def pdm_build_initialize(context):
    if context.target == "sdist":
        raise RuntimeError(
            "this package ships a native extension, which a source distribution "
            "cannot build: build a wheel on each platform with `uv build --wheel`"
        )
    if context.target == "wheel":
        settings = context.config_settings
        settings.setdefault("--python-tag", STABLE_ABI_PYTHON)
        settings.setdefault("--py-limited-api", "abi3")


def pdm_build_update_files(context, files):
    if context.target != "wheel":
        return
    extensions = [path for name, path in files.items() if name.endswith(EXTENSION_SUFFIXES)]
    if not extensions:
        raise RuntimeError(
            "the native extension is missing: run `bimetal build` before building a wheel"
        )
    if sys.platform == "darwin":
        # pdm-backend reads the tag only after this hook.
        context.config_settings.setdefault("--plat-name", macos_platform(extensions[0]))


def macos_platform(extension):
    """Return the tag of the macOS release and the machine that `extension`
    targets, which its `LC_BUILD_VERSION` load command records."""
    with open(extension, "rb") as binary:
        header = binary.read(32).ljust(32, b"\0")
        magic, machine, _, _, count, size, _, _ = struct.unpack("<8I", header)
        commands = binary.read(size)
    if magic != MACH_O_64_MAGIC or machine not in MACHINES:
        raise RuntimeError(f"{extension} is not a 64-bit Mach-O binary for x86_64 or arm64")
    offset = 0
    for _ in range(count):
        command, command_size = struct.unpack_from("<2I", commands, offset)
        if command == LC_BUILD_VERSION:
            minimum = struct.unpack_from("<I", commands, offset + 12)[0]
            major, minor = minimum >> 16, (minimum >> 8) & 0xFF
            # From macOS 11, installers match only the major release.
            if major >= 11:
                minor = 0
            return f"macosx_{major}_{minor}_{MACHINES[machine]}"
        offset += command_size
    raise RuntimeError(f"{extension} records no macOS deployment target")
