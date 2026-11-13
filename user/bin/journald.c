/*
 *
 *       user/bin/journald.c
 *       Production-grade Systemd-compatible Journal Logging Daemon
 *
 *       2026/11/06 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#include "splibc.h"
#include "journal.h"

#define MAX_RING_ENTRIES 2048
#define MAX_PAYLOAD_SIZE 4096
#define MAX_JOURNAL_SIZE (8 * 1024 * 1024)
#define MAX_CLIENTS 16

struct ring_entry {
    struct journal_entry_hdr hdr;
    char unit[64];
    char comm[32];
    char ident[32];
    char message[1024];
};

struct journal_daemon {
    char machine_id[33];
    char boot_id[33];
    int journal_fd;
    char current_path[256];
    u64 seq_counter;
    u64 entry_count;
    u64 error_count;
    u64 warn_count;
    u64 kernel_count;

    int kmsg_fd;
    int journal_sock_fd;
    int syslog_sock_fd;
    int stdout_listen_fd;
    int query_listen_fd;

    int query_clients[MAX_CLIENTS];
    int query_follow[MAX_CLIENTS];
    int client_count;

    struct ring_entry ring[MAX_RING_ENTRIES];
    int ring_head;
    int ring_count;

    volatile int running;
};

static struct journal_daemon daemon_state;

static u64 get_time_usec(int clockid) {
    struct timespec ts;
    if (clock_gettime(clockid, &ts) == 0) {
        return (u64)ts.tv_sec * 1000000ULL + (u64)(ts.tv_nsec / 1000);
    }
    return 0;
}

static void generate_hex_id(char *out, size_t len) {
    u64 r1 = get_time_usec(CLOCK_REALTIME);
    u64 r2 = get_time_usec(CLOCK_MONOTONIC);
    int fd = open("/dev/urandom", O_RDONLY);
    if (fd >= 0) {
        u64 buf[2];
        if (read(fd, buf, sizeof(buf)) == (ssize_t)sizeof(buf)) {
            r1 ^= buf[0];
            r2 ^= buf[1];
        }
        close(fd);
    }
    snprintf(out, len, "%016lx%016lx", (unsigned long)r1, (unsigned long)r2);
}

static void init_machine_and_boot_id(void) {
    /* Machine ID from /etc/machine-id */
    int mfd = open("/etc/machine-id", O_RDONLY);
    int valid = 0;
    if (mfd >= 0) {
        char buf[64];
        ssize_t n = read(mfd, buf, sizeof(buf) - 1);
        close(mfd);
        if (n >= 32) {
            buf[32] = 0;
            strncpy(daemon_state.machine_id, buf, sizeof(daemon_state.machine_id) - 1);
            valid = 1;
        }
    }
    if (!valid) {
        generate_hex_id(daemon_state.machine_id, sizeof(daemon_state.machine_id));
        int wfd = open("/etc/machine-id", O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (wfd >= 0) {
            write(wfd, daemon_state.machine_id, 32);
            write(wfd, "\n", 1);
            close(wfd);
        }
    }

    /* Boot ID */
    int bfd = open("/proc/sys/kernel/random/boot_id", O_RDONLY);
    valid = 0;
    if (bfd >= 0) {
        char buf[64];
        ssize_t n = read(bfd, buf, sizeof(buf) - 1);
        close(bfd);
        if (n >= 32) {
            int di = 0;
            for (ssize_t i = 0; i < n && di < 32; i++) {
                if (buf[i] != '-') {
                    daemon_state.boot_id[di++] = buf[i];
                }
            }
            daemon_state.boot_id[32] = 0;
            if (di == 32) {
                valid = 1;
            }
        }
    }
    if (!valid) {
        generate_hex_id(daemon_state.boot_id, sizeof(daemon_state.boot_id));
    }
}

static void ensure_directories(void) {
    mkdir("/run", 0755);
    mkdir("/run/systemd", 0755);
    mkdir("/run/systemd/journal", 0755);
    mkdir("/run/log", 0755);
    mkdir("/run/log/journal", 0755);
    mkdir("/var", 0755);
    mkdir("/var/log", 0755);
    mkdir("/var/log/journal", 0755);
}

