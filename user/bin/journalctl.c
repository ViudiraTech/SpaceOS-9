/*
 *
 *       user/bin/journalctl.c
 *       Query and follow systemd-journald logs
 *
 *       2026/11/06 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#include "splibc.h"
#include "journal.h"

struct filter_opts {
    char unit[64];
    int max_prio;
    int kernel_only;
    int follow;
    int lines;
    int reverse;
    char output_fmt[16];
    int disk_usage;
    int list_boots;
};

static const char *priority_names[] = {
    "emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"
};

static const char *priority_color(int prio) {
    switch (prio) {
    case 0: /* emerg */
    case 1: /* alert */
    case 2: /* crit */
    case 3: /* err */
        return "\033[1;31m"; /* red */
    case 4: /* warning */
        return "\033[1;33m"; /* yellow/amber */
    case 5: /* notice */
    case 6: /* info */
        return "\033[0m";    /* normal */
    case 7: /* debug */
    default:
        return "\033[90m";   /* dim gray */
    }
}

static int parse_priority_arg(const char *arg) {
    if (arg == NULL) {
        return 7;
    }
    if (arg[0] >= '0' && arg[0] <= '7' && arg[1] == 0) {
        return arg[0] - '0';
    }
    for (int i = 0; i < 8; i++) {
        if (strcmp(arg, priority_names[i]) == 0) {
            return i;
        }
    }
    return 7;
}

static void print_help(void) {
    printf("journalctl [OPTIONS...]\n\n"
           "Query and display system and service logs.\n\n"
           "Options:\n"
           "  -b, --boot                Show logs from the current boot\n"
           "  -k, --dmesg               Show kernel messages only\n"
           "  -u, --unit=UNIT           Show logs for the specified service unit\n"
           "  -p, --priority=PRIO       Show entries with specified priority (0-7 or err, warning, info...)\n"
           "  -f, --follow              Follow live log entries\n"
           "  -n, --lines=INTEGER       Number of journal entries to show (default: all or 10 when specified)\n"
           "  -r, --reverse             Show the newest entries first\n"
           "  -o, --output=STRING       Output format (short, verbose, json, cat)\n"
           "      --disk-usage          Show total disk usage of journal files\n"
           "      --list-boots          Show tabular information about all boots\n"
           "  -h, --help                Show this help message\n");
}

static void unescape_msg(char *msg) {
    char *r = msg;
    char *w = msg;
    while (*r != 0) {
        if (*r == '\\' && *(r + 1) == 'n') {
            *w++ = '\n';
            r += 2;
        } else if (*r == '\\' && *(r + 1) == 'r') {
            *w++ = '\r';
            r += 2;
        } else {
            *w++ = *r++;
        }
    }
    *w = 0;
}

static void format_and_print_entry(const char *output_fmt,
                                   u64 seq, u64 realtime_us, u64 mono_us,
                                   int prio, int fac, int transport,
                                   int pid, int uid, int gid,
                                   const char *unit, const char *comm,
                                   const char *ident, char *msg) {
    (void)fac;
    (void)transport;
    (void)gid;
    unescape_msg(msg);

    if (strcmp(output_fmt, "cat") == 0) {
        printf("%s\n", msg);
        return;
    }

    if (strcmp(output_fmt, "json") == 0) {
        printf("{\"__CURSOR\":\"%lu\",\"_SOURCE_REALTIME_TIMESTAMP\":\"%lu\","
               "\"_SOURCE_MONOTONIC_TIMESTAMP\":\"%lu\",\"PRIORITY\":%d,"
               "\"_PID\":%d,\"_UID\":%d,\"_SYSTEMD_UNIT\":\"%s\","
               "\"_COMM\":\"%s\",\"SYSLOG_IDENTIFIER\":\"%s\",\"MESSAGE\":\"%s\"}\n",
               (unsigned long)seq, (unsigned long)realtime_us,
               (unsigned long)mono_us, prio, pid, uid, unit, comm, ident, msg);
        return;
    }

    if (strcmp(output_fmt, "verbose") == 0) {
        u64 sec = mono_us / 1000000;
        u64 usec = mono_us % 1000000;
        printf("--- Entry %lu ---\n", (unsigned long)seq);
        printf("    _SOURCE_REALTIME_TIMESTAMP=%lu\n", (unsigned long)realtime_us);
        printf("    _SOURCE_MONOTONIC_TIMESTAMP=[%5lu.%06lu]\n", (unsigned long)sec, (unsigned long)usec);
        printf("    PRIORITY=%d (%s)\n", prio, priority_names[prio & 7]);
        printf("    _SYSTEMD_UNIT=%s\n", unit);
        printf("    _COMM=%s\n", comm);
        printf("    SYSLOG_IDENTIFIER=%s\n", ident);
        if (pid > 0) {
            printf("    _PID=%d\n", pid);
        }
        printf("    MESSAGE=%s\n", msg);
        return;
    }

    /* Default "short" output format */
    u64 sec = mono_us / 1000000;
    u64 usec = mono_us % 1000000;
    const char *color = priority_color(prio);

    const char *tag = (ident[0] != 0) ? ident : ((comm[0] != 0) ? comm : unit);
    if (pid > 0) {
        printf("[%5lu.%06lu] spaceos %s[%d]: %s%s\033[0m\n",
               (unsigned long)sec, (unsigned long)usec, tag, pid, color, msg);
    } else {
        printf("[%5lu.%06lu] spaceos %s: %s%s\033[0m\n",
               (unsigned long)sec, (unsigned long)usec, tag, color, msg);
    }
}

