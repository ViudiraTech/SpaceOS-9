/*
 *
 *       user/bench/spbench.c
 *       Linux-ABI micro/macro benchmark for SpaceOS-9
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

/*
 * Every test runs for a fixed wall-clock window and reports a rate, so the
 * same binary is meaningful under TCG, KVM and on hardware.  Output lines are
 * machine readable:  BENCH <name> <value> <unit>
 * The binary also runs unmodified on Linux, which gives a reference column.
 *
 * Usage: spbench [-t seconds] [-f filter] [-p]   (-p: power off when done)
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#ifndef FUTEX_WAIT_PRIVATE
#define FUTEX_WAIT_PRIVATE 128
#define FUTEX_WAKE_PRIVATE 129
#endif
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/reboot.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static double window = 1.0;
static const char *filter;
static int ncpu = 1;

static double now(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (double)ts.tv_sec + (double)ts.tv_nsec * 1e-9;
}

static void report(const char *name, double value, const char *unit)
{
	printf("BENCH %-28s %14.3f %s\n", name, value, unit);
	fflush(stdout);
}

static void pin(int cpu)
{
	cpu_set_t set;
	CPU_ZERO(&set);
	CPU_SET(cpu % ncpu, &set);
	sched_setaffinity(0, sizeof(set), &set);
}

static void unpin(void)
{
	cpu_set_t set;
	CPU_ZERO(&set);
	for (int i = 0; i < ncpu; i++)
		CPU_SET(i, &set);
	sched_setaffinity(0, sizeof(set), &set);
}

static void die(const char *what)
{
	printf("BENCH_ERROR %s errno=%d\n", what, errno);
	fflush(stdout);
	exit(1);
}

/* Run body() in batches until the window elapses; return ops/second. */
#define TIMED_LOOP(batch, body)                                     \
	({                                                          \
		double start_ = now(), end_ = start_ + window, t_;  \
		unsigned long ops_ = 0;                             \
		do {                                                \
			for (int i_ = 0; i_ < (batch); i_++) {      \
				body;                               \
			}                                           \
			ops_ += (batch);                            \
		} while ((t_ = now()) < end_);                      \
		(double)ops_ / (t_ - start_);                       \
	})

/* ------------------------------------------------------------------ */

static void bench_null_syscall(void)
{
	double r = TIMED_LOOP(256, syscall(SYS_getppid));
	report("syscall_getppid", 1e9 / r, "ns/op");
}

static void bench_clock(void)
{
	struct timespec ts;
	double r = TIMED_LOOP(256, clock_gettime(CLOCK_MONOTONIC, &ts));
	report("clock_gettime", 1e9 / r, "ns/op");
}

static void bench_yield(void)
{
	double r = TIMED_LOOP(64, sched_yield());
	report("sched_yield_solo", 1e9 / r, "ns/op");
}

/* Two processes ping-pong one byte through a pair of pipes. Each round trip
 * is two wakeups and two context switches. */
static void pipe_pingpong(const char *name, int cpu_a, int cpu_b)
{
	int ab[2], ba[2];
	if (pipe(ab) || pipe(ba))
		die("pipe");
	pid_t child = fork();
	if (child < 0)
		die("fork");
	char c = 0;
	if (child == 0) {
		if (cpu_b >= 0)
			pin(cpu_b);
		close(ab[1]);
		close(ba[0]);
		while (read(ab[0], &c, 1) == 1) {
			if (write(ba[1], &c, 1) != 1)
				break;
		}
		_exit(0);
	}
	if (cpu_a >= 0)
		pin(cpu_a);
	close(ab[0]);
	close(ba[1]);
	double r = TIMED_LOOP(32, {
		if (write(ab[1], &c, 1) != 1 || read(ba[0], &c, 1) != 1)
			die("pingpong");
	});
	close(ab[1]);
	close(ba[0]);
	waitpid(child, NULL, 0);
	unpin();
	report(name, 1e9 / r / 2.0, "ns/switch");
}

