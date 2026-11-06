/*
 *       user/splibc/include/splibc.h
 *       SpaceOS-9 freestanding C library interface
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#ifndef SPLIBC_H
#define SPLIBC_H

#if !defined(__x86_64__)
#error "splibc currently targets the x86_64 Linux ABI"
#endif

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long u64;
typedef signed char i8;
typedef short i16;
typedef int i32;
typedef long i64;

typedef u64 size_t;
typedef i64 ssize_t;
typedef i64 off_t;
typedef i32 pid_t;
typedef u32 mode_t;
typedef u32 uid_t;
typedef u32 gid_t;
typedef i64 time_t;
typedef u64 sigset_t;
typedef u64 dev_t;
typedef u64 ino_t;
typedef u64 nlink_t;
typedef i64 blksize_t;
typedef i64 blkcnt_t;
typedef u32 socklen_t;

#ifndef NULL
#define NULL ((void *)0)
#endif
#define EOF (-1)

#define PATH_MAX 4096
#define NAME_MAX 255

/* ---- errno -------------------------------------------------------------- */
extern int errno;
#define EPERM 1
#define ENOENT 2
#define ESRCH 3
#define EINTR 4
#define EIO 5
#define ENXIO 6
#define E2BIG 7
#define ENOEXEC 8
#define EBADF 9
#define ECHILD 10
#define EAGAIN 11
#define ENOMEM 12
#define EACCES 13
#define EFAULT 14
#define EBUSY 16
#define EEXIST 17
#define ENODEV 19
#define ENOTDIR 20
#define EISDIR 21
#define EINVAL 22
#define ENFILE 23
#define EMFILE 24
#define ENOTTY 25
#define EFBIG 27
#define ENOSPC 28
#define ESPIPE 29
#define EROFS 30
#define EPIPE 32
#define ERANGE 34
#define ENAMETOOLONG 36
#define ENOSYS 38
#define ENOTEMPTY 39
#define ELOOP 40
#define EPROTO 71
#define ENOTSOCK 88
#define EDESTADDRREQ 89
#define EMSGSIZE 90
#define EPROTOTYPE 91
#define ENOPROTOOPT 92
#define EAFNOSUPPORT 97
#define EADDRINUSE 98
#define ECONNRESET 104
#define EISCONN 106
#define ENOTCONN 107
#define ETIMEDOUT 110
#define ECONNREFUSED 111

/* ---- open / seek / stat ------------------------------------------------- */
#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_ACCMODE 3
#define O_CREAT 0100
#define O_EXCL 0200
#define O_NOCTTY 0400
#define O_TRUNC 01000
#define O_APPEND 02000
#define O_NONBLOCK 04000
#define O_DIRECTORY 0200000
#define O_CLOEXEC 02000000

#define AT_FDCWD (-100)

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

#define S_IFMT 0170000
#define S_IFIFO 0010000
#define S_IFCHR 0020000
#define S_IFDIR 0040000
#define S_IFBLK 0060000
#define S_IFREG 0100000
#define S_IFLNK 0120000
#define S_IFSOCK 0140000
#define S_ISREG(m) (((m) & S_IFMT) == S_IFREG)
#define S_ISDIR(m) (((m) & S_IFMT) == S_IFDIR)
#define S_ISCHR(m) (((m) & S_IFMT) == S_IFCHR)
#define S_ISFIFO(m) (((m) & S_IFMT) == S_IFIFO)
#define S_ISSOCK(m) (((m) & S_IFMT) == S_IFSOCK)

struct stat {
    dev_t st_dev;
    ino_t st_ino;
    nlink_t st_nlink;
    mode_t st_mode;
    uid_t st_uid;
    gid_t st_gid;
    u32 __pad0;
    dev_t st_rdev;
    i64 st_size;
    i64 st_blksize;
    i64 st_blocks;
    i64 st_atime;
    i64 st_atime_nsec;
    i64 st_mtime;
    i64 st_mtime_nsec;
    i64 st_ctime;
    i64 st_ctime_nsec;
    i64 __unused[3];
};

