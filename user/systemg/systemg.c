/*
 *
 *       user/systemg/systemg.c
 *       Minimal systemd-compatible service manager running as PID 1
 *
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#include "splibc.h"
#include "journal.h"

#define MAX_UNITS 32
#define MAX_DEPS 8
#define SOCK_PATH "/run/systemg.sock"
#define JOURNAL_QUEUE_CAP 128
#define JOURNAL_MSG_MAX   320

enum unit_type { TYPE_SIMPLE, TYPE_EXEC, TYPE_ONESHOT };
enum restart_policy { RESTART_NO, RESTART_ON_FAILURE, RESTART_ALWAYS };

struct unit {
    char name[64];
    char description[96];
    char exec[256];
    char after[MAX_DEPS][64];
    int after_count;
    char requires[MAX_DEPS][64];
    int requires_count;
    char wants[MAX_DEPS][64];
    int wants_count;
    int type;
    int restart;
    int restart_sec;
    pid_t pid;
    int started;
    int failed;
    int pending_restart;
    int restarts;
    int ever_started;
    time_t started_at;
    int exit_status;
    u64 queue_ts;
    u64 start_ts;
    u64 active_ts;
    u64 duration_us;
};

static struct unit units[MAX_UNITS];
static int unit_count;
static volatile int child_changed;
static u64 kernel_finish_usec;
static u64 userspace_start_usec;
static u64 userspace_finish_usec;
static const char *g_sock_path = SOCK_PATH;

static void child_handler(int signal_number) {
    (void)signal_number;
    child_changed = 1;
}

static u64 get_monotonic_usec(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0) {
        return (u64)ts.tv_sec * 1000000ULL + (u64)(ts.tv_nsec / 1000);
    }
    return 0;
}

static time_t get_uptime_seconds(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0) {
        return ts.tv_sec;
    }
    return 0;
}

static u64 get_realtime_usec(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) == 0) {
        return (u64)ts.tv_sec * 1000000ULL + (u64)(ts.tv_nsec / 1000);
    }
    return 0;
}

struct early_journal_entry {
    u64 mono_us;
    u64 real_us;
    int priority;
    char unit[64];
    char message[JOURNAL_MSG_MAX];
};

static struct early_journal_entry journal_queue[JOURNAL_QUEUE_CAP];
static int journal_q_head;
static int journal_q_count;
static int journal_sock_fd = -1;

static void format_unit_name(char *dst, size_t cap, const char *name) {
    if (cap == 0) {
        return;
    }
    if (name == NULL || *name == 0) {
        snprintf(dst, cap, "systemg.service");
    } else if (strchr(name, '.') != NULL) {
        snprintf(dst, cap, "%s", name);
    } else {
        snprintf(dst, cap, "%s.service", name);
    }
}

static void sanitize_journal_message(char *dst, size_t cap, const char *src) {
    size_t d = 0;
    if (cap == 0) {
        return;
    }
    for (size_t s = 0; src[s] != 0 && d + 1 < cap; s++) {
        unsigned char c = (unsigned char)src[s];
        /* Strip ANSI CSI escape sequences (\033[...X) printed to console. */
        if (c == 0x1b && src[s + 1] == '[') {
            s += 2;
            while (src[s] != 0 && ((unsigned char)src[s] < 0x40 || (unsigned char)src[s] > 0x7e)) {
                s++;
            }
            if (src[s] == 0) {
                break;
            }
            continue;
        }
        if (c == '\n' || c == '\r') {
            dst[d++] = ' ';
            continue;
        }
        dst[d++] = (char)c;
    }
    while (d > 0 && dst[d - 1] == ' ') {
        d--;
    }
    dst[d] = 0;
}

