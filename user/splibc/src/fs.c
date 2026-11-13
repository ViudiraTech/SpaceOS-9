/*
 *       user/splibc/src/fs.c
 *       Pathname filesystem wrappers
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "libc.h"

int openat(int dirfd, const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        __builtin_va_list arguments;
        __builtin_va_start(arguments, flags);
        mode = (mode_t)__builtin_va_arg(arguments, int);
        __builtin_va_end(arguments);
    }
    return __sc4(SYS_OPENAT, dirfd, (long)path, flags, mode);
}

int open(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        __builtin_va_list arguments;
        __builtin_va_start(arguments, flags);
        mode = (mode_t)__builtin_va_arg(arguments, int);
        __builtin_va_end(arguments);
    }
    return __sc4(SYS_OPENAT, AT_FDCWD, (long)path, flags, mode);
}

int stat(const char *path, struct stat *st) {
    return __sc4(SYS_NEWFSTATAT, AT_FDCWD, (long)path, (long)st, 0);
}

int fstat(int fd, struct stat *st) {
    return __sc2(SYS_FSTAT, fd, (long)st);
}

int mkdir(const char *path, mode_t mode) {
    return __sc2(SYS_MKDIR, (long)path, mode);
}

int mknod(const char *path, mode_t mode, dev_t dev) {
    return __sc4(SYS_MKNODAT, AT_FDCWD, (long)path, mode, dev);
}

int unlink(const char *path) {
    return __sc3(SYS_UNLINKAT, AT_FDCWD, (long)path, 0);
}

int rmdir(const char *path) {
    return __sc3(SYS_UNLINKAT, AT_FDCWD, (long)path, AT_REMOVEDIR);
}

int chdir(const char *path) {
    return __sc1(SYS_CHDIR, (long)path);
}

ssize_t readlink(const char *path, char *buf, size_t size) {
    return __sc4(SYS_READLINKAT, AT_FDCWD, (long)path, (long)buf, (long)size);
}