static int open_or_create_journal(void) {
    ensure_directories();
    const char *path = JOURNAL_FILE_SYSTEM;
    int fd = open(path, O_RDWR | O_CREAT, 0640);
    if (fd < 0) {
        path = JOURNAL_FILE_VOLATILE;
        fd = open(path, O_RDWR | O_CREAT, 0640);
    }
    if (fd < 0) {
        return -1;
    }

    strncpy(daemon_state.current_path, path, sizeof(daemon_state.current_path) - 1);

    struct stat st;
    if (fstat(fd, &st) == 0 && st.st_size >= (off_t)sizeof(struct journal_header)) {
        struct journal_header hdr;
        if (read(fd, &hdr, sizeof(hdr)) == (ssize_t)sizeof(hdr) &&
            hdr.magic == JOURNAL_HEADER_MAGIC) {
            daemon_state.seq_counter = hdr.max_seq;
            daemon_state.entry_count = hdr.entry_count;
            daemon_state.journal_fd = fd;
            return 0;
        }
    }

    /* Initialize fresh journal header */
    lseek(fd, 0, SEEK_SET);
    struct journal_header hdr;
    memset(&hdr, 0, sizeof(hdr));
    hdr.magic = JOURNAL_HEADER_MAGIC;
    hdr.version = JOURNAL_VERSION;
    hdr.header_size = sizeof(struct journal_header);
    hdr.entry_count = 0;
    hdr.head_entry_offset = 0;
    hdr.tail_entry_offset = 0;
    strncpy(hdr.machine_id, daemon_state.machine_id, sizeof(hdr.machine_id) - 1);
    strncpy(hdr.boot_id, daemon_state.boot_id, sizeof(hdr.boot_id) - 1);
    hdr.min_realtime_us = get_time_usec(CLOCK_REALTIME);
    hdr.max_realtime_us = hdr.min_realtime_us;
    hdr.file_size = sizeof(struct journal_header);

    write(fd, &hdr, sizeof(hdr));
    daemon_state.journal_fd = fd;
    daemon_state.seq_counter = 0;
    daemon_state.entry_count = 0;
    return 0;
}

static void rotate_journal_if_needed(void) {
    if (daemon_state.journal_fd < 0) {
        return;
    }
    struct stat st;
    if (fstat(daemon_state.journal_fd, &st) == 0 && st.st_size >= MAX_JOURNAL_SIZE) {
        close(daemon_state.journal_fd);
        daemon_state.journal_fd = -1;

        char rotated[280];
        u64 now = get_time_usec(CLOCK_REALTIME);
        snprintf(rotated, sizeof(rotated), "%s@%lu.journal", daemon_state.current_path, (unsigned long)now);
        rename(daemon_state.current_path, rotated);
        open_or_create_journal();
    }
}

static void escape_query_field(char *dest, size_t dest_len, const char *src) {
    size_t d = 0;
    for (size_t s = 0; src[s] != 0 && d + 2 < dest_len; s++) {
        if (src[s] == '\n') {
            dest[d++] = '\\';
            dest[d++] = 'n';
        } else if (src[s] == '\r') {
            dest[d++] = '\\';
            dest[d++] = 'r';
        } else if (src[s] == '|') {
            dest[d++] = '/';
        } else {
            dest[d++] = src[s];
        }
    }
    dest[d] = 0;
}