static int query_daemon(const struct filter_opts *opts) {
    int sfd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (sfd < 0) {
        return -1;
    }
    struct sockaddr_un sun;
    memset(&sun, 0, sizeof(sun));
    sun.sun_family = AF_UNIX;
    strncpy(sun.sun_path, JOURNAL_QUERY_PATH, sizeof(sun.sun_path) - 1);
    if (connect(sfd, &sun, sizeof(sun)) < 0) {
        close(sfd);
        return -1;
    }

    if (opts->disk_usage) {
        write(sfd, "STAT\n", 5);
        char resp[512] = {0};
        read(sfd, resp, sizeof(resp) - 1);
        char *sz = strstr(resp, "size=");
        if (sz != NULL) {
            u64 bytes = (u64)strtol(sz + 5, NULL, 10);
            printf("Archived and active journals take up %lu bytes on disk.\n", (unsigned long)bytes);
        }
        close(sfd);
        return 0;
    }

    if (opts->list_boots) {
        write(sfd, "STAT\n", 5);
        char resp[512] = {0};
        read(sfd, resp, sizeof(resp) - 1);
        char *boot = strstr(resp, "boot=");
        if (boot != NULL) {
            char bstr[33] = {0};
            strncpy(bstr, boot + 5, 32);
            printf("IDX BOOT ID                          FIRST ENTRY                 LAST ENTRY\n");
            printf("  0 %-32s current boot                now\n", bstr);
        }
        close(sfd);
        return 0;
    }

    if (opts->follow) {
        /* Request existing entries first, then follow */
        char cmd[256];
        snprintf(cmd, sizeof(cmd), "QUERY unit=%s prio=%d limit=%d\n",
                 opts->kernel_only ? "kernel" : opts->unit,
                 opts->max_prio,
                 (opts->lines > 0) ? opts->lines : 10);
        write(sfd, cmd, strlen(cmd));

        char line[2048];
        size_t lpos = 0;
        int in_follow_mode = 0;

        for (;;) {
            char c;
            ssize_t n = read(sfd, &c, 1);
            if (n <= 0) {
                break;
            }
            if (c == '\n') {
                line[lpos] = 0;
                lpos = 0;
                if (strcmp(line, "END") == 0 && !in_follow_mode) {
                    write(sfd, "FOLLOW\n", 7);
                    in_follow_mode = 1;
                    continue;
                }
                if (strncmp(line, "ENTRY|", 6) == 0) {
                    /* Parse entry */
                    char *p = line + 6;
                    char *parts[14];
                    int pcount = 0;
                    parts[pcount++] = p;
                    while (*p != 0 && pcount < 14) {
                        if (*p == '|') {
                            *p = 0;
                            parts[pcount++] = p + 1;
                        }
                        p++;
                    }
                    if (pcount >= 13) {
                        u64 seq = (u64)strtol(parts[0], NULL, 10);
                        u64 realtime_us = (u64)strtol(parts[1], NULL, 10);
                        u64 mono_us = (u64)strtol(parts[2], NULL, 10);
                        int prio = atoi(parts[3]);
                        int fac = atoi(parts[4]);
                        int transport = atoi(parts[5]);
                        int pid = atoi(parts[6]);
                        int uid = atoi(parts[7]);
                        int gid = atoi(parts[8]);
                        const char *unit = parts[9];
                        const char *comm = parts[10];
                        const char *ident = parts[11];
                        char *msg = parts[12];

                        format_and_print_entry(opts->output_fmt, seq, realtime_us,
                                               mono_us, prio, fac, transport,
                                               pid, uid, gid, unit, comm, ident, msg);
                    }
                }
            } else if (lpos + 1 < sizeof(line)) {
                line[lpos++] = c;
            }
        }
        close(sfd);
        return 0;
    }

    char cmd[256];
    snprintf(cmd, sizeof(cmd), "QUERY unit=%s prio=%d limit=%d %s\n",
             opts->kernel_only ? "kernel" : opts->unit,
             opts->max_prio,
             (opts->lines > 0) ? opts->lines : 2000,
             opts->reverse ? "reverse=1" : "");
    write(sfd, cmd, strlen(cmd));

    char line[2048];
    size_t lpos = 0;
    for (;;) {
        char c;
        ssize_t n = read(sfd, &c, 1);
        if (n <= 0) {
            break;
        }
        if (c == '\n') {
            line[lpos] = 0;
            lpos = 0;
            if (strcmp(line, "END") == 0) {
                break;
            }
            if (strncmp(line, "ENTRY|", 6) == 0) {
                char *p = line + 6;
                char *parts[14];
                int pcount = 0;
                parts[pcount++] = p;
                while (*p != 0 && pcount < 14) {
                    if (*p == '|') {
                        *p = 0;
                        parts[pcount++] = p + 1;
                    }
                    p++;
                }
                if (pcount >= 13) {
                    u64 seq = (u64)strtol(parts[0], NULL, 10);
                    u64 realtime_us = (u64)strtol(parts[1], NULL, 10);
                    u64 mono_us = (u64)strtol(parts[2], NULL, 10);
                    int prio = atoi(parts[3]);
                    int fac = atoi(parts[4]);
                    int transport = atoi(parts[5]);
                    int pid = atoi(parts[6]);
                    int uid = atoi(parts[7]);
                    int gid = atoi(parts[8]);
                    const char *unit = parts[9];
                    const char *comm = parts[10];
                    const char *ident = parts[11];
                    char *msg = parts[12];

                    format_and_print_entry(opts->output_fmt, seq, realtime_us,
                                           mono_us, prio, fac, transport,
                                           pid, uid, gid, unit, comm, ident, msg);
                }
            }
        } else if (lpos + 1 < sizeof(line)) {
            line[lpos++] = c;
        }
    }

    close(sfd);
    return 0;
}