struct linux_dirent64 {
    u64 d_ino;
    i64 d_off;
    u16 d_reclen;
    u8 d_type;
    char d_name[];
} __attribute__((packed));

struct dirent {
    u64 d_ino;
    i64 d_off;
    u16 d_reclen;
    u8 d_type;
    char d_name[256];
};

#define DT_UNKNOWN 0
#define DT_FIFO 1
#define DT_CHR 2
#define DT_DIR 4
#define DT_BLK 6
#define DT_REG 8
#define DT_LNK 10
#define DT_SOCK 12

typedef struct __dirstream {
    int fd;
    int index;
    int size;
    char buffer[32768];
    struct dirent entry;
} DIR;

struct dirent *readdir(DIR *dir);
DIR *opendir(const char *path);
int closedir(DIR *dir);

/* ---- fcntl / ioctl / termios -------------------------------------------- */
#define F_DUPFD 0
#define F_GETFD 1
#define F_SETFD 2
#define F_GETFL 3
#define F_SETFL 4
#define F_DUPFD_CLOEXEC 1030
#define F_ADD_SEALS 1033
#define F_GET_SEALS 1034
#define FD_CLOEXEC 1

#define TCGETS 0x5401
#define TCSETS 0x5402
#define TCSETSW 0x5403
#define TCSETSF 0x5404
#define TIOCGPGRP 0x540F
#define TIOCSPGRP 0x5410
#define TIOCGWINSZ 0x5413
#define TIOCGSID 0x5429

#define ICANON 0000002
#define ECHO 0000010
#define ECHOE 0000020
#define ECHOK 0000040
#define ISIG 0000001
#define ICRNL 0000400
#define IXON 0002000
#define OPOST 0000001

struct termios {
    u32 c_iflag;
    u32 c_oflag;
    u32 c_cflag;
    u32 c_lflag;
    u8 c_line;
    u8 c_cc[19];
} __attribute__((packed));

/* ---- signals ------------------------------------------------------------ */
#define SIGHUP 1
#define SIGINT 2
#define SIGQUIT 3
#define SIGILL 4
#define SIGTRAP 5
#define SIGABRT 6
#define SIGBUS 7
#define SIGFPE 8
#define SIGKILL 9
#define SIGUSR1 10
#define SIGSEGV 11
#define SIGUSR2 12
#define SIGPIPE 13
#define SIGALRM 14
#define SIGTERM 15
#define SIGCHLD 17
#define SIGCONT 18
#define SIGSTOP 19
#define SIGTSTP 20
#define SIGTTIN 21
#define SIGTTOU 22
#define SIGWINCH 28

#define SIG_DFL ((void (*)(int))0)
#define SIG_IGN ((void (*)(int))1)
#define SIG_ERR ((void (*)(int))-1)

#define SA_NOCLDSTOP 0x00000001
#define SA_NOCLDWAIT 0x00000002
#define SA_SIGINFO 0x00000004
#define SA_RESTORER 0x04000000
#define SA_ONSTACK 0x08000000
#define SA_RESTART 0x10000000
#define SA_NODEFER 0x40000000
#define SA_RESETHAND 0x80000000

#define sigemptyset(set) (*(set) = 0)
#define sigfillset(set) (*(set) = ~0UL)
#define sigaddset(set, sig) (*(set) |= 1UL << ((sig) - 1))
#define sigdelset(set, sig) (*(set) &= ~(1UL << ((sig) - 1)))
#define sigismember(set, sig) ((*(set) >> ((sig) - 1)) & 1)

#define SIG_BLOCK 0
#define SIG_UNBLOCK 1
#define SIG_SETMASK 2
#define SIGSTKSZ 8192

struct sigaction {
    void (*sa_handler)(int);
    u64 sa_flags;
    void (*sa_restorer)(void);
    sigset_t sa_mask;
};

struct siginfo {
    i32 si_signo;
    i32 si_errno;
    i32 si_code;
    i32 __pad;
    i32 si_pid;
    u32 si_uid;
    i32 si_status;
    char __rest[128 - 28];
};