static void bench_pipe_pingpong(void)
{
	pipe_pingpong("pipe_pingpong_same_cpu", 0, 0);
	if (ncpu > 1)
		pipe_pingpong("pipe_pingpong_cross_cpu", 0, 1);
	pipe_pingpong("pipe_pingpong_unpinned", -1, -1);
}

static void bench_unix_pingpong(void)
{
	int sv[2];
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv))
		die("socketpair");
	pid_t child = fork();
	char c = 0;
	if (child == 0) {
		close(sv[0]);
		while (read(sv[1], &c, 1) == 1)
			if (write(sv[1], &c, 1) != 1)
				break;
		_exit(0);
	}
	close(sv[1]);
	double r = TIMED_LOOP(32, {
		if (write(sv[0], &c, 1) != 1 || read(sv[0], &c, 1) != 1)
			die("unix pingpong");
	});
	close(sv[0]);
	waitpid(child, NULL, 0);
	report("unix_stream_pingpong", 1e9 / r / 2.0, "ns/switch");
}

/* Futex ping-pong between two threads of one process. */
static _Atomic int fx_word;
static _Atomic int fx_stop;

static long futex(void *addr, int op, int val)
{
	return syscall(SYS_futex, addr, op, val, NULL, NULL, 0);
}

static void *futex_partner(void *arg)
{
	(void)arg;
	for (;;) {
		while (atomic_load(&fx_word) != 1) {
			if (atomic_load(&fx_stop))
				return NULL;
			futex(&fx_word, FUTEX_WAIT_PRIVATE, 0);
		}
		atomic_store(&fx_word, 0);
		futex(&fx_word, FUTEX_WAKE_PRIVATE, 1);
	}
}

static void bench_futex_pingpong(void)
{
	pthread_t thread;
	atomic_store(&fx_word, 0);
	atomic_store(&fx_stop, 0);
	if (pthread_create(&thread, NULL, futex_partner, NULL))
		die("pthread_create");
	double r = TIMED_LOOP(32, {
		atomic_store(&fx_word, 1);
		futex(&fx_word, FUTEX_WAKE_PRIVATE, 1);
		while (atomic_load(&fx_word) != 0)
			futex(&fx_word, FUTEX_WAIT_PRIVATE, 1);
	});
	atomic_store(&fx_stop, 1);
	atomic_store(&fx_word, 2);
	futex(&fx_word, FUTEX_WAKE_PRIVATE, 1);
	pthread_join(thread, NULL);
	report("futex_pingpong", 1e9 / r / 2.0, "ns/switch");
}

static void bench_fork_exit(void)
{
	double r = TIMED_LOOP(4, {
		pid_t pid = fork();
		if (pid == 0)
			_exit(0);
		if (pid < 0)
			die("fork");
		waitpid(pid, NULL, 0);
	});
	report("fork_exit_wait", 1e6 / r, "us/op");
}

static void bench_fork_exec(const char *self)
{
	char *argv[] = {(char *)self, "-x", NULL};
	char *envp[] = {NULL};
	double r = TIMED_LOOP(2, {
		pid_t pid = fork();
		if (pid == 0) {
			execve(self, argv, envp);
			_exit(127);
		}
		if (pid < 0)
			die("fork");
		int status;
		waitpid(pid, &status, 0);
		if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
			die("exec child");
	});
	report("fork_exec_wait", 1e6 / r, "us/op");
}

static void *thread_nop(void *arg)
{
	return arg;
}

static void bench_thread_create(void)
{
	double r = TIMED_LOOP(4, {
		pthread_t t;
		if (pthread_create(&t, NULL, thread_nop, NULL))
			die("pthread_create");
		pthread_join(t, NULL);
	});
	report("thread_create_join", 1e6 / r, "us/op");
}

static void bench_mmap(void)
{
	double r = TIMED_LOOP(64, {
		void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
			       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (p == MAP_FAILED)
			die("mmap");
		munmap(p, 4096);
	});
	report("mmap_munmap_4k", 1e9 / r, "ns/op");
}

