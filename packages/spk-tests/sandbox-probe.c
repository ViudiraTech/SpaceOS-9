/*
 *       packages/spk-tests/sandbox-probe.c
 *       Verify script sandbox denials from inside an .INSTALL script
 *
 *       2026/11/22 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "test_syscall.h"

#define SYS_SOCKET 41
#define SYS_CLONE 56
#define SYS_PTRACE 101
#define SYS_MKNOD 133
#define SYS_CHROOT 161
#define SYS_MOUNT 165
#define SYS_CLONE3 435
#define CLONE_NEWNS 0x00020000
#define O_CREAT 0100
#define O_WRONLY 1

static u64 text_length(const char *text) {
    u64 length = 0;
    while (text[length] != 0) {
        length++;
    }
    return length;
}

static int expect_failure(const char *what, i64 result) {
    if (result >= 0) {
        sys_write(2, what, text_length(what));
        sys_write(2, "\n", 1);
        return 1;
    }
    return 0;
}

int main(void) {
    int failures = 0;
    failures += expect_failure(
        "socket was allowed",
        syscall6(SYS_SOCKET, 2 /* AF_INET */, 1 /* SOCK_STREAM */, 0, 0, 0, 0)
    );
    failures += expect_failure(
        "mount was allowed",
        syscall6(SYS_MOUNT, (i64)"none", (i64)"/tmp", (i64)"ramfs", 0, 0, 0)
    );
    failures += expect_failure(
        "chroot was allowed",
        syscall3(SYS_CHROOT, (i64)"/", 0, 0)
    );
    failures += expect_failure(
        "clone(CLONE_NEWNS) was allowed",
        syscall6(SYS_CLONE, 17 | CLONE_NEWNS, 0, 0, 0, 0, 0)
    );
    u64 clone_args[8] = {0};
    failures += expect_failure(
        "clone3 was allowed",
        syscall3(SYS_CLONE3, (i64)clone_args, sizeof(clone_args), 0)
    );
    failures += expect_failure(
        "ptrace was allowed",
        syscall6(SYS_PTRACE, 0, 0, 0, 0, 0, 0)
    );
    failures += expect_failure(
        "mknod was allowed",
        syscall3(SYS_MKNOD, (i64)"/stage/sandbox-dev", 0100644, 0)
    );
    failures += expect_failure(
        "source write was allowed",
        sys_openat(AT_FDCWD, "/source/etc/spk-script-write", O_CREAT | O_WRONLY, 0644)
    );
    failures += expect_failure(
        "proc hidepid leaked another process",
        sys_openat(AT_FDCWD, "/proc/1/stat", 0, 0)
    );
    failures += expect_failure(
        "pivot_root left the old root visible",
        sys_openat(AT_FDCWD, "/oldroot/etc/passwd", 0, 0)
    );
    if (failures != 0) {
        sys_write(2, "SANDBOX_ESCAPE\n", 15);
        return 1;
    }
    sys_write(1, "SANDBOX_OK\n", 11);
    return 0;
}