static void broadcast_to_followers(const struct ring_entry *re) {
    char esc_msg[1200];
    escape_query_field(esc_msg, sizeof(esc_msg), re->message);

    char line[1500];
    int len = snprintf(line, sizeof(line),
                       "ENTRY|%lu|%lu|%lu|%u|%u|%u|%d|%u|%u|%s|%s|%s|%s\n",
                       (unsigned long)re->hdr.seq_num,
                       (unsigned long)re->hdr.realtime_usec,
                       (unsigned long)re->hdr.monotonic_usec,
                       re->hdr.priority,
                       re->hdr.facility,
                       re->hdr.transport,
                       re->hdr.pid,
                       re->hdr.uid,
                       re->hdr.gid,
                       re->unit,
                       re->comm,
                       re->ident,
                       esc_msg);

    for (int i = 0; i < MAX_CLIENTS; i++) {
        int cfd = daemon_state.query_clients[i];
        if (cfd >= 0 && daemon_state.query_follow[i]) {
            if (write(cfd, line, len) < 0) {
                close(cfd);
                daemon_state.query_clients[i] = -1;
                daemon_state.query_follow[i] = 0;
                daemon_state.client_count--;
            }
        }
    }
}

static void write_entry_record(u8 transport, u8 priority, u8 facility,
                               u32 pid, u32 uid, u32 gid,
                               const char *unit, const char *comm,
                               const char *ident, const char *message,
                               u64 monotonic_us, u64 realtime_us) {
    if (message == NULL || *message == 0) {
        return;
    }

    if (monotonic_us == 0) {
        monotonic_us = get_time_usec(CLOCK_MONOTONIC);
    }
    if (realtime_us == 0) {
        realtime_us = get_time_usec(CLOCK_REALTIME);
    }

    daemon_state.seq_counter++;
    daemon_state.entry_count++;
    if (priority <= LOG_ERR) {
        daemon_state.error_count++;
    } else if (priority == LOG_WARNING) {
        daemon_state.warn_count++;
    }
    if (transport == TRANSPORT_KERNEL) {
        daemon_state.kernel_count++;
    }

    /* Save into in-memory ring */
    int slot = (daemon_state.ring_head + daemon_state.ring_count) % MAX_RING_ENTRIES;
    if (daemon_state.ring_count == MAX_RING_ENTRIES) {
        daemon_state.ring_head = (daemon_state.ring_head + 1) % MAX_RING_ENTRIES;
        slot = (daemon_state.ring_head + daemon_state.ring_count - 1) % MAX_RING_ENTRIES;
    } else {
        daemon_state.ring_count++;
    }

    struct ring_entry *re = &daemon_state.ring[slot];
    memset(re, 0, sizeof(*re));
    re->hdr.magic = JOURNAL_ENTRY_MAGIC;
    re->hdr.seq_num = daemon_state.seq_counter;
    re->hdr.realtime_usec = realtime_us;
    re->hdr.monotonic_usec = monotonic_us;
    re->hdr.priority = priority;
    re->hdr.facility = facility;
    re->hdr.transport = transport;
    re->hdr.pid = pid;
    re->hdr.uid = uid;
    re->hdr.gid = gid;

    strncpy(re->unit, unit ? unit : "", sizeof(re->unit) - 1);
    strncpy(re->comm, comm ? comm : "", sizeof(re->comm) - 1);
    strncpy(re->ident, ident ? ident : "", sizeof(re->ident) - 1);
    strncpy(re->message, message, sizeof(re->message) - 1);

    size_t ulen = strlen(re->unit);
    size_t clen = strlen(re->comm);
    size_t ilen = strlen(re->ident);
    size_t mlen = strlen(re->message);

    re->hdr.unit_len = (u16)ulen;
    re->hdr.comm_len = (u16)clen;
    re->hdr.ident_len = (u16)ilen;
    re->hdr.message_len = (u32)mlen;

    u32 payload_size = (u32)(ulen + 1 + clen + 1 + ilen + 1 + mlen + 1);
    re->hdr.payload_size = payload_size;
    re->hdr.entry_size = sizeof(struct journal_entry_hdr) + payload_size;

    /* Write to on-disk journal if open */
    if (daemon_state.journal_fd >= 0) {
        rotate_journal_if_needed();

        off_t offset = lseek(daemon_state.journal_fd, 0, SEEK_END);
        write(daemon_state.journal_fd, &re->hdr, sizeof(re->hdr));
        write(daemon_state.journal_fd, re->unit, ulen + 1);
        write(daemon_state.journal_fd, re->comm, clen + 1);
        write(daemon_state.journal_fd, re->ident, ilen + 1);
        write(daemon_state.journal_fd, re->message, mlen + 1);

        /* Update header */
        struct journal_header hdr;
        lseek(daemon_state.journal_fd, 0, SEEK_SET);
        if (read(daemon_state.journal_fd, &hdr, sizeof(hdr)) == (ssize_t)sizeof(hdr)) {
            if (hdr.head_entry_offset == 0) {
                hdr.head_entry_offset = offset;
                hdr.min_seq = daemon_state.seq_counter;
                hdr.min_realtime_us = realtime_us;
            }
            hdr.tail_entry_offset = offset;
            hdr.entry_count = daemon_state.entry_count;
            hdr.max_seq = daemon_state.seq_counter;
            hdr.max_realtime_us = realtime_us;
            hdr.file_size = offset + re->hdr.entry_size;
            lseek(daemon_state.journal_fd, 0, SEEK_SET);
            write(daemon_state.journal_fd, &hdr, sizeof(hdr));
        }
    }

    broadcast_to_followers(re);
}

