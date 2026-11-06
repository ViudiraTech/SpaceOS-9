#!/usr/bin/env python3
#
#       tools/gen_kallsyms.py
#       Patch the linked kernel with a front-coded symbol table
#
#       2026/11/6 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#
# The kernel reserves a PROGBITS section named .spk_kallsyms. This script
# reads the final ELF symbol table and rewrites that section in place:
#
#   header  magic "SPKS", version 1, count, flags, payload length
#   record  address:u64, type:u8, prefix:u16, suffix_len:u16, suffix
#
# Names are stored in address order with front-coding against the previous
# name, which roughly halves the Rust-mangled symbol table.  Addresses remain
# link-time values; the kernel adds the runtime slide reported by Limine.

import struct
import sys

MAGIC = 0x534B5053  # "SPKS"
VERSION = 1
HEADER = struct.Struct("<IIIIQ8x")
RECORD_FIXED = 13

SHF_WRITE = 0x1
SHF_ALLOC = 0x2
SHF_EXECINSTR = 0x4
SHT_SYMTAB = 2
SHT_NOBITS = 8
SHN_UNDEF = 0
SHN_LORESERVE = 0xFF00


class ElfError(Exception):
    pass


class Elf:
    def __init__(self, path: str):
        self.path = path
        self.data = bytearray(open(path, "rb").read())
        if len(self.data) < 64 or self.data[:4] != b"\x7fELF":
            raise ElfError("not an ELF file")
        if self.data[4] != 2 or self.data[5] != 1:
            raise ElfError("only ELF64 little-endian kernels are supported")
        self.shoff = self.u64(0x28)
        self.shentsize = self.u16(0x3A)
        self.shnum = self.u16(0x3C)
        self.shstrndx = self.u16(0x3E)
        if self.shentsize != 64 or self.shnum == 0 or self.shstrndx >= self.shnum:
            raise ElfError("invalid section header table")
        if self.shoff + self.shnum * 64 > len(self.data):
            raise ElfError("section header table is out of bounds")
        self.sections = [self.section(index) for index in range(self.shnum)]
        strings = self.sections[self.shstrndx]
        self.names = [
            self.cstring(strings["offset"] + section["name"], strings["size"])
            for section in self.sections
        ]

    def u16(self, offset: int) -> int:
        return struct.unpack_from("<H", self.data, offset)[0]

    def u32(self, offset: int) -> int:
        return struct.unpack_from("<I", self.data, offset)[0]

    def u64(self, offset: int) -> int:
        return struct.unpack_from("<Q", self.data, offset)[0]

    def section(self, index: int) -> dict:
        base = self.shoff + index * self.shentsize
        return {
            "name": self.u32(base),
            "type": self.u32(base + 4),
            "flags": self.u64(base + 8),
            "addr": self.u64(base + 16),
            "offset": self.u64(base + 24),
            "size": self.u64(base + 32),
            "link": self.u32(base + 40),
            "info": self.u32(base + 44),
            "align": self.u64(base + 48),
            "entsize": self.u64(base + 56),
        }

    def cstring(self, offset: int, limit: int) -> str:
        end = min(offset + limit, len(self.data))
        if offset < 0 or offset >= end:
            return ""
        raw = bytes(self.data[offset:end])
        nul = raw.find(b"\0")
        if nul < 0:
            return ""
        return raw[:nul].decode("utf-8", "replace")

    def find(self, name: str) -> dict | None:
        for index, section in enumerate(self.sections):
            if self.names[index] == name:
                return section
        return None


def symbol_kind(section: dict, binding: int, sym_type: int) -> str:
    if sym_type == 2 or section["flags"] & SHF_EXECINSTR:
        return "T" if binding in (1, 2) else "t"
    if section["flags"] & SHF_WRITE:
        if section["type"] == SHT_NOBITS:
            return "B" if binding in (1, 2) else "b"
        return "D" if binding in (1, 2) else "d"
    return "R" if binding in (1, 2) else "r"


def symbols(elf: Elf, include_data: bool):
    symtab = None
    for section in elf.sections:
        if section["type"] == SHT_SYMTAB:
            symtab = section
            break
    if symtab is None or symtab["entsize"] != 24 or symtab["link"] >= len(elf.sections):
        raise ElfError("kernel ELF has no usable .symtab")
    strtab = elf.sections[symtab["link"]]
    count = symtab["size"] // 24
    out = []
    for index in range(count):
        offset = symtab["offset"] + index * 24
        if offset + 24 > len(elf.data):
            raise ElfError("symbol table is out of bounds")
        name_off, info, _other, shndx, value, _size = struct.unpack_from(
            "<IBBHQQ", elf.data, offset
        )
        sym_type = info & 0xF
        binding = info >> 4
        if sym_type in (3, 4) or shndx == SHN_UNDEF or shndx >= SHN_LORESERVE:
            continue
        if shndx >= len(elf.sections):
            continue
        section = elf.sections[shndx]
        if not section["flags"] & SHF_ALLOC:
            continue
        if not include_data and not section["flags"] & SHF_EXECINSTR:
            continue
        name = elf.cstring(strtab["offset"] + name_off, strtab["size"])
        if not name or name.startswith("$") or name.startswith(".L"):
            continue
        out.append((value, symbol_kind(section, binding, sym_type), name))
    out.sort(key=lambda item: (item[0], item[2]))
    return out


def build(table) -> bytes:
    payload = bytearray()
    previous = b""
    for address, kind, name in table:
        encoded = name.encode("utf-8")
        prefix = 0
        limit = min(len(previous), len(encoded))
        while prefix < limit and previous[prefix] == encoded[prefix]:
            prefix += 1
        suffix = encoded[prefix:]
        payload += struct.pack("<QBHH", address, ord(kind), prefix, len(suffix))
        payload += suffix
        previous = encoded
    return bytes(HEADER.pack(MAGIC, VERSION, len(table), 0, len(payload))) + bytes(payload)


def main() -> int:
    args = [argument for argument in sys.argv[1:] if argument != "--all"]
    include_data = "--all" in sys.argv[1:]
    if len(args) != 1:
        print("usage: gen_kallsyms.py <kernel-elf> [--all]", file=sys.stderr)
        return 2
    path = args[0]
    elf = Elf(path)
    section = elf.find(".spk_kallsyms")
    if section is None:
        raise ElfError(".spk_kallsyms section is missing from the kernel")
    if section["type"] == SHT_NOBITS:
        raise ElfError(
            ".spk_kallsyms must stay PROGBITS; keep its non-zero initializer"
        )
    table = symbols(elf, include_data)
    blob = build(table)
    if len(blob) > section["size"]:
        raise ElfError(
            f"kallsyms blob needs {len(blob)} bytes but the section holds "
            f"{section['size']}; raise CAPACITY in src/kallsyms.rs"
        )
    start = section["offset"]
    elf.data[start : start + len(blob)] = blob
    elf.data[start + len(blob) : start + section["size"]] = b"\0" * (
        section["size"] - len(blob)
    )
    with open(path, "wb") as stream:
        stream.write(elf.data)
    print(
        f"kallsyms: {len(table)} symbols, {len(blob)} bytes "
        f"({section['size']} reserved)"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ElfError as error:
        print(f"gen_kallsyms: {error}", file=sys.stderr)
        sys.exit(1)
