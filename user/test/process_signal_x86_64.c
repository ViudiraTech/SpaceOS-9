/*
 *       user/test/process_signal_x86_64.c
 *       Threads, signals, process groups and cross-process kill
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "include/test_syscall.h"

extern volatile i64 thread_tid;
extern volatile i64 thread_ran;
void thread_entry(void);

static volatile i64 handler_sig;
static volatile i64 handler_count;
volatile i32 futex_gate;
volatile i32 thread_waiting;
volatile i32 thread_done;
volatile i32 thread_loop;
static volatile i32 futex_never;
static volatile i32 chld_seen;
static volatile i32 chld_signal;
static volatile i32 chld_code;
static volatile i32 chld_pid;
static volatile i32 chld_status;
static char thread_stack[65536] __attribute__((aligned(16)));

static u64 string_length(const char *text) {
    u64 length = 0;
    while (text[length] != 0) {
        length++;
    }
    return length;
}

static void fail(const char *why) {
    sys_write(1, why, string_length(why));
    sys_exit_group(1);
}

static void handler(int sig, void *info, void *context) {
    (void)info;
    (void)context;
    handler_sig = sig;
    handler_count++;
}

static void terminate_handler(int sig, void *info, void *context) {
    (void)sig;
    (void)info;
    (void)context;
    sys_exit(7);
}

struct kernel_siginfo {
    i32 signo;
    i32 error;
    i32 code;
    i32 padding;
    i32 pid;
    u32 uid;
    i32 status;
};

static void child_handler(int sig, void *info, void *context) {
    (void)context;
    struct kernel_siginfo *child = (struct kernel_siginfo *)info;
    chld_signal = sig;
    chld_code = child->code;
    chld_pid = child->pid;
    chld_status = child->status;
    chld_seen = 1;
}

struct kernel_sigaction {
    void *handler;
    u64 flags;
    void *restorer;
    u64 mask;
};

static void install_handler(int sig, void (*fn)(int, void *, void *)) {
    struct kernel_sigaction action;
    action.handler = (void *)fn;
    action.flags = SA_SIGINFO | SA_RESTORER;
    action.restorer = (void *)signal_restorer;
    action.mask = 0;
    if (sys_rt_sigaction(sig, &action, 0, 8) != 0) {
        fail("E:action\n");
    }
}

static void wait_frames(int count) {
    for (int i = 0; i < count; i++) {
        sys_sched_yield();
    }
}

static void register_handler(int sig, void *info, void *context) {
    (void)sig;
    (void)info;
    /* Linux x86_64 ucontext: mcontext starts at byte 40; gregs indices
     * RCX=14 and R11=3. Signal return must preserve these independently
     * of the saved RIP/RFLAGS, even when returning through a syscall. */
    u64 *gregs = (u64 *)((char *)context + 40);
    gregs[14] = 0x12345678;
    gregs[3] = 0x76543210;
}

static void test_sigreturn_registers(i64 pid) {
    install_handler(SIGUSR1, register_handler);
    register u64 r11 __asm__("r11");
    u64 rcx;
    i64 result;
    __asm__ volatile("syscall" : "=a"(result), "=c"(rcx), "=r"(r11)
                     : "a"(SYS_KILL), "D"(pid), "S"(SIGUSR1)
                     : "memory");
    if (result != 0 || rcx != 0x12345678 || r11 != 0x76543210) {
        fail("E:sigreturn-registers\n");
    }
}

static volatile u64 handler_frame;

static void frame_handler(int sig, void *info, void *context) {
    (void)sig;
    (void)info;
    (void)context;
    handler_frame = (u64)__builtin_frame_address(0);
}

/* A signal may interrupt a leaf function that keeps locals in the 128 bytes
 * below RSP, and it must enter its handler with the call-time stack alignment. */
static void test_signal_stack_layout(i64 pid) {
    install_handler(SIGUSR1, frame_handler);
    u64 damaged;
    i64 result;
    register u64 cursor __asm__("r8");
    __asm__ volatile("lea -128(%%rsp), %%r8\n"
                     "1: mov %%r8, (%%r8)\n"
                     "add $8, %%r8\n"
                     "cmp %%rsp, %%r8\n"
                     "jb 1b\n"
                     "syscall\n"
                     "mov %%rax, %%rdi\n"
                     "xor %%edx, %%edx\n"
                     "lea -128(%%rsp), %%r8\n"
                     "2: cmp %%r8, (%%r8)\n"
                     "je 3f\n"
                     "inc %%rdx\n"
                     "3: add $8, %%r8\n"
                     "cmp %%rsp, %%r8\n"
                     "jb 2b\n"
                     : "=D"(result), "=d"(damaged), "=r"(cursor)
                     : "a"(SYS_KILL), "D"(pid), "S"(SIGUSR1)
                     : "rcx", "r11", "memory", "cc");
    if (result != 0 || handler_frame == 0) {
        fail("E:signal-layout-run\n");
    }
    if (damaged != 0) {
        fail("E:red-zone\n");
    }
    /* A correctly aligned entry leaves RBP on a 16-byte boundary after push. */
    if (handler_frame % 16 != 0) {
        fail("E:handler-alignment\n");
    }
}