typedef struct {
    void *ss_sp;
    i32 ss_flags;
    u64 ss_size;
} stack_t;

extern void splibc_sigreturn(void);

/* ---- process / wait ----------------------------------------------------- */
#define WNOHANG 1
#define WUNTRACED 2
#define WEXITED 4
#define WCONTINUED 8
#define WSTOPPED 2
#define WNOWAIT 0x01000000
#define P_ALL 0
#define P_PID 1
#define P_PGID 2

#define CLD_EXITED 1
#define CLD_KILLED 2
#define CLD_DUMPED 3
#define CLD_STOPPED 5
#define CLD_CONTINUED 6

#define WIFEXITED(s) (((s) & 0x7f) == 0)
#define WEXITSTATUS(s) (((s) >> 8) & 0xff)
#define WIFSIGNALED(s) ((((s) & 0x7f) != 0) && (((s) & 0x7f) != 0x7f))
#define WTERMSIG(s) ((s) & 0x7f)
#define WIFSTOPPED(s) (((s) & 0xff) == 0x7f)
#define WSTOPSIG(s) (((s) >> 8) & 0xff)
#define WIFCONTINUED(s) ((s) == 0xffff)

/* ---- mmap / poll / epoll ------------------------------------------------ */
#define PROT_READ 1
#define PROT_WRITE 2
#define PROT_EXEC 4
#define MAP_SHARED 1
#define MAP_PRIVATE 2
#define MAP_FIXED 0x10
#define MAP_ANONYMOUS 0x20
#define MAP_FAILED ((void *)-1)

#define POLLIN 0x001
#define POLLOUT 0x004
#define POLLERR 0x008
#define POLLHUP 0x010
#define POLLNVAL 0x020
#define POLLRDHUP 0x2000

struct pollfd {
    int fd;
    short events;
    short revents;
};

#define EPOLLIN 0x001
#define EPOLLOUT 0x004
#define EPOLLERR 0x008
#define EPOLLHUP 0x010
#define EPOLLRDHUP 0x2000
#define EPOLLONESHOT (1u << 30)
#define EPOLLET (1u << 31)
#define EPOLL_CTL_ADD 1
#define EPOLL_CTL_DEL 2
#define EPOLL_CTL_MOD 3
#define EPOLL_CLOEXEC O_CLOEXEC

struct epoll_event {
    u32 events;
    u64 data;
} __attribute__((packed));

#define EFD_SEMAPHORE 1
#define EFD_CLOEXEC O_CLOEXEC
#define EFD_NONBLOCK O_NONBLOCK

#define SFD_CLOEXEC O_CLOEXEC
#define SFD_NONBLOCK O_NONBLOCK

#define TFD_CLOEXEC O_CLOEXEC
#define TFD_NONBLOCK O_NONBLOCK
#define TFD_TIMER_ABSTIME 1

#define MFD_CLOEXEC 1
#define MFD_ALLOW_SEALING 2

/* ---- time --------------------------------------------------------------- */
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#define CLOCK_PROCESS_CPUTIME_ID 2
#define CLOCK_THREAD_CPUTIME_ID 3
#define CLOCK_BOOTTIME 7

struct timespec {
    i64 tv_sec;
    i64 tv_nsec;
};

struct itimerspec {
    struct timespec it_interval;
    struct timespec it_value;
};

/* ---- sockets ------------------------------------------------------------ */
#define AF_UNIX 1
#define PF_UNIX AF_UNIX
#define SOCK_STREAM 1
#define SOCK_DGRAM 2
#define SOCK_SEQPACKET 5
#define SOCK_NONBLOCK O_NONBLOCK
#define SOCK_CLOEXEC O_CLOEXEC
#define SOL_SOCKET 1
#define SO_TYPE 3
#define SO_ERROR 4
#define SO_SNDBUF 7
#define SO_RCVBUF 8
#define SO_PASSCRED 16
#define SO_PEERCRED 17
#define SO_ACCEPTCONN 30
#define SO_PROTOCOL 38
#define SO_DOMAIN 39
#define SCM_RIGHTS 1
#define SCM_CREDENTIALS 2
#define MSG_PEEK 2
#define MSG_TRUNC 0x20
#define MSG_DONTWAIT 0x40
#define MSG_EOR 0x80
#define MSG_WAITALL 0x100
#define MSG_NOSIGNAL 0x4000
#define MSG_CMSG_CLOEXEC 0x40000000
#define SHUT_RD 0
#define SHUT_WR 1
#define SHUT_RDWR 2