static void bench_page_fault(void)
{
	const size_t len = 16u << 20;
	double start = now(), end = start + window, t;
	unsigned long pages = 0;
	do {
		volatile char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
					MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (p == MAP_FAILED)
			die("mmap");
		for (size_t off = 0; off < len; off += 4096)
			p[off] = 1;
		munmap((void *)p, len);
		pages += len / 4096;
	} while ((t = now()) < end);
	report("anon_page_fault", 1e9 * (t - start) / (double)pages, "ns/page");
}

static void bench_cow_fault(void)
{
	const size_t len = 4u << 20;
	char *p = mmap(NULL, len, PROT_READ | PROT_WRITE,
		       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (p == MAP_FAILED)
		die("mmap");
	memset(p, 1, len);
	int fds[2];
	if (pipe(fds))
		die("pipe");
	double start = now(), end = start + window, t;
	unsigned long pages = 0;
	do {
		pid_t pid = fork();
		if (pid == 0) {
			double s = now();
			for (size_t off = 0; off < len; off += 4096)
				p[off] = 2;
			double e = now() - s;
			if (write(fds[1], &e, sizeof(e)) != sizeof(e))
				_exit(1);
			_exit(0);
		}
		double e;
		if (read(fds[0], &e, sizeof(e)) != sizeof(e))
			die("cow read");
		waitpid(pid, NULL, 0);
		pages += len / 4096;
		(void)e;
	} while ((t = now()) < end);
	munmap(p, len);
	close(fds[0]);
	close(fds[1]);
	report("fork_cow_4m", 1e6 * (t - start) / (double)(pages / (len / 4096)), "us/fork");
}

static void bench_pipe_bandwidth(void)
{
	int fds[2];
	static char buf[65536];
	if (pipe(fds))
		die("pipe");
	pid_t pid = fork();
	if (pid == 0) {
		close(fds[1]);
		while (read(fds[0], buf, sizeof(buf)) > 0)
			;
		_exit(0);
	}
	close(fds[0]);
	double start = now(), end = start + window, t;
	unsigned long bytes = 0;
	do {
		ssize_t n = write(fds[1], buf, sizeof(buf));
		if (n <= 0)
			die("pipe write");
		bytes += (unsigned long)n;
	} while ((t = now()) < end);
	close(fds[1]);
	waitpid(pid, NULL, 0);
	report("pipe_bandwidth_64k", (double)bytes / (t - start) / 1048576.0, "MiB/s");
}

static void bench_vfs(void)
{
	struct stat st;
	const char *path = "/etc/hostname";
	if (stat(path, &st))
		path = "/proc/self/stat";
	double r = TIMED_LOOP(32, stat(path, &st));
	report("stat", 1e9 / r, "ns/op");
	r = TIMED_LOOP(32, {
		int fd = open(path, O_RDONLY);
		if (fd < 0)
			die("open");
		close(fd);
	});
	report("open_close", 1e9 / r, "ns/op");
	int fd = open("/dev/zero", O_RDONLY);
	if (fd >= 0) {
		static char buf[4096];
		r = TIMED_LOOP(64, read(fd, buf, sizeof(buf)));
		report("read_dev_zero_4k", 1e9 / r, "ns/op");
		close(fd);
	}
	fd = open("/tmp/spbench.dat", O_RDWR | O_CREAT | O_TRUNC, 0644);
	if (fd >= 0) {
		static char buf[4096];
		double start = now(), end = start + window, t;
		unsigned long bytes = 0;
		do {
			if (pwrite(fd, buf, sizeof(buf), (off_t)(bytes % (8u << 20))) != sizeof(buf))
				die("tmpfs write");
			bytes += sizeof(buf);
		} while ((t = now()) < end);
		report("tmpfs_write_4k", (double)bytes / (t - start) / 1048576.0, "MiB/s");
		close(fd);
		unlink("/tmp/spbench.dat");
	}
}

/* Each CPU runs a private getppid loop; total rate exposes shared locks on
 * the syscall path. */
static void bench_syscall_scaling(void)
{
	if (ncpu < 2)
		return;
	int fds[2];
	if (pipe(fds))
		die("pipe");
	for (int cpu = 0; cpu < ncpu; cpu++) {
		if (fork() == 0) {
			pin(cpu);
			double r = TIMED_LOOP(256, syscall(SYS_getppid));
			if (write(fds[1], &r, sizeof(r)) != sizeof(r))
				_exit(1);
			_exit(0);
		}
	}
	double total = 0, r;
	for (int cpu = 0; cpu < ncpu; cpu++) {
		if (read(fds[0], &r, sizeof(r)) != sizeof(r))
			die("scaling read");
		total += r;
	}
	while (wait(NULL) > 0)
		;
	close(fds[0]);
	close(fds[1]);
	report("syscall_scaling_total", total / 1e6, "Mops/s");
	report("syscall_scaling_per_cpu", 1e9 / (total / ncpu), "ns/op");
}

/* Mini hackbench: G groups of N senders and N receivers exchanging fixed
 * messages over pipes; total completion time stresses wakeup placement and
 * load balancing. */
static void bench_hackbench(void)
{
	const int groups = 2, fan = 5, loops = 50, msg = 100;
	double start = now();
	for (int g = 0; g < groups; g++) {
		int fds[2 * 5];
		for (int r = 0; r < fan; r++)
			if (pipe(&fds[2 * r]))
				die("hb pipe");
		for (int r = 0; r < fan; r++) {
			if (fork() == 0) {
				char buf[100];
				for (int i = 0; i < fan; i++)
					close(fds[2 * i + 1]);
				long want = (long)fan * loops * msg, got = 0;
				while (got < want) {
					ssize_t n = read(fds[2 * r], buf, sizeof(buf));
					if (n <= 0)
						_exit(1);
					got += n;
				}
				_exit(0);
			}
		}
		for (int s = 0; s < fan; s++) {
			if (fork() == 0) {
				char buf[100];
				memset(buf, s, sizeof(buf));
				for (int i = 0; i < loops; i++)
					for (int r = 0; r < fan; r++)
						if (write(fds[2 * r + 1], buf, msg) != msg)
							_exit(1);
				_exit(0);
			}
		}
		for (int i = 0; i < 2 * fan; i++)
			close(fds[i]);
	}
	int status, failed = 0;
	while (wait(&status) > 0)
		if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
			failed++;
	if (failed)
		die("hackbench child failed");
	report("hackbench_2x5x50", (now() - start) * 1e3, "ms");
}

/* CPU-bound fairness: ncpu+1 spinning processes for one window; report the
 * spread of work done (max/min). 1.0 is perfectly fair. */
static void bench_fairness(void)
{
	int n = ncpu + 1;
	int fds[2];
	if (pipe(fds))
		die("pipe");
	double deadline = now() + window * 2;
	for (int i = 0; i < n; i++) {
		if (fork() == 0) {
			volatile unsigned long count = 0;
			while (now() < deadline)
				for (int k = 0; k < 4096; k++)
					count++;
			unsigned long c = count;
			if (write(fds[1], &c, sizeof(c)) != sizeof(c))
				_exit(1);
			_exit(0);
		}
	}
	unsigned long lo = ~0ul, hi = 0, sum = 0, c;
	for (int i = 0; i < n; i++) {
		if (read(fds[0], &c, sizeof(c)) != sizeof(c))
			die("fair read");
		lo = c < lo ? c : lo;
		hi = c > hi ? c : hi;
		sum += c;
	}
	while (wait(NULL) > 0)
		;
	close(fds[0]);
	close(fds[1]);
	report("cpu_fairness_max_over_min", lo ? (double)hi / (double)lo : 0.0, "ratio");
	report("cpu_spin_throughput", (double)sum / (window * 2) / 1e6, "Mloops/s");
}

/* Wakeup latency of a sleeper: nanosleep(1ms) overshoot. */
static void bench_sleep_latency(void)
{
	struct timespec req = {0, 1000000};
	double worst = 0, total = 0;
	int n = 0;
	double end = now() + window;
	while (now() < end) {
		double s = now();
		nanosleep(&req, NULL);
		double over = (now() - s) * 1e6 - 1000.0;
		total += over;
		worst = over > worst ? over : worst;
		n++;
	}
	report("nanosleep_1ms_overshoot_avg", total / n, "us");
	report("nanosleep_1ms_overshoot_max", worst, "us");
}

/* MemFree from /proc/meminfo in KiB, or -1. Reported before and after the
 * suite so a kernel leak shows up as a drop across runs. */
static long mem_free_kib(void)
{
	FILE *f = fopen("/proc/meminfo", "r");
	char line[128];
	long value = -1;
	if (!f)
		return -1;
	while (fgets(line, sizeof(line), f))
		if (sscanf(line, "MemFree: %ld", &value) == 1)
			break;
	fclose(f);
	return value;
}

struct test {
	const char *name;
	void (*fn)(void);
};

static const char *self_path;
static void bench_exec(void) { bench_fork_exec(self_path); }

static const struct test tests[] = {
	{"syscall", bench_null_syscall},
	{"clock", bench_clock},
	{"yield", bench_yield},
	{"pipe", bench_pipe_pingpong},
	{"unix", bench_unix_pingpong},
	{"futex", bench_futex_pingpong},
	{"fork", bench_fork_exit},
	{"exec", bench_exec},
	{"thread", bench_thread_create},
	{"mmap", bench_mmap},
	{"fault", bench_page_fault},
	{"cow", bench_cow_fault},
	{"pipebw", bench_pipe_bandwidth},
	{"vfs", bench_vfs},
	{"scaling", bench_syscall_scaling},
	{"hackbench", bench_hackbench},
	{"fairness", bench_fairness},
	{"sleep", bench_sleep_latency},
};

int main(int argc, char **argv)
{
	int poweroff = 0;
	self_path = argv[0];
	for (int i = 1; i < argc; i++) {
		if (!strcmp(argv[i], "-x"))
			return 0; /* exec target */
		if (!strcmp(argv[i], "-t") && i + 1 < argc)
			window = atof(argv[++i]);
		else if (!strcmp(argv[i], "-f") && i + 1 < argc)
			filter = argv[++i];
		else if (!strcmp(argv[i], "-p"))
			poweroff = 1;
	}
	/* As PID 1 there are no arguments: take "filter" and "window" from
	 * /etc/spbench.conf (one "key value" per line) when present. */
	if (!filter) {
		static char conf_filter[256];
		FILE *conf = fopen("/etc/spbench.conf", "r");
		if (conf) {
			char key[32], value[224];
			while (fscanf(conf, "%31s %223[^\n]", key, value) == 2) {
				if (!strcmp(key, "filter")) {
					snprintf(conf_filter, sizeof(conf_filter), "%s", value);
					filter = conf_filter;
				} else if (!strcmp(key, "window")) {
					window = atof(value);
				}
			}
			fclose(conf);
		}
	}
	/* Kernel command line arguments after init= are not passed through on
	 * every kernel, so also honour the environment. */
	if (getenv("SPBENCH_POWEROFF"))
		poweroff = 1;
	if (getpid() == 1)
		poweroff = 1;
	if (access(self_path, X_OK) != 0)
		self_path = "/bin/spbench";
	ncpu = (int)sysconf(_SC_NPROCESSORS_ONLN);
	if (ncpu < 1)
		ncpu = 1;
	setvbuf(stdout, NULL, _IOLBF, 0);
	printf("BENCH_START cpus=%d window=%.2fs\n", ncpu, window);
	long free_before = mem_free_kib();
	for (size_t i = 0; i < sizeof(tests) / sizeof(tests[0]); i++) {
		if (filter && !strstr(filter, tests[i].name))
			continue;
		tests[i].fn();
	}
	long free_after = mem_free_kib();
	if (free_before >= 0 && free_after >= 0) {
		report("mem_free_start", (double)free_before, "KiB");
		report("mem_free_end", (double)free_after, "KiB");
		report("mem_free_lost", (double)(free_before - free_after), "KiB");
	}
	printf("BENCH_DONE\n");
	fflush(stdout);
	if (poweroff) {
		sync();
		reboot(RB_POWER_OFF);
	}
	return 0;
}