/* Parse /dev/kmsg record: "<pri>,<seq>,<usec>,<flags>;<message>\n" */
static void handle_kmsg_input(void) {
    if (daemon_state.kmsg_fd < 0) {
        return;
    }

    char buf[2048];
    for (;;) {
        ssize_t n = read(daemon_state.kmsg_fd, buf, sizeof(buf) - 1);
        if (n <= 0) {
            break;
        }
        buf[n] = 0;

        char *comma1 = strchr(buf, ',');
        if (comma1 == NULL) {
            continue;
        }
        *comma1 = 0;
        int pnum = atoi(buf);
        u8 prio = pnum & 7;
        u8 facility = (pnum >> 3);

        char *comma2 = strchr(comma1 + 1, ',');
        if (comma2 == NULL) {
            continue;
        }
        /* comma1 + 1 is sequence */
        char *comma3 = strchr(comma2 + 1, ',');
        if (comma3 == NULL) {
            continue;
        }
        *comma3 = 0;
        u64 usec = (u64)strtol(comma2 + 1, NULL, 10);

        char *semicolon = strchr(comma3 + 1, ';');
        if (semicolon == NULL) {
            continue;
        }
        char *msg = semicolon + 1;
        size_t mlen = strlen(msg);
        while (mlen > 0 && (msg[mlen - 1] == '\n' || msg[mlen - 1] == '\r')) {
            msg[--mlen] = 0;
        }

        write_entry_record(TRANSPORT_KERNEL, prio, facility, 0, 0, 0,
                           "kernel", "kernel", "kernel", msg, usec, 0);
    }
}