struct sockaddr_un {
    u16 sun_family;
    char sun_path[108];
};

struct ucred {
    pid_t pid;
    uid_t uid;
    gid_t gid;
};

struct iovec {
    void *iov_base;
    u64 iov_len;
};

struct msghdr {
    void *msg_name;
    socklen_t msg_namelen;
    u32 __pad;
    struct iovec *msg_iov;
    u64 msg_iovlen;
    void *msg_control;
    u64 msg_controllen;
    i32 msg_flags;
    u32 __pad2;
};

struct cmsghdr {
    u64 cmsg_len;
    i32 cmsg_level;
    i32 cmsg_type;
};

#define CMSG_ALIGN(len) (((len) + 7) & ~7UL)
#define CMSG_SPACE(len) (CMSG_ALIGN(sizeof(struct cmsghdr)) + CMSG_ALIGN(len))
#define CMSG_LEN(len) (CMSG_ALIGN(sizeof(struct cmsghdr)) + (len))
#define CMSG_DATA(cmsg) ((unsigned char *)(cmsg) + CMSG_ALIGN(sizeof(struct cmsghdr)))
#define CMSG_FIRSTHDR(msg) ((msg)->msg_controllen >= sizeof(struct cmsghdr) ? (struct cmsghdr *)(msg)->msg_control : (struct cmsghdr *)0)

/* ---- raw syscalls and libc API ------------------------------------------ */
long __syscall6(long number, long a, long b, long c, long d, long e, long f);
long __syscall_ret(long result);

ssize_t read(int fd, void *buf, size_t count);
ssize_t write(int fd, const void *buf, size_t count);
ssize_t pread(int fd, void *buf, size_t count, off_t offset);
ssize_t pwrite(int fd, const void *buf, size_t count, off_t offset);
int open(const char *path, int flags, ...);
int openat(int dirfd, const char *path, int flags, ...);
int close(int fd);
off_t lseek(int fd, off_t offset, int whence);
int ftruncate(int fd, off_t length);
int stat(const char *path, struct stat *st);
int fstat(int fd, struct stat *st);
int mkdir(const char *path, mode_t mode);
int mknod(const char *path, mode_t mode, dev_t dev);
int unlink(const char *path);
int rmdir(const char *path);
int chdir(const char *path);
char *getcwd(char *buf, size_t size);
int dup(int fd);
int dup2(int oldfd, int newfd);
int pipe(int fds[2]);
int pipe2(int fds[2], int flags);
int access(const char *path, int mode);
int ioctl(int fd, unsigned long request, void *arg);
int fcntl(int fd, int cmd, long arg);

void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset);
int munmap(void *addr, size_t length);
int mprotect(void *addr, size_t length, int prot);

int poll(struct pollfd *fds, unsigned long nfds, int timeout);
int epoll_create1(int flags);
int epoll_ctl(int epfd, int op, int fd, struct epoll_event *event);
int epoll_wait(int epfd, struct epoll_event *events, int maxevents, int timeout);

int eventfd(unsigned int initval, int flags);
int signalfd(int fd, const sigset_t *mask, size_t size);
int timerfd_create(int clockid, int flags);
int timerfd_settime(int fd, int flags, const struct itimerspec *new_value,
                    struct itimerspec *old_value);
int memfd_create(const char *name, unsigned int flags);

int init_module(const void *image, unsigned long length, const char *parameters);
int finit_module(int fd, const char *parameters, int flags);
int delete_module(const char *name, int flags);