static int try_send_journal_entry(const struct early_journal_entry *entry) {
    if (entry->message[0] == 0) {
        return 0;
    }
    if (journal_sock_fd < 0) {
        journal_sock_fd = socket(AF_UNIX, SOCK_DGRAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
        if (journal_sock_fd < 0) {
            return -1;
        }
    }

    char packet[1024];
    int len = snprintf(packet, sizeof(packet),
                       "PRIORITY=%d\n"
                       "SYSLOG_FACILITY=%d\n"
                       "SYSLOG_IDENTIFIER=systemg\n"
                       "_COMM=systemg\n"
                       "_SYSTEMD_UNIT=%s\n"
                       "_PID=%d\n"
                       "_UID=0\n"
                       "_GID=0\n"
                       "_SOURCE_MONOTONIC_TIMESTAMP=%lu\n"
                       "_SOURCE_REALTIME_TIMESTAMP=%lu\n"
                       "MESSAGE=%s\n",
                       entry->priority & 7,
                       LOG_DAEMON >> 3,
                       entry->unit,
                       (int)getpid(),
                       (unsigned long)entry->mono_us,
                       (unsigned long)entry->real_us,
                       entry->message);
    if (len <= 0) {
        return -1;
    }
    if ((size_t)len >= sizeof(packet)) {
        len = (int)strlen(packet);
    }

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    snprintf(addr.sun_path, sizeof(addr.sun_path), "%s", JOURNAL_SOCKET_PATH);

    if (sendto(journal_sock_fd, packet, (size_t)len, 0, &addr, sizeof(addr)) < 0) {
        return -1;
    }
    return 0;
}

static void flush_journal_queue(void) {
    while (journal_q_count > 0) {
        const struct early_journal_entry *entry = &journal_queue[journal_q_head];
        if (try_send_journal_entry(entry) < 0) {
            break;
        }
        journal_q_head = (journal_q_head + 1) % JOURNAL_QUEUE_CAP;
        journal_q_count--;
    }
}

static void journal_log(int priority, const char *unit_name, const char *format, ...) {
    char raw[JOURNAL_MSG_MAX];
    __builtin_va_list args;
    __builtin_va_start(args, format);
    vsnprintf(raw, sizeof(raw), format, args);
    __builtin_va_end(args);

    struct early_journal_entry entry;
    memset(&entry, 0, sizeof(entry));
    entry.mono_us = get_monotonic_usec();
    entry.real_us = get_realtime_usec();
    entry.priority = priority & 7;
    format_unit_name(entry.unit, sizeof(entry.unit), unit_name);
    sanitize_journal_message(entry.message, sizeof(entry.message), raw);
    if (entry.message[0] == 0) {
        return;
    }

    flush_journal_queue();
    if (journal_q_count == 0 && try_send_journal_entry(&entry) == 0) {
        return;
    }

    if (journal_q_count == JOURNAL_QUEUE_CAP) {
        journal_q_head = (journal_q_head + 1) % JOURNAL_QUEUE_CAP;
        journal_q_count--;
    }
    int tail = (journal_q_head + journal_q_count) % JOURNAL_QUEUE_CAP;
    journal_queue[tail] = entry;
    journal_q_count++;
}

static void print_unit_status(const char *tag, const char *action, const struct unit *unit, const char *suffix) {
    const char *sfx = suffix ? suffix : ".";
    int prio = LOG_INFO;
    if (strstr(action, "Failed") != NULL) {
        prio = LOG_ERR;
    } else if (strstr(action, "Dependency") != NULL) {
        prio = LOG_WARNING;
    }

    if (unit->description[0] != 0) {
        printf("%s %s %s.service \033[90m- %s\033[0m%s\n",
               tag, action, unit->name, unit->description, sfx);
        journal_log(prio, unit->name, "%s %s.service - %s%s",
                    action, unit->name, unit->description, sfx);
    } else {
        printf("%s %s %s.service%s\n",
               tag, action, unit->name, sfx);
        journal_log(prio, unit->name, "%s %s.service%s",
                    action, unit->name, sfx);
    }
}

static const char *exit_status_name(int status) {
    switch (status) {
    case 0: return "SUCCESS";
    case 1: return "FAILURE";
    case 2: return "INVALIDARGUMENT";
    case 3: return "NOTIMPLEMENTED";
    case 4: return "NOPERMISSION";
    case 5: return "NOTINSTALLED";
    case 6: return "NOTCONFIGURED";
    case 7: return "NOTRUNNING";
    case 203: return "EXEC";
    case 204: return "NOENT";
    case 205: return "NOMEM";
    default: return "UNKNOWN";
    }
}

static const char *signal_name(int signal_number) {
    switch (signal_number) {
    case SIGHUP: return "HUP";
    case SIGINT: return "INT";
    case SIGQUIT: return "QUIT";
    case SIGILL: return "ILL";
    case SIGTRAP: return "TRAP";
    case SIGABRT: return "ABRT";
    case SIGBUS: return "BUS";
    case SIGFPE: return "FPE";
    case SIGKILL: return "KILL";
    case SIGUSR1: return "USR1";
    case SIGSEGV: return "SEGV";
    case SIGUSR2: return "USR2";
    case SIGPIPE: return "PIPE";
    case SIGALRM: return "ALRM";
    case SIGTERM: return "TERM";
    case SIGCHLD: return "CHLD";
    case SIGCONT: return "CONT";
    case SIGSTOP: return "STOP";
    case SIGTSTP: return "TSTP";
    case SIGTTIN: return "TTIN";
    case SIGTTOU: return "TTOU";
    default: return "UNKNOWN";
    }
}

static void log_process_exit(const struct unit *unit, const char *kind, int status) {
    if (WIFEXITED(status)) {
        int code = WEXITSTATUS(status);
        int prio = (code == 0) ? LOG_INFO : LOG_WARNING;
        printf("systemg[1]: %s: %s exited, code=exited, status=%d/%s%s\n",
               unit->name, kind, code, exit_status_name(code),
               code == 0 ? " (success)" : "");
        journal_log(prio, unit->name, "%s: %s exited, code=exited, status=%d/%s%s",
                    unit->name, kind, code, exit_status_name(code),
                    code == 0 ? " (success)" : "");
    } else if (WIFSIGNALED(status)) {
        int signal_number = WTERMSIG(status);
        const char *reason = (status & 0x80) != 0 ? "dumped" : "killed";
        printf("systemg[1]: %s: %s exited, code=%s, status=%d/%s\n",
               unit->name, kind, reason, signal_number, signal_name(signal_number));
        journal_log(LOG_ERR, unit->name, "%s: %s exited, code=%s, status=%d/%s",
                    unit->name, kind, reason, signal_number, signal_name(signal_number));
    }
}

static char *trim(char *text) {
    while (*text == ' ' || *text == '\t' || *text == '\r') {
        text++;
    }
    char *end = text + strlen(text);
    while (end > text && (end[-1] == ' ' || end[-1] == '\t' || end[-1] == '\r'
                          || end[-1] == '\n')) {
        *--end = 0;
    }
    return text;
}

static void copy_name(char *destination, size_t size, const char *source) {
    size_t index = 0;
    while (index + 1 < size && source[index] != 0) {
        destination[index] = source[index];
        index++;
    }
    destination[index] = 0;
}

static void add_dependency(char list[MAX_DEPS][64], int *count, const char *value) {
    char buffer[256];
    copy_name(buffer, sizeof(buffer), value);
    char *cursor = buffer;
    while (*cursor != 0) {
        while (*cursor == ' ' || *cursor == '\t') {
            cursor++;
        }
        if (*cursor == 0) {
            break;
        }
        char *end = cursor;
        while (*end != 0 && *end != ' ' && *end != '\t') {
            end++;
        }
        char saved = *end;
        *end = 0;
        if (*count < MAX_DEPS) {
            char *dot = strchr(cursor, '.');
            if (dot != NULL) {
                *dot = 0;
            }
            copy_name(list[*count], 64, cursor);
            *count += 1;
        }
        *end = saved;
        cursor = end;
    }
}

static int unit_index(const char *name) {
    for (int index = 0; index < unit_count; index++) {
        if (strcmp(units[index].name, name) == 0) {
            return index;
        }
    }
    return -1;
}

static int load_unit(const char *path, const char *name) {
    int existing = unit_index(name);
    struct unit *unit;
    if (existing >= 0) {
        unit = &units[existing];
    } else {
        if (unit_count >= MAX_UNITS) {
            return -1;
        }
        unit = &units[unit_count];
        memset(unit, 0, sizeof(*unit));
        copy_name(unit->name, sizeof(unit->name), name);
        unit->restart = RESTART_NO;
        unit->restart_sec = 1;
        unit->queue_ts = get_monotonic_usec();
    }

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    char content[4096];
    ssize_t count = read(fd, content, sizeof(content) - 1);
    close(fd);
    if (count <= 0) {
        return -1;
    }
    content[count] = 0;

    int section = 0;
    char *line = content;
    while (line != NULL && *line != 0) {
        char *next = strchr(line, '\n');
        if (next != NULL) {
            *next = 0;
            next++;
        }
        char *text = trim(line);
        if (*text == '[') {
            if (strncmp(text, "[Unit]", 6) == 0) {
                section = 1;
            } else if (strncmp(text, "[Service]", 9) == 0) {
                section = 2;
            } else {
                section = 0;
            }
        } else if (*text != 0 && *text != '#') {
            char *equals = strchr(text, '=');
            if (equals != NULL) {
                *equals = 0;
                char *key = trim(text);
                char *value = trim(equals + 1);
                if (section == 1) {
                    if (strcmp(key, "Description") == 0) {
                        copy_name(unit->description, sizeof(unit->description), value);
                    } else if (strcmp(key, "After") == 0) {
                        add_dependency(unit->after, &unit->after_count, value);
                    } else if (strcmp(key, "Requires") == 0) {
                        add_dependency(unit->requires, &unit->requires_count, value);
                    } else if (strcmp(key, "Wants") == 0) {
                        add_dependency(unit->wants, &unit->wants_count, value);
                    }
                } else if (section == 2) {
                    if (strcmp(key, "ExecStart") == 0) {
                        copy_name(unit->exec, sizeof(unit->exec), value);
                    } else if (strcmp(key, "Type") == 0) {
                        if (strcmp(value, "oneshot") == 0) {
                            unit->type = TYPE_ONESHOT;
                        } else if (strcmp(value, "exec") == 0) {
                            unit->type = TYPE_EXEC;
                        } else {
                            unit->type = TYPE_SIMPLE;
                        }
                    } else if (strcmp(key, "Restart") == 0) {
                        if (strcmp(value, "always") == 0) {
                            unit->restart = RESTART_ALWAYS;
                        } else if (strcmp(value, "on-failure") == 0) {
                            unit->restart = RESTART_ON_FAILURE;
                        } else {
                            unit->restart = RESTART_NO;
                        }
                    } else if (strcmp(key, "RestartSec") == 0) {
                        unit->restart_sec = atoi(value);
                    }
                }
            }
        }
        line = next;
    }
    if (unit->exec[0] == 0) {
        return -1;
    }
    if (existing < 0) {
        unit_count++;
    }
    return 0;
}

static void reload_all_units(void) {
    const char *dir_path = getenv("SPACEOS_SERVICES");
    if (dir_path == NULL || *dir_path == 0) {
        dir_path = "/etc/systemg";
    }
    DIR *directory = opendir(dir_path);
    if (directory == NULL) {
        return;
    }
    struct dirent *entry;
    while ((entry = readdir(directory)) != NULL) {
        if (entry->d_name[0] == '.') {
            continue;
        }
        size_t length = strlen(entry->d_name);
        if (length < 9 || strcmp(entry->d_name + length - 8, ".service") != 0) {
            continue;
        }
        char path[256];
        snprintf(path, sizeof(path), "%s/%s", dir_path, entry->d_name);
        char name[64];
        copy_name(name, sizeof(name), entry->d_name);
        name[length - 8] = 0;
        load_unit(path, name);
    }
    closedir(directory);
}

static int split_arguments(char *command, char *arguments[], int maximum) {
    int count = 0;
    char *cursor = command;
    while (*cursor != 0 && count + 1 < maximum) {
        while (*cursor == ' ' || *cursor == '\t') {
            cursor++;
        }
        if (*cursor == 0) {
            break;
        }
        char *start = cursor;
        int quoted = 0;
        char *write = cursor;
        while (*cursor != 0) {
            if (*cursor == '"') {
                quoted = !quoted;
                cursor++;
                continue;
            }
            if (!quoted && (*cursor == ' ' || *cursor == '\t')) {
                break;
            }
            *write++ = *cursor++;
        }
        if (*cursor != 0) {
            cursor++;
        }
        *write = 0;
        arguments[count++] = start;
    }
    arguments[count] = NULL;
    return count;
}

static int dependencies_ready(const struct unit *unit) {
    for (int index = 0; index < unit->after_count; index++) {
        int other = unit_index(unit->after[index]);
        if (other >= 0 && !units[other].started) {
            return 0;
        }
    }
    for (int index = 0; index < unit->requires_count; index++) {
        int other = unit_index(unit->requires[index]);
        if (other >= 0 && !units[other].started) {
            return 0;
        }
    }
    return 1;
}

static pid_t start_unit(struct unit *unit) {
    if (unit->pid > 0) {
        return unit->pid;
    }
    unit->start_ts = get_monotonic_usec();
    char command[256];
    copy_name(command, sizeof(command), unit->exec);
    char *arguments[16];
    int count = split_arguments(command, arguments, 16);
    if (count == 0) {
        unit->failed = 1;
        journal_log(LOG_ERR, unit->name, "%s.service has no ExecStart command.", unit->name);
        return -1;
    }
    print_unit_status("        ", "Starting", unit, "...");
    pid_t pid = fork();
    if (pid == 0) {
        for (int fd = 3; fd < 256; fd++) {
            close(fd);
        }
        journal_sock_fd = -1;
        /* getty/login creates its own session; a process-group leader cannot
         * call setsid(2).  Other services still get a private process group so
         * systemg can terminate their whole tree. */
        if (strcmp(unit->name, "console") != 0) {
            setpgid(0, 0);
        }
        signal(SIGINT, SIG_DFL);
        signal(SIGQUIT, SIG_DFL);
        signal(SIGTSTP, SIG_DFL);
        signal(SIGTTIN, SIG_DFL);
        signal(SIGTTOU, SIG_DFL);
        signal(SIGCHLD, SIG_DFL);
        execvp(arguments[0], arguments);
        int err = errno;
        fprintf(2, "systemg: exec %s failed: %s\n", arguments[0], strerror(err));
        journal_log(LOG_ERR, unit->name, "Failed to execute %s: %s", arguments[0], strerror(err));
        _exit(127);
    }
    if (pid < 0) {
        int err = errno;
        printf("systemg: fork failed for %s\n", unit->name);
        journal_log(LOG_ERR, unit->name, "Fork failed for %s: %s", unit->name, strerror(err));
        unit->failed = 1;
        return -1;
    }
    unit->pid = pid;
    unit->started = 1;
    unit->ever_started = 1;
    unit->failed = 0;
    unit->pending_restart = 0;
    unit->started_at = get_uptime_seconds();
    if (strcmp(unit->name, "console") != 0) {
        setpgid(pid, pid);
    }
    if (unit->type != TYPE_ONESHOT) {
        unit->active_ts = get_monotonic_usec();
        if (unit->active_ts >= unit->start_ts) {
            unit->duration_us = unit->active_ts - unit->start_ts;
        }
        if (unit->duration_us == 0) {
            unit->duration_us = 1;
        }
        print_unit_status("[  \033[1;32mOK\033[0m  ]", "Started", unit, ".");
    }
    if (unit->type == TYPE_ONESHOT) {
        int status = 0;
        if (waitpid(pid, &status, 0) == pid) {
            unit->pid = 0;
            unit->exit_status = status;
            unit->failed = !(WIFEXITED(status) && WEXITSTATUS(status) == 0);
            log_process_exit(unit, "Control process", status);
        } else {
            unit->pid = 0;
            unit->failed = 1;
        }
        unit->active_ts = get_monotonic_usec();
        if (unit->active_ts >= unit->start_ts) {
            unit->duration_us = unit->active_ts - unit->start_ts;
        }
        if (unit->duration_us == 0) {
            unit->duration_us = 1;
        }
        if (unit->failed) {
            print_unit_status("[\033[1;31mFAILED\033[0m]", "Failed to start", unit, ".");
            printf("systemg[1]: %s: Failed with result 'exit-code'.\n", unit->name);
            journal_log(LOG_ERR, unit->name, "%s: Failed with result 'exit-code'.", unit->name);
        } else {
            print_unit_status("[  \033[1;32mOK\033[0m  ]", "Started", unit, ".");
        }
    }
    return pid;
}

static int stop_unit(struct unit *unit) {
    unit->pending_restart = 0;
    unit->started = 0;
    if (unit->pid > 0) {
        kill(unit->pid, SIGTERM);
        printf("systemg[1]: Stopping %s (PID %d)...\n", unit->name, (int)unit->pid);
        journal_log(LOG_INFO, unit->name, "Stopping %s (PID %d)...", unit->name, (int)unit->pid);
        return 0;
    }
    return 0;
}

static int restart_unit(struct unit *unit) {
    if (unit->pid > 0) {
        unit->pending_restart = 1;
        unit->started = 1;
        printf("systemg[1]: Restarting %s (PID %d)...\n", unit->name, (int)unit->pid);
        journal_log(LOG_INFO, unit->name, "Restarting %s (PID %d)...", unit->name, (int)unit->pid);
        kill(unit->pid, SIGTERM);
        return 0;
    } else {
        unit->pending_restart = 0;
        unit->restarts++;
        return start_unit(unit);
    }
}

static void start_ready_units(void) {
    int progress = 1;
    while (progress) {
        progress = 0;
        for (int index = 0; index < unit_count; index++) {
            struct unit *unit = &units[index];
            if (unit->started) {
                continue;
            }
            if (!dependencies_ready(unit)) {
                continue;
            }
            start_unit(unit);
            progress = 1;
        }
    }
    for (int index = 0; index < unit_count; index++) {
        struct unit *unit = &units[index];
        if (unit->started) {
            continue;
        }
        for (int dependency = 0; dependency < unit->requires_count; dependency++) {
            int other = unit_index(unit->requires[dependency]);
            if (other >= 0 && (!units[other].started || units[other].failed)) {
                print_unit_status("[\033[1;33mDEPEND\033[0m]", "Dependency failed for", unit, ".");
                unit->failed = 1;
                unit->started = 1;
                break;
            }
        }
    }
    int has_ddm = 0;
    int ddm_idx = unit_index("ddm");
    if (ddm_idx >= 0 && units[ddm_idx].started && !units[ddm_idx].failed) {
        has_ddm = 1;
    }
    if (has_ddm) {
        printf("[  \033[1;32mOK\033[0m  ] Reached target \033[1mGraphical Interface\033[0m \033[90m- graphical.target\033[0m.\n");
        journal_log(LOG_INFO, "graphical.target", "Reached target Graphical Interface - graphical.target.");
    } else {
        printf("[  \033[1;32mOK\033[0m  ] Reached target \033[1mMulti-User System\033[0m \033[90m- multi-user.target\033[0m.\n");
        journal_log(LOG_INFO, "multi-user.target", "Reached target Multi-User System - multi-user.target.");
    }
}

static struct unit *unit_for_pid(pid_t pid) {
    for (int index = 0; index < unit_count; index++) {
        if (units[index].pid == pid) {
            return &units[index];
        }
    }
    return NULL;
}

static int should_restart(const struct unit *unit) {
    if (unit->pending_restart) {
        return 1;
    }
    if (unit->restart == RESTART_ALWAYS) {
        return 1;
    }
    if (unit->restart == RESTART_ON_FAILURE && unit->failed) {
        return 1;
    }
    return 0;
}

/* Handle systemctl IPC client command */
static void handle_ipc_client(int client_fd) {
    char req[512];
    ssize_t n = read(client_fd, req, sizeof(req) - 1);
    if (n <= 0) {
        return;
    }
    req[n] = 0;
    char *cmd = trim(req);

    char *arg = strchr(cmd, ' ');
    if (arg != NULL) {
        *arg = 0;
        arg = trim(arg + 1);
        size_t arg_len = strlen(arg);
        if (arg_len > 8 && strcmp(arg + arg_len - 8, ".service") == 0) {
            arg[arg_len - 8] = 0;
        }
    }

    if (strcmp(cmd, "TIMES") == 0 || strcmp(cmd, "TIMINGS") == 0) {
        u64 u_fin = (userspace_finish_usec > 0) ? userspace_finish_usec : get_monotonic_usec();
        dprintf(client_fd, "KERNEL_USEC:%llu\n", (unsigned long long)kernel_finish_usec);
        dprintf(client_fd, "USERSPACE_START_USEC:%llu\n", (unsigned long long)userspace_start_usec);
        dprintf(client_fd, "USERSPACE_FINISH_USEC:%llu\n", (unsigned long long)u_fin);
        dprintf(client_fd, "COUNT:%d\n", unit_count);
        for (int i = 0; i < unit_count; i++) {
            struct unit *u = &units[i];
            const char *state = (u->pid > 0 || (u->type == TYPE_ONESHOT && u->started && !u->failed))
                                    ? "active"
                                    : (u->failed ? "failed" : "inactive");
            const char *sub = (u->pid > 0) ? "running"
                                           : ((u->type == TYPE_ONESHOT && !u->failed && u->ever_started) ? "exited" : "dead");
            const char *type_str = (u->type == TYPE_ONESHOT) ? "oneshot"
                                 : ((u->type == TYPE_EXEC) ? "exec" : "simple");

            char after_buf[256];
            after_buf[0] = 0;
            for (int d = 0; d < u->after_count; d++) {
                if (d > 0) strcat(after_buf, ",");
                strcat(after_buf, u->after[d]);
            }

            char req_buf[256];
            req_buf[0] = 0;
            for (int d = 0; d < u->requires_count; d++) {
                if (d > 0) strcat(req_buf, ",");
                strcat(req_buf, u->requires[d]);
            }

            char wants_buf[256];
            wants_buf[0] = 0;
            for (int d = 0; d < u->wants_count; d++) {
                if (d > 0) strcat(wants_buf, ",");
                strcat(wants_buf, u->wants[d]);
            }

            dprintf(client_fd, "UNIT:%s|%s|%s|%s|%d|%llu|%llu|%llu|%llu|%s|%s|%s|%s\n",
                    u->name, type_str, state, sub, (int)u->pid,
                    (unsigned long long)u->queue_ts,
                    (unsigned long long)u->start_ts,
                    (unsigned long long)u->active_ts,
                    (unsigned long long)u->duration_us,
                    after_buf, req_buf, wants_buf,
                    u->description);
        }
        return;
    }

    if (strcmp(cmd, "DUMP") == 0) {
        time_t now = get_uptime_seconds();
        u64 u_fin = (userspace_finish_usec > 0) ? userspace_finish_usec : get_monotonic_usec();
        u64 k_ms = kernel_finish_usec / 1000;
        u64 u_ms = (u_fin > userspace_start_usec)
                       ? ((u_fin - userspace_start_usec) / 1000)
                       : 0;
        u64 tot_ms = k_ms + u_ms;
        dprintf(client_fd, "--- Manager State ---\n");
        dprintf(client_fd, "PID: 1\n");
        dprintf(client_fd, "Socket: %s\n", g_sock_path);
        dprintf(client_fd, "Kernel startup: %llu.%03llus\n", (unsigned long long)(k_ms / 1000), (unsigned long long)(k_ms % 1000));
        dprintf(client_fd, "Userspace startup: %llu.%03llus\n", (unsigned long long)(u_ms / 1000), (unsigned long long)(u_ms % 1000));
        dprintf(client_fd, "Total startup: %llu.%03llus\n", (unsigned long long)(tot_ms / 1000), (unsigned long long)(tot_ms % 1000));
        dprintf(client_fd, "Uptime: %lds\n", (long)now);
        dprintf(client_fd, "Units loaded: %d\n\n", unit_count);

        for (int i = 0; i < unit_count; i++) {
            struct unit *u = &units[i];
            if (arg != NULL && *arg != 0) {
                if (strstr(u->name, arg) == NULL && strstr(u->description, arg) == NULL) {
                    continue;
                }
            }
            const char *state = (u->pid > 0 || (u->type == TYPE_ONESHOT && u->started && !u->failed))
                                    ? "active (running)"
                                    : (u->failed ? "failed (result 'exit-code')" : "inactive (dead)");
            const char *type_str = (u->type == TYPE_ONESHOT) ? "oneshot"
                                 : ((u->type == TYPE_EXEC) ? "exec" : "simple");
            const char *rst_str = (u->restart == RESTART_ALWAYS) ? "always"
                                : ((u->restart == RESTART_ON_FAILURE) ? "on-failure" : "no");

            dprintf(client_fd, "--- Unit %s.service ---\n", u->name);
            dprintf(client_fd, "  Description: %s\n", u->description);
            dprintf(client_fd, "  Loaded: /etc/systemg/%s.service\n", u->name);
            dprintf(client_fd, "  Type: %s\n", type_str);
            dprintf(client_fd, "  State: %s\n", state);
            dprintf(client_fd, "  Main PID: %d\n", (int)u->pid);
            dprintf(client_fd, "  Exit status: %s (%d)\n", exit_status_name(u->exit_status), u->exit_status);
            dprintf(client_fd, "  Restart policy: %s (delay %ds)\n", rst_str, u->restart_sec);
            dprintf(client_fd, "  Restarts: %d\n", u->restarts);
            dprintf(client_fd, "  ExecStart: %s\n", u->exec);

            u64 q_ms = u->queue_ts / 1000;
            u64 s_ms = u->start_ts / 1000;
            u64 a_ms = u->active_ts / 1000;
            u64 d_ms = u->duration_us / 1000;
            dprintf(client_fd, "  Timeline: queued at %llu.%03llus, started at %llu.%03llus, active at %llu.%03llus (+%llums)\n",
                    (unsigned long long)(q_ms / 1000), (unsigned long long)(q_ms % 1000),
                    (unsigned long long)(s_ms / 1000), (unsigned long long)(s_ms % 1000),
                    (unsigned long long)(a_ms / 1000), (unsigned long long)(a_ms % 1000),
                    (unsigned long long)d_ms);

            dprintf(client_fd, "  After:");
            if (u->after_count == 0) {
                dprintf(client_fd, " (none)");
            } else {
                for (int d = 0; d < u->after_count; d++) {
                    dprintf(client_fd, " %s.service", u->after[d]);
                }
            }
            dprintf(client_fd, "\n");

            dprintf(client_fd, "  Requires:");
            if (u->requires_count == 0) {
                dprintf(client_fd, " (none)");
            } else {
                for (int d = 0; d < u->requires_count; d++) {
                    dprintf(client_fd, " %s.service", u->requires[d]);
                }
            }
            dprintf(client_fd, "\n");

            dprintf(client_fd, "  Wants:");
            if (u->wants_count == 0) {
                dprintf(client_fd, " (none)");
            } else {
                for (int d = 0; d < u->wants_count; d++) {
                    dprintf(client_fd, " %s.service", u->wants[d]);
                }
            }
            dprintf(client_fd, "\n\n");
        }
        return;
    }

    char resp[4096];
    resp[0] = 0;

    if (strcmp(cmd, "LIST") == 0) {
        int offset = snprintf(resp, sizeof(resp), "COUNT:%d\n", unit_count);
        for (int i = 0; i < unit_count && offset < (int)sizeof(resp) - 128; i++) {
            struct unit *u = &units[i];
            const char *state = (u->pid > 0 || (u->type == TYPE_ONESHOT && u->started && !u->failed))
                                    ? "active"
                                    : (u->failed ? "failed" : "inactive");
            const char *sub = (u->pid > 0) ? "running"
                                           : ((u->type == TYPE_ONESHOT && !u->failed && u->ever_started) ? "exited" : "dead");
            offset += snprintf(resp + offset, sizeof(resp) - offset,
                               "%s.service|loaded|%s|%s|%d|%s\n",
                               u->name, state, sub, (int)u->pid, u->description);
        }
    } else if (strcmp(cmd, "STATUS") == 0) {
        if (arg == NULL || *arg == 0) {
            snprintf(resp, sizeof(resp), "ERR:unit name required\n");
        } else {
            int idx = unit_index(arg);
            if (idx < 0) {
                snprintf(resp, sizeof(resp), "ERR:unit not found\n");
            } else {
                struct unit *u = &units[idx];
                const char *state = (u->pid > 0 || (u->type == TYPE_ONESHOT && u->started && !u->failed))
                                        ? "active (running)"
                                        : (u->failed ? "failed (result 'exit-code')" : "inactive (dead)");
                time_t now = get_uptime_seconds();
                time_t uptime = (u->started_at > 0 && now >= u->started_at) ? (now - u->started_at) : 0;
                snprintf(resp, sizeof(resp),
                         "NAME:%s.service\n"
                         "DESC:%s\n"
                         "LOADED:loaded (/etc/systemg/%s.service; enabled)\n"
                         "STATE:%s\n"
                         "PID:%d\n"
                         "EXEC:%s\n"
                         "UPTIME:%ld\n"
                         "RESTARTS:%d\n"
                         "EXIT_CODE:%d\n",
                         u->name, u->description, u->name, state, (int)u->pid, u->exec,
                         (long)uptime, u->restarts, u->exit_status);
            }
        }
    } else if (strcmp(cmd, "START") == 0) {
        if (arg == NULL || *arg == 0) {
            snprintf(resp, sizeof(resp), "ERR:unit name required\n");
        } else {
            int idx = unit_index(arg);
            if (idx < 0) {
                snprintf(resp, sizeof(resp), "ERR:unit not found\n");
            } else {
                start_unit(&units[idx]);
                snprintf(resp, sizeof(resp), "OK\n");
            }
        }
    } else if (strcmp(cmd, "STOP") == 0) {
        if (arg == NULL || *arg == 0) {
            snprintf(resp, sizeof(resp), "ERR:unit name required\n");
        } else {
            int idx = unit_index(arg);
            if (idx < 0) {
                snprintf(resp, sizeof(resp), "ERR:unit not found\n");
            } else {
                stop_unit(&units[idx]);
                snprintf(resp, sizeof(resp), "OK\n");
            }
        }
    } else if (strcmp(cmd, "RESTART") == 0) {
        if (arg == NULL || *arg == 0) {
            snprintf(resp, sizeof(resp), "ERR:unit name required\n");
        } else {
            int idx = unit_index(arg);
            if (idx < 0) {
                snprintf(resp, sizeof(resp), "ERR:unit not found\n");
            } else {
                restart_unit(&units[idx]);
                snprintf(resp, sizeof(resp), "OK\n");
            }
        }
    } else if (strcmp(cmd, "IS_ACTIVE") == 0) {
        if (arg == NULL || *arg == 0) {
            snprintf(resp, sizeof(resp), "inactive\n");
        } else {
            int idx = unit_index(arg);
            if (idx >= 0 && (units[idx].pid > 0 || (units[idx].type == TYPE_ONESHOT && units[idx].started && !units[idx].failed))) {
                snprintf(resp, sizeof(resp), "active\n");
            } else if (idx >= 0 && units[idx].failed) {
                snprintf(resp, sizeof(resp), "failed\n");
            } else {
                snprintf(resp, sizeof(resp), "inactive\n");
            }
        }
    } else if (strcmp(cmd, "IS_FAILED") == 0) {
        if (arg == NULL || *arg == 0) {
            snprintf(resp, sizeof(resp), "unknown\n");
        } else {
            int idx = unit_index(arg);
            if (idx >= 0 && units[idx].failed) {
                snprintf(resp, sizeof(resp), "failed\n");
            } else {
                snprintf(resp, sizeof(resp), "active\n");
            }
        }
    } else if (strcmp(cmd, "RELOAD") == 0) {
        reload_all_units();
        journal_log(LOG_INFO, "systemg.service", "Reloaded unit configuration.");
        snprintf(resp, sizeof(resp), "OK\n");
    } else {
        snprintf(resp, sizeof(resp), "ERR:unknown command\n");
    }

    write(client_fd, resp, strlen(resp));
}

/* Load the modules listed in /etc/modules through the Linux finit_module(2)
 * syscall. Failures are reported but do not abort PID 1. */
static void load_kernel_modules(void) {
    int fd = open("/etc/modules", O_RDONLY);
    if (fd < 0) {
        return;
    }
    char content[2048];
    ssize_t count = read(fd, content, sizeof(content) - 1);
    close(fd);
    if (count <= 0) {
        return;
    }
    content[count] = 0;
    char *line = content;
    while (line != NULL && *line != 0) {
        char *next = strchr(line, '\n');
        if (next != NULL) {
            *next = 0;
            next++;
        }
        char *name = trim(line);
        if (*name != 0 && *name != '#') {
            char path[256];
            snprintf(path, sizeof(path), "/lib/modules/%s.ko", name);
            int image = open(path, O_RDONLY | O_CLOEXEC);
            if (image < 0) {
                int err = errno;
                printf("systemg[1]: Failed to open kernel module %s: %s\n",
                       path, strerror(err));
                journal_log(LOG_WARNING, "systemg.service",
                            "Failed to open kernel module %s: %s", path, strerror(err));
            } else {
                int result = finit_module(image, "", 0);
                int err = errno;
                close(image);
                if (result < 0) {
                    printf("systemg[1]: Failed to load kernel module %s: %s\n",
                           name, strerror(err));
                    journal_log(LOG_ERR, "systemg.service",
                                "Failed to load kernel module %s: %s", name, strerror(err));
                } else {
                    printf("systemg[1]: Loaded kernel module %s\n", name);
                    journal_log(LOG_INFO, "systemg.service",
                                "Loaded kernel module %s.", name);
                }
            }
        }
        line = next;
    }
}

int main(int argc, char **argv, char **envp) {
    (void)argc;
    (void)argv;
    (void)envp;
    kernel_finish_usec = get_monotonic_usec();
    userspace_start_usec = kernel_finish_usec;
    int console = open("/dev/console", O_RDWR);
    if (console >= 0) {
        dup2(console, 0);
        dup2(console, 1);
        dup2(console, 2);
        if (console > 2) {
            close(console);
        }
    }
    setenv("PATH", "/bin:/sbin:/usr/bin", 1);
    setenv("HOME", "/root", 1);
    signal(SIGCHLD, child_handler);
    signal(SIGINT, SIG_IGN);
    signal(SIGQUIT, SIG_IGN);
    signal(SIGTERM, SIG_IGN);
    signal(SIGTSTP, SIG_IGN);
    signal(SIGTTIN, SIG_IGN);
    signal(SIGTTOU, SIG_IGN);
    printf("SYSTEMG_OK pid=%d\n", getpid());
    journal_log(LOG_INFO, "systemg.service",
                "SpaceOS systemg PID %d initialized.", (int)getpid());
    load_kernel_modules();

    /* Initialize control socket */
    const char *env_sock = getenv("SYSTEMG_SOCK");
    if (env_sock != NULL && *env_sock != 0) {
        g_sock_path = env_sock;
    } else {
        mkdir("/run", 0755);
    }
    unlink(g_sock_path);
    int server_fd = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
    if (server_fd >= 0) {
        struct sockaddr_un sun;
        memset(&sun, 0, sizeof(sun));
        sun.sun_family = AF_UNIX;
        copy_name(sun.sun_path, sizeof(sun.sun_path), g_sock_path);
        if (bind(server_fd, &sun, sizeof(sun)) == 0) {
            listen(server_fd, 8);
        } else {
            close(server_fd);
            server_fd = -1;
        }
    }

    reload_all_units();
    start_ready_units();
    userspace_finish_usec = get_monotonic_usec();
    flush_journal_queue();

    for (;;) {
        flush_journal_queue();
        int status = 0;
        pid_t pid;
        while ((pid = waitpid(-1, &status, WNOHANG | WUNTRACED | WCONTINUED)) > 0) {
            if (WIFSTOPPED(status)) {
                continue;
            }
            if (WIFCONTINUED(status)) {
                continue;
            }
            struct unit *unit = unit_for_pid(pid);
            if (unit != NULL) {
                unit->pid = 0;
                unit->exit_status = status;
                unit->failed = !(WIFEXITED(status) && WEXITSTATUS(status) == 0);
                log_process_exit(unit, "Main process", status);
                if (unit->failed) {
                    printf("systemg[1]: %s: Failed with result 'exit-code'.\n",
                           unit->name);
                    journal_log(LOG_ERR, unit->name,
                                "%s: Failed with result 'exit-code'.", unit->name);
                } else {
                    printf("systemg[1]: %s: Deactivated successfully.\n", unit->name);
                    journal_log(LOG_INFO, unit->name,
                                "%s: Deactivated successfully.", unit->name);
                }
                print_unit_status("[  \033[1;32mOK\033[0m  ]", "Stopped", unit, ".");
                if (should_restart(unit)) {
                    unit->pending_restart = 1;
                }
            }
        }
        for (int index = 0; index < unit_count; index++) {
            struct unit *unit = &units[index];
            if (unit->pid == 0 && unit->started && should_restart(unit)) {
                unit->restarts += 1;
                printf("systemg[1]: %s: Scheduled restart job, restart counter is at %d.\n",
                       unit->name, unit->restarts);
                journal_log(LOG_NOTICE, unit->name,
                            "%s: Scheduled restart job, restart counter is at %d.",
                            unit->name, unit->restarts);
                sleep((unsigned int)unit->restart_sec);
                start_unit(unit);
            }
        }
        flush_journal_queue();

        /* Poll IPC server socket with 100ms timeout */
        if (server_fd >= 0) {
            struct pollfd pfd;
            pfd.fd = server_fd;
            pfd.events = POLLIN;
            pfd.revents = 0;
            int r = poll(&pfd, 1, 100);
            if (r > 0 && (pfd.revents & POLLIN)) {
                int client_fd = accept(server_fd, NULL, NULL);
                if (client_fd >= 0) {
                    handle_ipc_client(client_fd);
                    close(client_fd);
                }
            }
        } else {
            usleep(100000);
        }
        child_changed = 0;
    }
}
