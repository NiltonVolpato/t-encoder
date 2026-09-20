#!/usr/bin/env python3
# Copyright © 2026 Nilton Volpato
# SPDX-License-Identifier: MIT

"""Symbolizes statistical profiler PC addresses from LilyGO T-Encoder Pro firmware logs.

Usage:
  # Piped from log output:
  cat profile.log | python3 tools/symbolize_profile.py

  # Direct hex addresses as arguments:
  python3 tools/symbolize_profile.py 0x42071404 0x4219f1a0

  # With specific ELF:
  python3 tools/symbolize_profile.py --elf path/to/firmware 0x42071404
"""

import argparse
import glob
import os
import re
import subprocess
import sys
from pathlib import Path


def find_addr2line() -> str:
    """Find xtensa-esp32s3-elf-addr2line in PATH or the standard rustup/esp toolchain."""
    # 1. Check PATH
    import shutil
    tool = shutil.which("xtensa-esp32s3-elf-addr2line")
    if tool:
        return tool

    # 2. Check ~/.rustup/toolchains/esp/
    home = Path.home()
    candidates = glob.glob(
        str(home / ".rustup/toolchains/esp/**/bin/xtensa-esp32s3-elf-addr2line"),
        recursive=True,
    )
    if candidates:
        return candidates[0]

    # 3. Check /opt/espressif
    candidates = glob.glob(
        "/opt/espressif/**/bin/xtensa-esp32s3-elf-addr2line", recursive=True
    )
    if candidates:
        return candidates[0]

    sys.exit(
        "Error: Could not find xtensa-esp32s3-elf-addr2line. "
        "Ensure ESP toolchain is installed or pass --addr2line."
    )


def find_default_elf() -> Path | None:
    """Find default release firmware ELF."""
    candidates = [
        Path("target/xtensa-esp32s3-none-elf/release/firmware"),
        Path("../target/xtensa-esp32s3-none-elf/release/firmware"),
        Path("/cache/cargo/target/t-encoder-worktree-main1-fde9/xtensa-esp32s3-none-elf/release/firmware"),
    ]
    # Check cache dirs matching worktree
    cache_matches = glob.glob(
        "/cache/cargo/target/**/xtensa-esp32s3-none-elf/release/firmware",
        recursive=True,
    )
    candidates.extend(Path(p) for p in cache_matches)

    for c in candidates:
        if c.is_file():
            return c
    return None


def symbolize_address(addr2line: str, elf_path: str, pc: str) -> tuple[str, str]:
    """Runs addr2line to get (function_name, file_line) for a PC."""
    try:
        res = subprocess.run(
            [addr2line, "-e", elf_path, "-f", "-C", "-p", pc],
            capture_output=True,
            text=True,
            check=True,
        )
        output = res.stdout.strip()
        # addr2line format with -p: "function at /path/file.rs:line"
        if " at " in output:
            func, loc = output.rsplit(" at ", 1)
            return func.strip(), loc.strip()
        return output, "??"
    except Exception as e:
        return f"Error: {e}", "??"


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Symbolize PC hotspot addresses from statistical profiler output."
    )
    parser.add_argument(
        "addresses",
        nargs="*",
        help="PC hex addresses (e.g. 0x42071404) or leave blank to read from stdin",
    )
    parser.add_argument(
        "--elf",
        help="Path to release firmware ELF file",
        default=None,
    )
    parser.add_argument(
        "--addr2line",
        help="Path to xtensa-esp32s3-elf-addr2line",
        default=None,
    )

    args = parser.parse_args()

    addr2line_path = args.addr2line or find_addr2line()

    elf_path = None
    if args.elf:
        elf_path = Path(args.elf)
        if not elf_path.is_file():
            sys.exit(f"Error: ELF file not found at {elf_path}")
    else:
        elf_path = find_default_elf()
        if not elf_path:
            sys.exit(
                "Error: Could not locate firmware ELF. Please specify with --elf <path>"
            )

    print(f"Using addr2line : {addr2line_path}")
    print(f"Using ELF       : {elf_path}")
    print("=" * 80)

    # Patterns to match defmt logs:
    # [PROFILE]   #1: 0x42071404 - 1523 samples (32.4%)
    # or raw 0x42...
    entry_pattern = re.compile(
        r"(?:#(?P<rank>\d+):\s+)?(?P<pc>0x[0-9a-fA-F]+)(?:\s+-\s+(?P<samples>\d+)\s+samples\s+\((?P<pct>[0-9.]+%)\))?"
    )

    entries = []

    if args.addresses:
        for item in args.addresses:
            m = entry_pattern.search(item)
            if m:
                entries.append(
                    {
                        "rank": m.group("rank") or "-",
                        "pc": m.group("pc"),
                        "samples": m.group("samples") or "-",
                        "pct": m.group("pct") or "-",
                    }
                )
            elif item.startswith("0x") or all(c in "0123456789abcdefABCDEF" for c in item):
                pc = item if item.startswith("0x") else f"0x{item}"
                entries.append({"rank": "-", "pc": pc, "samples": "-", "pct": "-"})
    else:
        # Read from stdin
        if sys.stdin.isatty():
            print("Reading from stdin... (Paste log lines and press Ctrl+D):")
        for line in sys.stdin:
            for match in entry_pattern.finditer(line):
                entries.append(
                    {
                        "rank": match.group("rank") or "-",
                        "pc": match.group("pc"),
                        "samples": match.group("samples") or "-",
                        "pct": match.group("pct") or "-",
                    }
                )

    if not entries:
        print("No PC addresses found.")
        return

    print(f"{'Rank':<5} | {'Samples':<8} | {'Pct':<7} | {'PC':<10} | {'Location / Function'}")
    print("-" * 80)

    for entry in entries:
        func, loc = symbolize_address(addr2line_path, str(elf_path), entry["pc"])
        # Format function and loc cleanly
        print(f"{entry['rank']:<5} | {entry['samples']:<8} | {entry['pct']:<7} | {entry['pc']:<10} | {func}")
        print(f"{' ':5} | {' ':8} | {' ':7} | {' ':10} |   --> {loc}")
        print("-" * 80)


if __name__ == "__main__":
    main()
