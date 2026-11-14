/*
 *       user/splibc/src/syscalls.c
 *       Basic Linux ABI wrappers and errno
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "libc.h"

int errno;

long __syscall_ret(long result) {
    if (result < 0 && result >= -4095) {
        errno = (int)-result;
        return -1;
    }
    return result;
}

ssize_t read(int fd, void *buf, size_t count) {
    return __sc3(SYS_READ, fd, (long)buf, (long)count);
}

ssize_t write(int fd, const void *buf, size_t count) {
    return __sc3(SYS_WRITE, fd, (long)buf, (long)count);
}

ssize_t pread(int fd, void *buf, size_t count, off_t offset) {
    return __sc4(SYS_PREAD64, fd, (long)buf, (long)count, offset);
}

ssize_t pwrite(int fd, const void *buf, size_t count, off_t offset) {
    return __sc4(SYS_PWRITE64, fd, (long)buf, (long)count, offset);
}

int close(int fd) {
    return __sc1(SYS_CLOSE, fd);
}

off_t lseek(int fd, off_t offset, int whence) {
    return __sc3(SYS_LSEEK, fd, offset, whence);
}

int ftruncate(int fd, off_t length) {
    return __sc2(SYS_FTRUNCATE, fd, length);
}

int dup(int fd) {
    return __sc1(SYS_DUP, fd);
}

int dup2(int oldfd, int newfd) {
    return __sc2(SYS_DUP2, oldfd, newfd);
}

int pipe(int fds[2]) {
    return __sc1(SYS_PIPE, (long)fds);
}

int pipe2(int fds[2], int flags) {
    return __sc2(SYS_PIPE2, (long)fds, flags);
}

int access(const char *path, int mode) {
    return __sc3(SYS_FACCESSAT, AT_FDCWD, (long)path, mode);
}

int ioctl(int fd, unsigned long request, void *arg) {
    return __sc3(SYS_IOCTL, fd, (long)request, (long)arg);
}

int init_module(const void *image, unsigned long length, const char *parameters) {
    return __sc3(SYS_INIT_MODULE, (long)image, (long)length, (long)parameters);
}

int finit_module(int fd, const char *parameters, int flags) {
    return __sc3(SYS_FINIT_MODULE, fd, (long)parameters, flags);
}

int delete_module(const char *name, int flags) {
    return __sc2(SYS_DELETE_MODULE, (long)name, flags);
}

void sync(void) {
    (void)__sc0(SYS_SYNC);
}

int umount2(const char *target, int flags) {
    return __sc2(SYS_UMOUNT2, (long)target, flags);
}

int reboot(unsigned int cmd) {
    return __sc4(SYS_REBOOT, (long)0xFEE1DEADu, (long)672274793u, (long)cmd, 0);
}

int fcntl(int fd, int cmd, long arg) {
    return __sc3(SYS_FCNTL, fd, cmd, arg);
}

char *getcwd(char *buf, size_t size) {
    long result = __sc2(SYS_GETCWD, (long)buf, (long)size);
    return result < 0 ? NULL : buf;
}

int rename(const char *oldpath, const char *newpath) {
    return __sc2(SYS_RENAME, (long)oldpath, (long)newpath);
}

int symlink(const char *target, const char *linkpath) {
    return __sc2(SYS_SYMLINK, (long)target, (long)linkpath);
}

uid_t getuid(void) {
    return (uid_t)__sc0(SYS_GETUID);
}

gid_t getgid(void) {
    return (gid_t)__sc0(SYS_GETGID);
}

uid_t geteuid(void) {
    return (uid_t)__sc0(SYS_GETEUID);
}

gid_t getegid(void) {
    return (gid_t)__sc0(SYS_GETEGID);
}