/* Parse systemd Native Journal Protocol */
static void handle_journal_native_input(void) {
    if (daemon_state.journal_sock_fd < 0) {
        return;
    }

    char buf[4096];
    for (;;) {
        struct sockaddr_un sun;
        socklen_t slen = sizeof(sun);
        ssize_t n = recvfrom(daemon_state.journal_sock_fd, buf, sizeof(buf) - 1, 0,
                             &sun, &slen);
        if (n <= 0) {
            break;
        }
        buf[n] = 0;

        char message[1024] = {0};
        char unit[64] = "system";
        char comm[32] = {0};
        char ident[32] = {0};
        u8 prio = LOG_INFO;
        u8 fac = LOG_USER >> 3;
        u32 pid = 0;
        u32 uid = 0;
        u32 gid = 0;
        u64 mono_us = 0;
        u64 real_us = 0;

        char *ptr = buf;
        char *end = buf + n;
        while (ptr < end && *ptr != 0) {
            char *line_end = strchr(ptr, '\n');
            if (line_end == NULL) {
                line_end = end;
            }
            char *eq = strchr(ptr, '=');
            if (eq != NULL && eq < line_end) {
                *eq = 0;
                *line_end = 0;
                char *key = ptr;
                char *val = eq + 1;

                if (strcmp(key, "MESSAGE") == 0) {
                    strncpy(message, val, sizeof(message) - 1);
                } else if (strcmp(key, "PRIORITY") == 0) {
                    prio = (u8)atoi(val) & 7;
                } else if (strcmp(key, "SYSLOG_FACILITY") == 0) {
                    fac = (u8)atoi(val);
                } else if (strcmp(key, "SYSLOG_IDENTIFIER") == 0) {
                    strncpy(ident, val, sizeof(ident) - 1);
                } else if (strcmp(key, "_COMM") == 0) {
                    strncpy(comm, val, sizeof(comm) - 1);
                } else if (strcmp(key, "_SYSTEMD_UNIT") == 0) {
                    strncpy(unit, val, sizeof(unit) - 1);
                } else if (strcmp(key, "_PID") == 0) {
                    pid = (u32)atoi(val);
                } else if (strcmp(key, "_UID") == 0) {
                    uid = (u32)atoi(val);
                } else if (strcmp(key, "_GID") == 0) {
                    gid = (u32)atoi(val);
                } else if (strcmp(key, "_SOURCE_MONOTONIC_TIMESTAMP") == 0) {
                    mono_us = (u64)strtol(val, NULL, 10);
                } else if (strcmp(key, "_SOURCE_REALTIME_TIMESTAMP") == 0) {
                    real_us = (u64)strtol(val, NULL, 10);
                }
            }
            ptr = (line_end < end) ? (line_end + 1) : end;
        }

        if (ident[0] == 0 && comm[0] != 0) {
            strncpy(ident, comm, sizeof(ident) - 1);
        }
        if (message[0] != 0) {
            write_entry_record(TRANSPORT_JOURNAL, prio, fac,
                               pid, uid, gid, unit, comm, ident, message,
                               mono_us, real_us);
        }
    }
}

/* Parse syslog /dev/log format */
static void handle_syslog_input(void) {
    if (daemon_state.syslog_sock_fd < 0) {
        return;
    }

    char buf[2048];
    for (;;) {
        struct sockaddr_un sun;
        socklen_t slen = sizeof(sun);
        ssize_t n = recvfrom(daemon_state.syslog_sock_fd, buf, sizeof(buf) - 1, 0,
                             &sun, &slen);
        if (n <= 0) {
            break;
        }
        buf[n] = 0;

        u8 prio = LOG_INFO;
        u8 fac = LOG_USER >> 3;
        char *msg = buf;
        if (msg[0] == '<') {
            char *close_angle = strchr(msg, '>');
            if (close_angle != NULL) {
                *close_angle = 0;
                int p = atoi(msg + 1);
                prio = p & 7;
                fac = (p >> 3);
                msg = close_angle + 1;
            }
        }

        /* Extract identifier: "ident[pid]: msg" or "ident: msg" */
        char ident[32] = "syslog";
        char unit[64] = "syslog";
        u32 pid = 0;
        char *colon = strstr(msg, ": ");
        if (colon != NULL && colon - msg < 32) {
            char tag[48];
            size_t tlen = (size_t)(colon - msg);
            memcpy(tag, msg, tlen);
            tag[tlen] = 0;
            msg = colon + 2;

            char *bracket = strchr(tag, '[');
            if (bracket != NULL) {
                *bracket = 0;
                pid = (u32)atoi(bracket + 1);
            }
            strncpy(ident, tag, sizeof(ident) - 1);
            snprintf(unit, sizeof(unit), "%s.service", ident);
        }

        size_t mlen = strlen(msg);
        while (mlen > 0 && (msg[mlen - 1] == '\n' || msg[mlen - 1] == '\r')) {
            msg[--mlen] = 0;
        }

        write_entry_record(TRANSPORT_SYSLOG, prio, fac, pid, 0, 0,
                           unit, ident, ident, msg, 0, 0);
    }
}

static int entry_matches_filter(const struct ring_entry *re,
                                const char *unit_filter, int max_prio) {
    if (re->hdr.priority > max_prio) {
        return 0;
    }
    if (unit_filter[0] == 0) {
        return 1;
    }
    if (strstr(re->unit, unit_filter) != NULL) {
        return 1;
    }
    if ((strcmp(unit_filter, "systemg") == 0 ||
         strcmp(unit_filter, "systemg.service") == 0) &&
        (strcmp(re->comm, "systemg") == 0 || strcmp(re->ident, "systemg") == 0)) {
        return 1;
    }
    return 0;
}

