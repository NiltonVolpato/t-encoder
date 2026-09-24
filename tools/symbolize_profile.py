#!/usr/bin/env python3
# Copyright © 2026 Nilton Volpato
# SPDX-License-Identifier: MIT

"""Symbolizes statistical profiler PC / callsite addresses from firmware logs.

Usage:
  # Piped from log output:
  cat profile.log | python3 tools/symbolize_profile.py

  # Direct hex addresses as arguments:
  python3 tools/symbolize_profile.py 0x420ca056 0x42089656

  # With specific ELF:
  python3 tools/symbolize_profile.py --elf path/to/firmware 0x420ca056

  # If addresses are raw Return Addresses (unadjusted by firmware):
  python3 tools/symbolize_profile.py --raw-ra 0x420ca059
"""

import argparse
import glob
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path


def find_addr2line() -> str:
    """Find xtensa-esp32s3-elf-addr2line in PATH or standard rustup/esp toolchains."""
    # 1. Check PATH
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
        return sorted(candidates, reverse=True)[0]

    # 3. Check /opt/espressif
    candidates = glob.glob(
        "/opt/espressif/**/bin/xtensa-esp32s3-elf-addr2line", recursive=True
    )
    if candidates:
        return sorted(candidates, reverse=True)[0]

    sys.exit(
        "Error: Could not find xtensa-esp32s3-elf-addr2line. "
        "Ensure ESP toolchain is installed or pass --addr2line."
    )