/* Fallback: Direct read from /dev/kmsg */
static int read_kmsg_fallback(const struct filter_opts *opts) {
    int fd = open("/dev/kmsg", opts->follow ? O_RDONLY : (O_RDONLY | O_NONBLOCK));
    if (fd < 0) {
        fprintf(2, "Failed to open /dev/kmsg: No such file or directory\n");
        return 1;
    }

    char buf[2048];
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        if (n <= 0) {
            if (opts->follow) {
                usleep(50000);
                continue;
            }
            break;
        }
        buf[n] = 0;

        char *comma1 = strchr(buf, ',');
        if (comma1 == NULL) continue;
        *comma1 = 0;
        int pnum = atoi(buf);
        int prio = pnum & 7;
        if (prio > opts->max_prio) {
            continue;
        }

        char *comma2 = strchr(comma1 + 1, ',');
        if (comma2 == NULL) continue;
        char *comma3 = strchr(comma2 + 1, ',');
        if (comma3 == NULL) continue;
        *comma3 = 0;
        u64 seq = (u64)strtol(comma1 + 1, NULL, 10);
        u64 usec = (u64)strtol(comma2 + 1, NULL, 10);

        char *semicolon = strchr(comma3 + 1, ';');
        if (semicolon == NULL) continue;
        char *msg = semicolon + 1;
        size_t mlen = strlen(msg);
        while (mlen > 0 && (msg[mlen - 1] == '\n' || msg[mlen - 1] == '\r')) {
            msg[--mlen] = 0;
        }

        format_and_print_entry(opts->output_fmt, seq, 0, usec, prio, 0,
                               TRANSPORT_KERNEL, 0, 0, 0,
                               "kernel", "kernel", "kernel", msg);
    }
    close(fd);
    return 0;
}

