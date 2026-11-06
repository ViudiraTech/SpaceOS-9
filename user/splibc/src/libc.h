/*
 *       user/splibc/src/libc.h
 *       Pprivate syscall numbers and helpers
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#ifndef SPLIBC_LIBC_H
#define SPLIBC_LIBC_H

#include "splibc.h"

#define SYS_READ 0
#define SYS_WRITE 1
#define SYS_CLOSE 3
#define SYS_FSTAT 5
#define SYS_POLL 7
#define SYS_LSEEK 8
#define SYS_MMAP 9
#define SYS_MPROTECT 10
#define SYS_MUNMAP 11
#define SYS_BRK 12
#define SYS_RT_SIGACTION 13
#define SYS_RT_SIGPROCMASK 14
#define SYS_IOCTL 16
#define SYS_PREAD64 17
#define SYS_PWRITE64 18
#define SYS_ACCESS 21
#define SYS_PIPE 22
#define SYS_SCHED_YIELD 24
#define SYS_DUP 32
#define SYS_DUP2 33
#define SYS_PAUSE 34
#define SYS_NANOSLEEP 35
#define SYS_GETPID 39
#define SYS_SOCKET 41
#define SYS_CONNECT 42
#define SYS_ACCEPT 43
#define SYS_SENDTO 44
#define SYS_RECVFROM 45
#define SYS_SENDMSG 46
#define SYS_RECVMSG 47
#define SYS_SHUTDOWN 48
#define SYS_BIND 49
#define SYS_LISTEN 50
#define SYS_GETSOCKNAME 51
#define SYS_GETPEERNAME 52
#define SYS_SOCKETPAIR 53
#define SYS_SETSOCKOPT 54
#define SYS_GETSOCKOPT 55
#define SYS_CLONE 56
#define SYS_FORK 57
#define SYS_EXECVE 59
#define SYS_EXIT 60
#define SYS_WAIT4 61
#define SYS_KILL 62
#define SYS_FCNTL 72
#define SYS_FTRUNCATE 77
#define SYS_OPENAT 257
#define SYS_GETCWD 79
#define SYS_CHDIR 80
#define SYS_MKDIR 83
#define SYS_RMDIR 84
#define SYS_UNLINK 87
#define SYS_READLINK 89
#define SYS_GETPPID 110
#define SYS_SETPGID 109
#define SYS_SETSID 112
#define SYS_GETPGID 121
#define SYS_GETSID 124
#define SYS_RT_SIGPENDING 127
#define SYS_RT_SIGSUSPEND 130
#define SYS_SIGALTSTACK 131
#define SYS_MKNOD 133
#define SYS_GETTID 186
#define SYS_GETDENTS64 217
#define SYS_SET_TID_ADDRESS 218
#define SYS_CLOCK_GETTIME 228
#define SYS_EXIT_GROUP 231
#define SYS_EPOLL_WAIT 232
#define SYS_EPOLL_CTL 233
#define SYS_WAITID 247
#define SYS_NEWFSTATAT 262
#define SYS_UNLINKAT 263
#define SYS_MKNODAT 259
#define SYS_READLINKAT 267
#define SYS_FACCESSAT 269
#define SYS_TIMERFD_CREATE 283
#define SYS_TIMERFD_SETTIME 286
#define SYS_TIMERFD_GETTIME 287
#define SYS_ACCEPT4 288
#define SYS_SIGNALFD4 289
#define SYS_EVENTFD2 290
#define SYS_EPOLL_CREATE1 291
#define SYS_DUP3 292
#define SYS_PIPE2 293
#define SYS_MEMFD_CREATE 319
#define SYS_GETRANDOM 318
#define SYS_ARCH_PRCTL 158
#define SYS_INIT_MODULE 175
#define SYS_DELETE_MODULE 176
#define SYS_FINIT_MODULE 313

#define AT_REMOVEDIR 0x200

static inline long __sc0(long n) { return __syscall_ret(__syscall6(n, 0, 0, 0, 0, 0, 0)); }
static inline long __sc1(long n, long a) { return __syscall_ret(__syscall6(n, a, 0, 0, 0, 0, 0)); }
static inline long __sc2(long n, long a, long b) { return __syscall_ret(__syscall6(n, a, b, 0, 0, 0, 0)); }
static inline long __sc3(long n, long a, long b, long c) { return __syscall_ret(__syscall6(n, a, b, c, 0, 0, 0)); }
static inline long __sc4(long n, long a, long b, long c, long d) { return __syscall_ret(__syscall6(n, a, b, c, d, 0, 0)); }
static inline long __sc5(long n, long a, long b, long c, long d, long e) { return __syscall_ret(__syscall6(n, a, b, c, d, e, 0)); }
static inline long __sc6(long n, long a, long b, long c, long d, long e, long f) { return __syscall_ret(__syscall6(n, a, b, c, d, e, f)); }

#endif