def find_default_elf() -> Path | None:
    """Find default release firmware ELF dynamically relative to workspace target."""
    script_dir = Path(__file__).resolve().parent
    workspace_root = script_dir.parent

    candidates: list[Path] = []
    seen_dirs: set[Path] = set()

    # 1. Search candidate target directories relative to workspace and cwd
    search_dirs = [
        Path.cwd() / "target" / "xtensa-esp32s3-none-elf" / "release",
        Path.cwd() / "esp32-devices" / "target" / "xtensa-esp32s3-none-elf" / "release",
        workspace_root / "esp32-devices" / "target" / "xtensa-esp32s3-none-elf" / "release",
        workspace_root / "target" / "xtensa-esp32s3-none-elf" / "release",
        Path.cwd() / "target" / "xtensa-esp32s3-none-elf" / "debug",
        workspace_root / "esp32-devices" / "target" / "xtensa-esp32s3-none-elf" / "debug",
    ]

    # 2. Also query cargo metadata in case CARGO_TARGET_DIR is configured
    for probe_dir in [workspace_root / "esp32-devices", workspace_root, Path.cwd()]:
        if probe_dir.is_dir():
            try:
                res = subprocess.run(
                    ["cargo", "metadata", "--format-version", "1", "--no-deps"],
                    cwd=probe_dir,
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                if res.returncode == 0:
                    data = json.loads(res.stdout)
                    target_dir = Path(data.get("target_directory", ""))
                    if target_dir:
                        search_dirs.append(target_dir / "xtensa-esp32s3-none-elf" / "release")
                        search_dirs.append(target_dir / "xtensa-esp32s3-none-elf" / "debug")
                    break
            except Exception:
                pass

    for d in search_dirs:
        try:
            resolved = d.resolve()
        except Exception:
            resolved = d
        if resolved in seen_dirs or not d.is_dir():
            continue
        seen_dirs.add(resolved)

        for f in d.iterdir():
            if (
                f.is_file()
                and not f.name.endswith((".d", ".rlib", ".rmeta", ".a", ".lock"))
                and not f.name.startswith(".")
            ):
                candidates.append(f)

    if not candidates:
        return None

    # Preferred known binary names if present
    preferred_names = {"waveshare_knob_1_8", "lilygo_t_encoder_pro", "firmware"}
    preferred = [c for c in candidates if c.name in preferred_names]
    if preferred:
        preferred.sort(key=lambda f: f.stat().st_mtime, reverse=True)
        return preferred[0]

    # Otherwise return the most recently modified binary
    candidates.sort(key=lambda f: f.stat().st_mtime, reverse=True)
    return candidates[0]


def symbolize_address(
    addr2line: str,
    elf_path: str,
    pc_str: str,
    raw_ra: bool = False,
    inlines: bool = True,
) -> list[tuple[str, str]]:
    """Runs addr2line to get inlined call stack frames [(func, loc), ...] for an address.

    If raw_ra is True, subtracts 3 bytes (Xtensa call instruction offset) to point
    to the call instruction rather than the landing instruction after return.
    """
    try:
        val = int(pc_str, 16)
        if raw_ra and val > 3:
            query_pc = f"0x{val - 3:08x}"
        else:
            query_pc = f"0x{val:08x}"

        cmd = [addr2line, "-e", elf_path, "-f", "-C", "-p"]
        if inlines:
            cmd.append("-i")
        cmd.append(query_pc)

        res = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=True,
        )
        lines = [line.strip() for line in res.stdout.strip().splitlines() if line.strip()]
        frames: list[tuple[str, str]] = []
        for line in lines:
            line_clean = line.removeprefix("(inlined by) ").strip()
            if " at " in line_clean:
                func, loc = line_clean.rsplit(" at ", 1)
                frames.append((func.strip(), loc.strip()))
            else:
                frames.append((line_clean, "??"))
        return frames or [("??", "??")]
    except Exception as e:
        return [(f"Error: {e}", "??")]


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Symbolize PC and callsite hotspot addresses from statistical profiler output."
    )
    parser.add_argument(
        "addresses",
        nargs="*",
        help="Hex addresses (e.g. 0x420ca056) or leave blank to read from stdin",
    )
    parser.add_argument(
        "--elf",
        help="Path to release firmware ELF file (auto-detected if omitted)",
        default=None,
    )
    parser.add_argument(
        "--addr2line",
        help="Path to xtensa-esp32s3-elf-addr2line",
        default=None,
    )
    parser.add_argument(
        "--raw-ra",
        action="store_true",
        help="Subtract 3 bytes from addresses (only use for raw return addresses without RA_OFFSET subtracted in firmware)",
    )
    parser.add_argument(
        "--no-inlines",
        action="store_true",
        help="Do not unwind inlined call stacks (default shows full inlining hierarchy)",
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
                "Error: Could not locate firmware ELF in target directory. Please specify with --elf <path>"
            )

    print(f"Using addr2line : {addr2line_path}")
    print(f"Using ELF       : {elf_path}")
    if args.raw_ra:
        print("Mode            : RAW Return Address (subtracting 3-byte call offset)")
    else:
        print("Mode            : Direct PC / Call Site Address")
    print("=" * 80)

    # Patterns to match defmt logs:
    # [PROFILE]   #1: 0x420ca056 - 1523 samples (32.4%)
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
        print("No addresses found.")
        return

    print(f"{'Rank':<5} | {'Samples':<8} | {'Pct':<7} | {'Address':<10} | {'Location / Function'}")
    print("-" * 80)

    inlines_enabled = not args.no_inlines

    for entry in entries:
        frames = symbolize_address(
            addr2line_path,
            str(elf_path),
            entry["pc"],
            raw_ra=args.raw_ra,
            inlines=inlines_enabled,
        )

        top_func, top_loc = frames[0]
        print(f"{entry['rank']:<5} | {entry['samples']:<8} | {entry['pct']:<7} | {entry['pc']:<10} | {top_func}")
        print(f"{' ':5} | {' ':8} | {' ':7} | {' ':10} |   --> {top_loc}")

        # If there are inlined frames, print the inlining hierarchy
        for inlined_func, inlined_loc in frames[1:]:
            print(f"{' ':5} | {' ':8} | {' ':7} | {' ':10} |   (inlined by) {inlined_func}")
            print(f"{' ':5} | {' ':8} | {' ':7} | {' ':10} |     --> {inlined_loc}")

        print("-" * 80)


if __name__ == "__main__":
    main()
