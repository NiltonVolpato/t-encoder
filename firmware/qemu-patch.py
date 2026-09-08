#!/usr/bin/env python3
"""Nop out the one instruction QEMU's esp32s3 machine cannot get past.

`esp_hal::init` ends up in `enable_pll_clk_impl`, which starts the BBPLL
self-calibration and then waits for it:

    while I2C_ANA_MST.ana_conf0().bbpll_cal_done().bit_is_clear() {}

QEMU models no I2C_ANA_MST — the analog/PLL block at 0x6000_E000 reads back 0
and drops writes — so the done bit never arrives and the app hangs before its
first log line. Every `CpuClock` preset routes through the PLL, so there is no
`esp_hal::Config` that avoids the wait; the loop has to come out of the binary.

This turns the backward `beqz` that closes that loop into a 3-byte `nop`, so
the calibration is started and simply not waited on. It patches the *ELF*, not
the flash image: the image carries a checksum and a SHA256 that the bootloader
verifies, and only `espflash save-image` recomputes them.

Deliberately strict — it refuses to write anything unless it finds exactly one
loop matching both the shape and the I2C_ANA_MST literal, so an esp-hal upgrade
that moves this code fails loudly instead of silently corrupting a build.

Usage: qemu-patch.py <objdump> <elf>   (the ELF is rewritten in place)
"""

import re
import subprocess
import sys

# `nop` is the 3-byte 0x0020f0, stored little-endian — the same width as the
# `beqz` it replaces, so nothing after it shifts.
NOP = bytes.fromhex("f02000")
ANA_CONF0 = "6000e040"


def run(*args):
    return subprocess.run(args, capture_output=True, text=True, check=True).stdout


def sections(readelf, elf):
    """(vaddr, file offset, size) for every PROGBITS section."""
    out = run(readelf, "-S", "-W", elf)
    pattern = r"\[\s*\d+\]\s+\S+\s+PROGBITS\s+([0-9a-f]+)\s+([0-9a-f]+)\s+([0-9a-f]+)"
    return [tuple(int(g, 16) for g in m.groups()) for m in re.finditer(pattern, out)]


def file_offset(secs, vaddr):
    for addr, off, size in secs:
        if addr <= vaddr < addr + size:
            return off + (vaddr - addr)
    sys.exit(f"qemu-patch: no section contains {vaddr:#010x}")


def instructions(objdump, elf):
    """(vaddr, byte width, mnemonic) for every disassembled instruction."""
    out = run(objdump, "-d", elf)
    found = []
    for line in out.splitlines():
        m = re.match(r"^([0-9a-f]{8}):\t([0-9a-f ]+)\t(.*)$", line)
        if m:
            width = len(m.group(2).replace(" ", "")) // 2
            found.append((int(m.group(1), 16), width, m.group(3).strip()))
    return found


def calibration_spin(insns):
    """The backward `beqz` closing the BBPLL calibration wait, if there is one."""
    hits = []
    for i, (addr, width, text) in enumerate(insns):
        # objdump renders the target as an address followed by `<symbol+off>`.
        m = re.match(r"beqz\s+a\d+, ([0-9a-f]+)\b", text)
        if not m:
            continue
        target = int(m.group(1), 16)
        if target >= addr:  # forward branch: not a loop
            continue
        body = [t for a, _, t in insns[max(0, i - 8):i] if a >= target]
        if any(ANA_CONF0 in t for t in body):
            hits.append((addr, width))
    return hits


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    objdump, elf = sys.argv[1], sys.argv[2]
    readelf = objdump.replace("objdump", "readelf")

    hits = calibration_spin(instructions(objdump, elf))
    if len(hits) != 1:
        sys.exit(
            f"qemu-patch: expected 1 BBPLL calibration spin, found {len(hits)}. "
            "esp-hal's clock code has moved — re-read enable_pll_clk_impl."
        )

    addr, width = hits[0]
    off = file_offset(sections(readelf, elf), addr)
    if width != len(NOP):
        sys.exit(f"qemu-patch: branch at {addr:#010x} is {width} bytes, not {len(NOP)}")

    data = bytearray(open(elf, "rb").read())
    data[off:off + width] = NOP
    open(elf, "wb").write(bytes(data))
    print(f"qemu-patch: nop'd the BBPLL calibration wait at {addr:#010x}")


main()