static void write_query_entry(int cfd, const struct ring_entry *re) {
    char esc_msg[1200];
    escape_query_field(esc_msg, sizeof(esc_msg), re->message);
    char line[1500];
    int len = snprintf(line, sizeof(line),
                       "ENTRY|%lu|%lu|%lu|%u|%u|%u|%d|%u|%u|%s|%s|%s|%s\n",
                       (unsigned long)re->hdr.seq_num,
                       (unsigned long)re->hdr.realtime_usec,
                       (unsigned long)re->hdr.monotonic_usec,
                       re->hdr.priority,
                       re->hdr.facility,
                       re->hdr.transport,
                       re->hdr.pid,
                       re->hdr.uid,
                       re->hdr.gid,
                       re->unit,
                       re->comm,
                       re->ident,
                       esc_msg);
    write(cfd, line, len);
}

static void handle_query_request(int cfd, char *cmd) {
    while (*cmd == ' ' || *cmd == '\t') cmd++;
    char *end = cmd + strlen(cmd);
    while (end > cmd && (end[-1] == '\n' || end[-1] == '\r' || end[-1] == ' ')) {
        *--end = 0;
    }

    if (strcmp(cmd, "STAT") == 0) {
        struct stat st;
        u64 size = 0;
        if (daemon_state.journal_fd >= 0 && fstat(daemon_state.journal_fd, &st) == 0) {
            size = (u64)st.st_size;
        }
        char resp[512];
        int len = snprintf(resp, sizeof(resp),
                           "STAT|machine=%s|boot=%s|entries=%lu|errors=%lu|warns=%lu|kernel=%lu|size=%lu\n",
                           daemon_state.machine_id, daemon_state.boot_id,
                           (unsigned long)daemon_state.entry_count,
                           (unsigned long)daemon_state.error_count,
                           (unsigned long)daemon_state.warn_count,
                           (unsigned long)daemon_state.kernel_count,
                           (unsigned long)size);
        write(cfd, resp, len);
        return;
    }

    if (strcmp(cmd, "COUNT") == 0) {
        char resp[64];
        int len = snprintf(resp, sizeof(resp), "COUNT|%lu\n", (unsigned long)daemon_state.entry_count);
        write(cfd, resp, len);
        return;
    }

    if (strcmp(cmd, "FOLLOW") == 0) {
        for (int i = 0; i < MAX_CLIENTS; i++) {
            if (daemon_state.query_clients[i] == cfd) {
                daemon_state.query_follow[i] = 1;
                break;
            }
        }
        write(cfd, "OK\n", 3);
        return;
    }

    if (strncmp(cmd, "QUERY", 5) == 0) {
        char *args = cmd + 5;
        while (*args == ' ') args++;

        char unit_filter[64] = {0};
        int max_prio = 7;
        int limit = 1000;
        int reverse = 0;

        char *token = args;
        while (*token != 0) {
            char *next = strchr(token, ' ');
            if (next != NULL) {
                *next = 0;
                next++;
            }
            if (strncmp(token, "unit=", 5) == 0) {
                strncpy(unit_filter, token + 5, sizeof(unit_filter) - 1);
            } else if (strncmp(token, "prio=", 5) == 0) {
                max_prio = atoi(token + 5);
            } else if (strncmp(token, "limit=", 6) == 0) {
                limit = atoi(token + 6);
            } else if (strcmp(token, "reverse=1") == 0) {
                reverse = 1;
            }
            token = next ? next : "";
        }

        int sent = 0;
        if (reverse) {
            for (int i = daemon_state.ring_count - 1; i >= 0 && sent < limit; i--) {
                int idx = (daemon_state.ring_head + i) % MAX_RING_ENTRIES;
                const struct ring_entry *re = &daemon_state.ring[idx];
                if (!entry_matches_filter(re, unit_filter, max_prio)) {
                    continue;
                }
                write_query_entry(cfd, re);
                sent++;
            }
        } else {
            for (int i = 0; i < daemon_state.ring_count && sent < limit; i++) {
                int idx = (daemon_state.ring_head + i) % MAX_RING_ENTRIES;
                const struct ring_entry *re = &daemon_state.ring[idx];
                if (!entry_matches_filter(re, unit_filter, max_prio)) {
                    continue;
                }
                write_query_entry(cfd, re);
                sent++;
            }
        }
        write(cfd, "END\n", 4);
        return;
    }

    write(cfd, "ERR: Unknown command\n", 21);
}

