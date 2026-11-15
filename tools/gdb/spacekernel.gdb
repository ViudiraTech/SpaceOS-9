#
#       tools/gdb/spacekernel.gdb
#       Attach GDB to a QEMU gdbstub with kernel symbols at the load address
#
#       2026/11/15 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#
# The kernel is linked as a PIE at 0 and Limine loads it at
# 0xffffffff80000000 when KASLR is off, so symbols need that offset.
#
#   make iso ARCH=x86_64 CONFIG_KASLR=n [CONFIG_BOOT_SELF_TEST=y]
#   python3 tools/qemu_test.py ... --gdb 1234 &
#   gdb -x tools/gdb/spacekernel.gdb
#
# Each vCPU is a GDB thread: `info threads`, `thread apply all bt`.

set pagination off
set confirm off
symbol-file -o 0xffffffff80000000 target/x86_64-unknown-none/debug/spacekernel
target remote 127.0.0.1:1234