pid_t fork(void);
pid_t getpid(void);
pid_t getppid(void);
pid_t gettid(void);
pid_t waitpid(pid_t pid, int *status, int options);
pid_t wait(int *status);
int waitid(int idtype, int id, struct siginfo *info, int options, void *rusage);
int execve(const char *path, char *const argv[], char *const envp[]);
int execvp(const char *file, char *const argv[]);
void _exit(int status) __attribute__((noreturn));
void exit(int status) __attribute__((noreturn));

typedef void (*sighandler_t)(int);
sighandler_t signal(int sig, sighandler_t handler);
int sigaction(int sig, const struct sigaction *act, struct sigaction *old);
int sigprocmask(int how, const sigset_t *set, sigset_t *old);
int sigpending(sigset_t *set);
int sigsuspend(const sigset_t *mask);
int sigaltstack(const stack_t *ss, stack_t *old);
int kill(pid_t pid, int sig);
int pause(void);

pid_t setsid(void);
int setpgid(pid_t pid, pid_t pgid);
pid_t getpgid(pid_t pid);
pid_t getpgrp(void);
pid_t getsid(pid_t pid);
pid_t tcgetpgrp(int fd);
int tcsetpgrp(int fd, pid_t pgrp);

int socket(int domain, int type, int protocol);
int socketpair(int domain, int type, int protocol, int fds[2]);
int bind(int fd, const struct sockaddr_un *addr, socklen_t len);
int listen(int fd, int backlog);
int connect(int fd, const struct sockaddr_un *addr, socklen_t len);
int accept(int fd, struct sockaddr_un *addr, socklen_t *len);
int accept4(int fd, struct sockaddr_un *addr, socklen_t *len, int flags);
int send(int fd, const void *buf, size_t len, int flags);
int recv(int fd, void *buf, size_t len, int flags);
int sendto(int fd, const void *buf, size_t len, int flags,
           const struct sockaddr_un *addr, socklen_t addrlen);
int recvfrom(int fd, void *buf, size_t len, int flags,
             struct sockaddr_un *addr, socklen_t *addrlen);
int sendmsg(int fd, const struct msghdr *msg, int flags);
int recvmsg(int fd, struct msghdr *msg, int flags);
int shutdown(int fd, int how);
int getsockopt(int fd, int level, int name, void *value, socklen_t *len);
int setsockopt(int fd, int level, int name, const void *value, socklen_t len);
int getsockname(int fd, struct sockaddr_un *addr, socklen_t *len);
int getpeername(int fd, struct sockaddr_un *addr, socklen_t *len);

int sched_yield(void);
int clock_gettime(int clockid, struct timespec *ts);
int nanosleep(const struct timespec *request, struct timespec *remaining);
unsigned int sleep(unsigned int seconds);
int usleep(unsigned int microseconds);

/* ---- memory and strings ------------------------------------------------- */
void *malloc(size_t size);
void *calloc(size_t count, size_t size);
void *realloc(void *pointer, size_t size);
void free(void *pointer);

void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n);
void *memset(void *dst, int value, size_t n);
int memcmp(const void *a, const void *b, size_t n);
size_t strlen(const char *s);
int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, size_t n);
char *strcpy(char *dst, const char *src);
char *strncpy(char *dst, const char *src, size_t n);
char *strcat(char *dst, const char *src);
char *strchr(const char *s, int c);
char *strrchr(const char *s, int c);
char *strstr(const char *haystack, const char *needle);
char *strdup(const char *s);
char *strerror(int error);
long strtol(const char *s, char **end, int base);
int atoi(const char *s);

extern char **environ;
char *getenv(const char *name);
int setenv(const char *name, const char *value, int overwrite);
int unsetenv(const char *name);

/* ---- stdio -------------------------------------------------------------- */
int printf(const char *format, ...);
int fprintf(int fd, const char *format, ...);
int dprintf(int fd, const char *format, ...);
int snprintf(char *buffer, size_t size, const char *format, ...);
int vsnprintf(char *buffer, size_t size, const char *format, __builtin_va_list arguments);
int puts(const char *s);
int fputs(const char *s, int fd);
int putchar(int c);
int isatty(int fd);

#endif