static void setup_listeners(void) {
    /* /dev/kmsg */
    daemon_state.kmsg_fd = open("/dev/kmsg", O_RDONLY | O_NONBLOCK);

    /* Native Journal Datagram Socket */
    unlink(JOURNAL_SOCKET_PATH);
    int sfd = socket(AF_UNIX, SOCK_DGRAM | SOCK_NONBLOCK, 0);
    if (sfd >= 0) {
        struct sockaddr_un sun;
        memset(&sun, 0, sizeof(sun));
        sun.sun_family = AF_UNIX;
        strncpy(sun.sun_path, JOURNAL_SOCKET_PATH, sizeof(sun.sun_path) - 1);
        if (bind(sfd, &sun, sizeof(sun)) == 0) {
            daemon_state.journal_sock_fd = sfd;
        } else {
            close(sfd);
        }
    }

    /* Syslog Datagram Socket */
    unlink(JOURNAL_DEV_LOG_PATH);
    unlink(JOURNAL_DEV_LOG_SYMLINK);
    int lfd = socket(AF_UNIX, SOCK_DGRAM | SOCK_NONBLOCK, 0);
    if (lfd >= 0) {
        struct sockaddr_un sun;
        memset(&sun, 0, sizeof(sun));
        sun.sun_family = AF_UNIX;
        strncpy(sun.sun_path, JOURNAL_DEV_LOG_PATH, sizeof(sun.sun_path) - 1);
        if (bind(lfd, &sun, sizeof(sun)) == 0) {
            daemon_state.syslog_sock_fd = lfd;
            symlink(JOURNAL_DEV_LOG_PATH, JOURNAL_DEV_LOG_SYMLINK);
        } else {
            close(lfd);
        }
    }

    /* Query IPC Socket */
    unlink(JOURNAL_QUERY_PATH);
    int qfd = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
    if (qfd >= 0) {
        struct sockaddr_un sun;
        memset(&sun, 0, sizeof(sun));
        sun.sun_family = AF_UNIX;
        strncpy(sun.sun_path, JOURNAL_QUERY_PATH, sizeof(sun.sun_path) - 1);
        if (bind(qfd, &sun, sizeof(sun)) == 0) {
            listen(qfd, 16);
            daemon_state.query_listen_fd = qfd;
        } else {
            close(qfd);
        }
    }
}