/* Fallback: Direct read from journal binary file */
static int read_file_fallback(const struct filter_opts *opts) {
    const char *path = JOURNAL_FILE_SYSTEM;
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        path = JOURNAL_FILE_VOLATILE;
        fd = open(path, O_RDONLY);
    }
    if (fd < 0) {
        return read_kmsg_fallback(opts);
    }

    struct journal_header hdr;
    if (read(fd, &hdr, sizeof(hdr)) != (ssize_t)sizeof(hdr) ||
        hdr.magic != JOURNAL_HEADER_MAGIC) {
        close(fd);
        return read_kmsg_fallback(opts);
    }

    if (opts->disk_usage) {
        struct stat st;
        fstat(fd, &st);
        printf("Archived and active journals take up %lu bytes on disk.\n", (unsigned long)st.st_size);
        close(fd);
        return 0;
    }

    if (opts->list_boots) {
        printf("IDX BOOT ID                          FIRST ENTRY                 LAST ENTRY\n");
        printf("  0 %-32s current boot                now\n", hdr.boot_id);
        close(fd);
        return 0;
    }

    for (;;) {
        struct journal_entry_hdr ehdr;
        ssize_t n = read(fd, &ehdr, sizeof(ehdr));
        if (n != (ssize_t)sizeof(ehdr)) {
            break;
        }
        if (ehdr.magic != JOURNAL_ENTRY_MAGIC) {
            break;
        }

        char payload[MAX_PAYLOAD_SIZE];
        if (ehdr.payload_size > sizeof(payload) - 1) {
            lseek(fd, ehdr.payload_size, SEEK_CUR);
            continue;
        }
        if (read(fd, payload, ehdr.payload_size) != (ssize_t)ehdr.payload_size) {
            break;
        }

        char *unit = payload;
        char *comm = unit + ehdr.unit_len + 1;
        char *ident = comm + ehdr.comm_len + 1;
        char *msg = ident + ehdr.ident_len + 1;

        if (ehdr.priority > opts->max_prio) {
            continue;
        }
        if (opts->kernel_only && ehdr.transport != TRANSPORT_KERNEL) {
            continue;
        }
        if (opts->unit[0] != 0 && strstr(unit, opts->unit) == NULL) {
            continue;
        }

        format_and_print_entry(opts->output_fmt, ehdr.seq_num,
                               ehdr.realtime_usec, ehdr.monotonic_usec,
                               ehdr.priority, ehdr.facility, ehdr.transport,
                               ehdr.pid, ehdr.uid, ehdr.gid,
                               unit, comm, ident, msg);
    }

    close(fd);
    return 0;
}

int main(int argc, char **argv) {
    struct filter_opts opts;
    memset(&opts, 0, sizeof(opts));
    opts.max_prio = 7;
    opts.lines = 0;
    strcpy(opts.output_fmt, "short");

    for (int i = 1; i < argc; i++) {
        const char *arg = argv[i];
        if (strcmp(arg, "-h") == 0 || strcmp(arg, "--help") == 0) {
            print_help();
            return 0;
        }
        if (strcmp(arg, "-b") == 0 || strcmp(arg, "--boot") == 0) {
            /* Current boot */
            continue;
        }
        if (strcmp(arg, "-k") == 0 || strcmp(arg, "--dmesg") == 0) {
            opts.kernel_only = 1;
            continue;
        }
        if (strcmp(arg, "-f") == 0 || strcmp(arg, "--follow") == 0) {
            opts.follow = 1;
            continue;
        }
        if (strcmp(arg, "-r") == 0 || strcmp(arg, "--reverse") == 0) {
            opts.reverse = 1;
            continue;
        }
        if (strcmp(arg, "--no-pager") == 0) {
            continue;
        }
        if (strcmp(arg, "--disk-usage") == 0) {
            opts.disk_usage = 1;
            continue;
        }
        if (strcmp(arg, "--list-boots") == 0) {
            opts.list_boots = 1;
            continue;
        }
        if (strncmp(arg, "-u", 2) == 0) {
            const char *val = (arg[2] != 0) ? (arg + 2) : ((i + 1 < argc) ? argv[++i] : "");
            strncpy(opts.unit, val, sizeof(opts.unit) - 1);
            continue;
        }
        if (strncmp(arg, "--unit=", 7) == 0) {
            strncpy(opts.unit, arg + 7, sizeof(opts.unit) - 1);
            continue;
        }
        if (strncmp(arg, "-p", 2) == 0) {
            const char *val = (arg[2] != 0) ? (arg + 2) : ((i + 1 < argc) ? argv[++i] : "");
            opts.max_prio = parse_priority_arg(val);
            continue;
        }
        if (strncmp(arg, "--priority=", 11) == 0) {
            opts.max_prio = parse_priority_arg(arg + 11);
            continue;
        }
        if (strncmp(arg, "-n", 2) == 0) {
            const char *val = (arg[2] != 0) ? (arg + 2) : ((i + 1 < argc) ? argv[++i] : "10");
            opts.lines = atoi(val);
            continue;
        }
        if (strncmp(arg, "--lines=", 8) == 0) {
            opts.lines = atoi(arg + 8);
            continue;
        }
        if (strncmp(arg, "-o", 2) == 0) {
            const char *val = (arg[2] != 0) ? (arg + 2) : ((i + 1 < argc) ? argv[++i] : "short");
            strncpy(opts.output_fmt, val, sizeof(opts.output_fmt) - 1);
            continue;
        }
        if (strncmp(arg, "--output=", 9) == 0) {
            strncpy(opts.output_fmt, arg + 9, sizeof(opts.output_fmt) - 1);
            continue;
        }

        /* If looks like a unit name */
        if (arg[0] != '-' && opts.unit[0] == 0) {
            strncpy(opts.unit, arg, sizeof(opts.unit) - 1);
            continue;
        }
    }

    if (query_daemon(&opts) == 0) {
        return 0;
    }

    /* Daemon not answering, fallback to direct journal file or /dev/kmsg */
    return read_file_fallback(&opts);
}