int main(void) {
    i64 me = sys_getpid();
    if (me <= 0 || sys_gettid() != me) {
        fail("E:ident\n");
    }
    if (sys_getpgid(0) != me || sys_getsid(0) != me) {
        fail("E:session\n");
    }

    test_sigreturn_registers(me);
    test_signal_stack_layout(me);
    install_handler(SIGUSR1, handler);
    install_handler(SIGUSR2, handler);
    install_handler(SIGTERM, terminate_handler);

    if (sys_kill(me, SIGUSR1) != 0) {
        fail("E:kill1\n");
    }
    if (handler_sig != SIGUSR1 || handler_count != 1) {
        fail("E:handler1\n");
    }

    u64 blocked = 1ul << (SIGUSR2 - 1);
    if (sys_rt_sigprocmask(SIG_BLOCK, &blocked, 0, 8) != 0) {
        fail("E:block\n");
    }
    if (sys_kill(me, SIGUSR2) != 0) {
        fail("E:kill2\n");
    }
    u64 pending = 0;
    if (sys_rt_sigpending(&pending, 8) != 0) {
        fail("E:pending\n");
    }
    if ((pending & blocked) == 0) {
        fail("E:notpending\n");
    }
    if (handler_sig == SIGUSR2) {
        fail("E:blockedran\n");
    }
    if (sys_rt_sigprocmask(SIG_UNBLOCK, &blocked, 0, 8) != 0) {
        fail("E:unblock\n");
    }
    if (handler_sig != SIGUSR2 || handler_count != 2) {
        fail("E:handler2\n");
    }

    /* A CLONE_THREAD child shares memory; CHILD_SETTID publishes its TID and
     * CHILD_CLEARTID clears the word when the thread exits. */
    volatile i32 child_tid = -1;
    u64 flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD |
                CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID;
    i64 tid = clone_thread(flags, thread_stack + sizeof(thread_stack), &child_tid);
    if (tid <= 0) {
        fail("E:clone\n");
    }
    for (long i = 0; i < 200000 && thread_ran == 0; i++) {
        sys_sched_yield();
    }
    if (thread_ran == 0) {
        fail("E:nothread\n");
    }
    if (thread_tid <= 0 || thread_tid != tid || thread_tid == me) {
        fail("E:threadtid\n");
    }
    if (child_tid != (i32)tid) {
        fail("E:settid\n");
    }
    /* FUTEX_WAIT timeout with no waker. */
    {
        struct {
            i64 seconds;
            i64 nanos;
        } timeout = {0, 1000000};
        i64 rc = sys_futex((u64)&futex_never, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0,
                           (u64)&timeout, 0, 0);
        if (rc != -ETIMEDOUT) {
            fail("E:futex_timeout\n");
        }
    }
    /* Wait until the child publishes its futex gate, then wake it. */
    for (long i = 0; i < 200000 && thread_waiting == 0; i++) {
        sys_sched_yield();
    }
    if (thread_waiting == 0) {
        fail("E:futex_gate\n");
    }
    futex_gate = 1;
    if (sys_futex((u64)&futex_gate, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, 0, 0, 0) != 1) {
        fail("E:futex_wake\n");
    }
    for (long i = 0; i < 200000 && thread_done == 0; i++) {
        sys_sched_yield();
    }
    if (thread_done == 0) {
        fail("E:futex_done\n");
    }
    /* pthread_join style: CLONE_CHILD_CLEARTID publishes zero and wakes. */
    for (int tries = 0; tries < 100 && child_tid != 0; tries++) {
        struct {
            i64 seconds;
            i64 nanos;
        } timeout = {0, 100000000};
        i64 rc = sys_futex((u64)&child_tid, FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
                           (u32)child_tid, (u64)&timeout, 0, 0);
        if (rc == 0 || rc == -11 || rc == -4 || rc == -ETIMEDOUT) {
            continue;
        }
        fail("E:futex_join\n");
    }
    if (child_tid != 0) {
        fail("E:cleartid\n");
    }

    /* Child installs a SIGTERM handler; kill must run it and wait4 must report
     * its exit code. */
    i64 child = sys_fork();
    if (child == 0) {
        install_handler(SIGTERM, terminate_handler);
        if (sys_setpgid(0, 0) != 0) {
            fail("E:childpgid\n");
        }
        if (sys_getpgid(0) != sys_getpid()) {
            fail("E:childpgid2\n");
        }
        for (;;) {
            __asm__ volatile("pause");
        }
    }
    if (child < 0) {
        fail("E:fork\n");
    }
    wait_frames(20000);
    if (sys_kill(child, SIGTERM) != 0) {
        fail("E:killchild\n");
    }
    i64 status = 0;
    i64 reaped = sys_wait4(child, &status, 0, 0);
    if (reaped != child) {
        fail("E:wait1\n");
    }
    if (((status >> 8) & 0xff) != 7) {
        fail("E:status1\n");
    }

    /* A SIGKILL death is reported as a signaled wait status. */
    child = sys_fork();
    if (child == 0) {
        for (;;) {
            __asm__ volatile("pause");
        }
    }
    if (child < 0) {
        fail("E:fork2\n");
    }
    wait_frames(20000);
    if (sys_kill(child, 9) != 0) {
        fail("E:kill2child\n");
    }
    status = 0;
    reaped = sys_wait4(child, &status, 0, 0);
    if (reaped != child) {
        fail("E:wait2\n");
    }
    if ((status & 0x7f) != 9) {
        fail("E:status2\n");
    }

    /* Job control: SIGSTOP parks the child, WUNTRACED reports it, SIGCONT
     * resumes it and WCONTINUED reports the transition. */
    child = sys_fork();
    if (child == 0) {
        for (;;) {
            __asm__ volatile("pause");
        }
    }
    if (child < 0) {
        fail("E:fork3\n");
    }
    wait_frames(20000);
    if (sys_kill(child, SIGSTOP) != 0) {
        fail("E:stopkill\n");
    }
    status = 0;
    reaped = sys_wait4(child, &status, WUNTRACED, 0);
    if (reaped != child || ((status >> 8) & 0xff) != SIGSTOP || (status & 0xff) != 0x7f) {
        fail("E:stopwait\n");
    }
    if (sys_kill(child, SIGCONT) != 0) {
        fail("E:contkill\n");
    }
    status = 0;
    reaped = sys_wait4(child, &status, WCONTINUED, 0);
    if (reaped != child || (status & 0xffff) != 0xffff) {
        fail("E:contwait\n");
    }
    if (sys_kill(child, 9) != 0) {
        fail("E:stopkill2\n");
    }
    if (sys_wait4(child, &status, 0, 0) != child) {
        fail("E:stopreap\n");
    }

    /* Controlling terminal: a session leader's open claims the console and
     * sets the foreground pgrp, then the ioctls read it back. */
    i64 console = sys_openat(AT_FDCWD, "/dev/console", O_RDWR, 0);
    if (console < 0) {
        fail("E:consoleopen\n");
    }
    i32 pgrp = (i32)me;
    if (sys_ioctl(console, TIOCSPGRP, (u64)&pgrp) != 0) {
        fail("E:spgrp\n");
    }
    pgrp = 0;
    if (sys_ioctl(console, TIOCGPGRP, (u64)&pgrp) != 0 || pgrp != (i32)me) {
        fail("E:gpgrp\n");
    }
    u32 session = 0;
    if (sys_ioctl(console, TIOCGSID, (u64)&session) != 0 || session != (u32)me) {
        fail("E:gsid\n");
    }
    struct kernel_termios {
        u32 iflag;
        u32 oflag;
        u32 cflag;
        u32 lflag;
        unsigned char line;
        unsigned char cc[19];
    } __attribute__((packed));
    struct kernel_termios settings;
    if (sys_ioctl(console, TCGETS, (u64)&settings) != 0) {
        fail("E:tcgets\n");
    }
    if ((settings.lflag & (ICANON | ECHO)) != (ICANON | ECHO)
        || (settings.iflag & ICRNL) == 0) {
        fail("E:termios_default\n");
    }
    struct kernel_termios saved = settings;
    settings.lflag &= ~(ICANON | ECHO);
    if (sys_ioctl(console, TCSETS, (u64)&settings) != 0) {
        fail("E:tsets\n");
    }
    if (sys_ioctl(console, TCGETS, (u64)&settings) != 0
        || (settings.lflag & (ICANON | ECHO)) != 0) {
        fail("E:termios_roundtrip\n");
    }
    if (sys_ioctl(console, TCSETS, (u64)&saved) != 0) {
        fail("E:tsets_restore\n");
    }
    if (sys_close(console) != 0) {
        fail("E:consoleclose\n");
    }

    /* A background process group reading the console gets SIGTTIN. */
    child = sys_fork();
    if (child == 0) {
        if (sys_setpgid(0, 0) != 0) {
            fail("E:bgpgid\n");
        }
        char byte;
        sys_read(0, &byte, 1);
        for (;;) {
            __asm__ volatile("pause");
        }
    }
    if (child < 0) {
        fail("E:fork4\n");
    }
    wait_frames(20000);
    status = 0;
    reaped = sys_wait4(child, &status, WUNTRACED, 0);
    if (reaped != child || ((status >> 8) & 0xff) != SIGTTIN) {
        fail("E:ttin\n");
    }
    if (sys_kill(child, SIGCONT) != 0) {
        fail("E:ttincont\n");
    }
    wait_frames(2000);
    if (sys_kill(child, 9) != 0) {
        fail("E:ttinkill\n");
    }
    if (sys_wait4(child, &status, 0, 0) != child) {
        fail("E:ttinreap\n");
    }

    /* execve keeps the PID and resets handled dispositions.  The child
     * installs SIGTERM (inherited handler exits 7) and then replaces itself
     * with a target that loops; default SIGTERM death must report signal 15. */
    child = sys_fork();
    if (child == 0) {
        volatile i32 exec_ctid = -1;
        thread_loop = 1;
        thread_ran = 0;
        u64 sibling_flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND |
                            CLONE_THREAD | CLONE_CHILD_CLEARTID;
        clone_thread(sibling_flags, thread_stack + sizeof(thread_stack), &exec_ctid);
        char *argv[] = {(char *)"exec_target", 0};
        char *envp[] = {0};
        sys_execve("/tmp/exec_target", argv, envp);
        sys_exit(1);
    }
    if (child < 0) {
        fail("E:fork5\n");
    }
    wait_frames(50000);
    if (sys_kill(child, SIGTERM) != 0) {
        fail("E:execkill\n");
    }
    status = 0;
    reaped = sys_wait4(child, &status, 0, 0);
    if (reaped != child) {
        fail("E:execwait\n");
    }
    if ((status & 0x7f) != SIGTERM) {
        fail("E:execsig\n");
    }

    /* waitid reports exit details without reaping under WNOWAIT. */
    child = sys_fork();
    if (child == 0) {
        sys_exit(9);
    }
    if (child < 0) {
        fail("E:fork6\n");
    }
    struct kernel_siginfo wait_info;
    for (int tries = 0; tries < 200000; tries++) {
        for (u64 i = 0; i < sizeof(wait_info); i++) {
            ((volatile unsigned char *)&wait_info)[i] = 0;
        }
        i64 rc = sys_waitid(P_PID, child, &wait_info, WEXITED | WNOWAIT, 0);
        if (rc == 0) {
            break;
        }
        if (rc != -10) {
            fail("E:waitid1\n");
        }
        sys_sched_yield();
    }
    if (wait_info.signo != SIGCHLD || wait_info.code != CLD_EXITED
        || wait_info.pid != (i32)child || wait_info.status != 9) {
        fail("E:waitidinfo\n");
    }
    /* A second WNOWAIT query must still see the zombie. */
    if (sys_waitid(P_PID, child, &wait_info, WEXITED | WNOWAIT | 1, 0) != 0) {
        fail("E:waitid2\n");
    }
    status = 0;
    if (sys_wait4(child, &status, 0, 0) != child) {
        fail("E:waitidreap\n");
    }

    /* SIGCHLD siginfo carries CLD_EXITED and the child's exit status. */
    install_handler(SIGCHLD, child_handler);
    chld_seen = 0;
    child = sys_fork();
    if (child == 0) {
        sys_exit(7);
    }
    status = 0;
    if (sys_wait4(child, &status, 0, 0) != child) {
        fail("E:chldwait\n");
    }
    for (int i = 0; i < 200000 && chld_seen == 0; i++) {
        sys_sched_yield();
    }
    if (chld_seen == 0 || chld_signal != SIGCHLD || chld_code != CLD_EXITED
        || chld_pid != (i32)child || chld_status != 7) {
        fail("E:chldinfo\n");
    }

    /* A non-COW user fault is reported to wait4 as SIGSEGV death. */
    child = sys_fork();
    if (child == 0) {
        *(volatile int *)0 = 1;
        sys_exit(1);
    }
    status = 0;
    if (sys_wait4(child, &status, 0, 0) != child) {
        fail("E:faultwait\n");
    }
    if ((status & 0x7f) != 11) {
        fail("E:faultstatus\n");
    }

    const char ok[] = "process/signal ok\n";
    sys_write(1, ok, sizeof(ok) - 1);
    sys_exit_group(43);
}