static void sig_term(int sig) {
    (void)sig;
    daemon_state.running = 0;
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

    signal(SIGTERM, sig_term);
    signal(SIGINT, sig_term);
    signal(SIGPIPE, SIG_IGN);

    memset(&daemon_state, 0, sizeof(daemon_state));
    daemon_state.running = 1;
    daemon_state.journal_fd = -1;
    daemon_state.kmsg_fd = -1;
    daemon_state.journal_sock_fd = -1;
    daemon_state.syslog_sock_fd = -1;
    daemon_state.stdout_listen_fd = -1;
    daemon_state.query_listen_fd = -1;

    for (int i = 0; i < MAX_CLIENTS; i++) {
        daemon_state.query_clients[i] = -1;
    }

    init_machine_and_boot_id();
    open_or_create_journal();
    setup_listeners();

    printf("systemd-journald[start]: Journal started, machine=%s boot=%s\n",
           daemon_state.machine_id, daemon_state.boot_id);

    /* Initial drain of early kernel boot logs */
    handle_kmsg_input();

    write_entry_record(TRANSPORT_JOURNAL, LOG_INFO, LOG_DAEMON >> 3,
                       getpid(), getuid(), getgid(),
                       "systemd-journald.service", "systemd-journald",
                       "journald", "Runtime journal is active.", 0, 0);

    while (daemon_state.running) {
        struct pollfd pfds[MAX_CLIENTS + 8];
        int pcount = 0;

        if (daemon_state.kmsg_fd >= 0) {
            pfds[pcount].fd = daemon_state.kmsg_fd;
            pfds[pcount].events = POLLIN;
            pfds[pcount].revents = 0;
            pcount++;
        }
        if (daemon_state.journal_sock_fd >= 0) {
            pfds[pcount].fd = daemon_state.journal_sock_fd;
            pfds[pcount].events = POLLIN;
            pfds[pcount].revents = 0;
            pcount++;
        }
        if (daemon_state.syslog_sock_fd >= 0) {
            pfds[pcount].fd = daemon_state.syslog_sock_fd;
            pfds[pcount].events = POLLIN;
            pfds[pcount].revents = 0;
            pcount++;
        }
        if (daemon_state.query_listen_fd >= 0) {
            pfds[pcount].fd = daemon_state.query_listen_fd;
            pfds[pcount].events = POLLIN;
            pfds[pcount].revents = 0;
            pcount++;
        }

        int query_start = pcount;
        for (int i = 0; i < MAX_CLIENTS; i++) {
            if (daemon_state.query_clients[i] >= 0) {
                pfds[pcount].fd = daemon_state.query_clients[i];
                pfds[pcount].events = POLLIN;
                pfds[pcount].revents = 0;
                pcount++;
            }
        }

        int ret = poll(pfds, pcount, 100);
        if (ret > 0) {
            int idx = 0;
            if (daemon_state.kmsg_fd >= 0) {
                if (pfds[idx++].revents & POLLIN) {
                    handle_kmsg_input();
                }
            }
            if (daemon_state.journal_sock_fd >= 0) {
                if (pfds[idx++].revents & POLLIN) {
                    handle_journal_native_input();
                }
            }
            if (daemon_state.syslog_sock_fd >= 0) {
                if (pfds[idx++].revents & POLLIN) {
                    handle_syslog_input();
                }
            }
            if (daemon_state.query_listen_fd >= 0) {
                if (pfds[idx++].revents & POLLIN) {
                    int cfd = accept(daemon_state.query_listen_fd, NULL, NULL);
                    if (cfd >= 0) {
                        int placed = 0;
                        for (int i = 0; i < MAX_CLIENTS; i++) {
                            if (daemon_state.query_clients[i] < 0) {
                                daemon_state.query_clients[i] = cfd;
                                daemon_state.query_follow[i] = 0;
                                daemon_state.client_count++;
                                placed = 1;
                                break;
                            }
                        }
                        if (!placed) {
                            close(cfd);
                        }
                    }
                }
            }

            for (int i = 0; i < MAX_CLIENTS; i++) {
                int cfd = daemon_state.query_clients[i];
                if (cfd >= 0 && query_start < pcount) {
                    if (pfds[query_start].revents & (POLLERR | POLLHUP)) {
                        close(cfd);
                        daemon_state.query_clients[i] = -1;
                        daemon_state.query_follow[i] = 0;
                        daemon_state.client_count--;
                    } else if (pfds[query_start].revents & POLLIN) {
                        char req[512];
                        ssize_t rn = read(cfd, req, sizeof(req) - 1);
                        if (rn <= 0) {
                            close(cfd);
                            daemon_state.query_clients[i] = -1;
                            daemon_state.query_follow[i] = 0;
                            daemon_state.client_count--;
                        } else {
                            req[rn] = 0;
                            handle_query_request(cfd, req);
                        }
                    }
                    query_start++;
                }
            }
        }
    }

    if (daemon_state.journal_fd >= 0) {
        close(daemon_state.journal_fd);
    }
    return 0;
}
