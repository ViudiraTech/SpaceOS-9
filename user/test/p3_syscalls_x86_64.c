/*
 *       user/test/p3_syscalls_x86_64.c
 *       Kernel P3a: mount namespace, bind/read-only, chroot and seccomp
 *
 *       2026/11/22 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "include/test_syscall.h"

#define SYS_MKDIR 83
#define SYS_UNSHARE 272
#define SYS_MOUNT 165
#define SYS_CHROOT 161
#define SYS_PRCTL 157
#define SYS_SECCOMP 317
#define SYS_GETPPID 110

#define CLONE_NEWNS 0x00020000
#define MS_RDONLY 1
#define MS_BIND 4096
#define PR_SET_NO_NEW_PRIVS 38
#define SECCOMP_SET_MODE_FILTER 1

#define O_CREAT 0100
#define O_TRUNC 01000
#define O_WRONLY 1

struct sock_filter {
    u16 code;
    unsigned char jt;
    unsigned char jf;
    u32 k;
};

struct sock_fprog {
    u16 len;
    u16 pad[3];
    struct sock_filter *filter;
};

static u64 length(const char *text) {
    u64 n = 0;
    while (text[n] != 0) {
        n++;
    }
    return n;
}

static void fail(const char *why) {
    sys_write(2, why, length(why));
    sys_exit_group(41);
}

static i64 write_file(const char *path, const char *data) {
    i64 fd = sys_openat(AT_FDCWD, path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) {
        return fd;
    }
    i64 n = sys_write(fd, data, length(data));
    sys_close(fd);
    return n;
}

int main(void) {
    if (syscall6(SYS_MKDIR, (i64)"/p3src", 0755, 0, 0, 0, 0) != 0) {
        fail("mkdir /p3src failed\n");
    }
    if (syscall6(SYS_MKDIR, (i64)"/p3dst", 0755, 0, 0, 0, 0) != 0) {
        fail("mkdir /p3dst failed\n");
    }
    if (write_file("/p3src/hello.txt", "p3-bind\n") < 0) {
        fail("write /p3src/hello.txt failed\n");
    }

    if (syscall3(SYS_UNSHARE, CLONE_NEWNS, 0, 0) != 0) {
        fail("unshare(CLONE_NEWNS) failed\n");
    }
    if (syscall6(SYS_MOUNT, (i64)"/p3src", (i64)"/p3dst", 0,
                 MS_BIND | MS_RDONLY, 0, 0) != 0) {
        fail("bind mount failed\n");
    }

    char buffer[16];
    i64 fd = sys_openat(AT_FDCWD, "/p3dst/hello.txt", 0, 0);
    if (fd < 0) {
        fail("read bind mount failed\n");
    }
    i64 n = sys_read(fd, buffer, sizeof(buffer));
    sys_close(fd);
    if (n != 8 || buffer[0] != 'p' || buffer[7] != '\n') {
        fail("bind mount content mismatch\n");
    }
    fd = sys_openat(AT_FDCWD, "/p3dst/hello.txt", O_WRONLY, 0);
    if (fd != -30) { /* EROFS */
        fail("read-only bind mount accepted a write\n");
    }

    if (syscall3(SYS_CHROOT, (i64)"/p3dst", 0, 0) != 0) {
        fail("chroot failed\n");
    }
    fd = sys_openat(AT_FDCWD, "/hello.txt", 0, 0);
    if (fd < 0) {
        fail("chroot-relative open failed\n");
    }
    sys_close(fd);
    fd = sys_openat(AT_FDCWD, "/p3src/hello.txt", 0, 0);
    if (fd != -2) { /* ENOENT */
        fail("chroot did not contain absolute paths\n");
    }

    if (syscall3(SYS_PRCTL, PR_SET_NO_NEW_PRIVS, 1, 0) != 0) {
        fail("PR_SET_NO_NEW_PRIVS failed\n");
    }
    static struct sock_filter filter[] = {
        {0x20, 0, 0, 0},                 /* ld [0] (syscall nr) */
        {0x15, 0, 1, SYS_GETPPID},       /* jeq getppid ? next : skip */
        {0x06, 0, 0, 0x0005002a},        /* ret ERRNO(42) */
        {0x06, 0, 0, 0x7fff0000},        /* ret ALLOW */
    };
    struct sock_fprog program = {4, {0, 0, 0}, filter};
    if (syscall3(SYS_SECCOMP, SECCOMP_SET_MODE_FILTER, 0, (i64)&program) != 0) {
        fail("seccomp filter install failed\n");
    }
    if (syscall3(SYS_GETPPID, 0, 0, 0) != -42) {
        fail("seccomp did not return configured errno\n");
    }
    if (sys_getpid() <= 0) {
        fail("seccomp blocked an allowed syscall\n");
    }
    return 44;
}
