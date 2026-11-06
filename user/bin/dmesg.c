/*
 *
 *       user/bin/dmesg.c
 *       Read and print the kernel ring buffer via /dev/kmsg
 *
 *       2026/11/06 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#include "splibc.h"

static void print_help(void) {
    printf("dmesg [options]\n\n"
           "Display or control the kernel ring buffer.\n\n"
           "Options:\n"
           "  -w, --follow             Wait for new messages\n"
           "  -r, --raw                Print the raw message buffer\n"
           "  -l, --level <level>      Restrict output to the given level (emerg, alert, crit, err, warn, notice, info, debug)\n"
           "  -h, --help               Display this help\n");
}

static const char *level_names[] = {
    "emerg", "alert", "crit", "err", "warn", "notice", "info", "debug"
};

static int parse_level(const char *arg) {
    if (arg == NULL) return 7;
    if (arg[0] >= '0' && arg[0] <= '7' && arg[1] == 0) return arg[0] - '0';
    for (int i = 0; i < 8; i++) {
        if (strcmp(arg, level_names[i]) == 0) return i;
    }
    return 7;
}

int main(int argc, char **argv) {
    int follow = 0;
    int raw = 0;
    int max_level = 7;

    for (int i = 1; i < argc; i++) {
        const char *arg = argv[i];
        if (strcmp(arg, "-h") == 0 || strcmp(arg, "--help") == 0) {
            print_help();
            return 0;
        }
        if (strcmp(arg, "-w") == 0 || strcmp(arg, "--follow") == 0) {
            follow = 1;
            continue;
        }
        if (strcmp(arg, "-r") == 0 || strcmp(arg, "--raw") == 0) {
            raw = 1;
            continue;
        }
        if (strcmp(arg, "-l") == 0 && i + 1 < argc) {
            max_level = parse_level(argv[++i]);
            continue;
        }
    }

    int fd = open("/dev/kmsg", follow ? O_RDONLY : (O_RDONLY | O_NONBLOCK));
    if (fd < 0) {
        fprintf(2, "dmesg: read kernel buffer failed: No such file or directory\n");
        return 1;
    }

    char buf[2048];
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        if (n <= 0) {
            if (follow) {
                usleep(50000);
                continue;
            }
            break;
        }
        buf[n] = 0;

        if (raw) {
            printf("%s", buf);
            continue;
        }

        /* Format: "<prio>,<seq>,<usec>,<flags>;<msg>\n" */
        char *comma1 = strchr(buf, ',');
        if (comma1 == NULL) continue;
        *comma1 = 0;
        int p = atoi(buf) & 7;
        if (p > max_level) continue;

        char *comma2 = strchr(comma1 + 1, ',');
        if (comma2 == NULL) continue;
        char *comma3 = strchr(comma2 + 1, ',');
        if (comma3 == NULL) continue;
        *comma3 = 0;
        u64 usec = (u64)strtol(comma2 + 1, NULL, 10);

        char *semicolon = strchr(comma3 + 1, ';');
        if (semicolon == NULL) continue;
        char *msg = semicolon + 1;

        u64 sec = usec / 1000000;
        u64 mic = usec % 1000000;

        const char *color = "";
        if (p <= 3) {
            color = "\033[1;31m"; /* red */
        } else if (p == 4) {
            color = "\033[1;33m"; /* yellow */
        }

        printf("[%5lu.%06lu] %s%s\033[0m", (unsigned long)sec, (unsigned long)mic, color, msg);
    }

    close(fd);
    return 0;
}
