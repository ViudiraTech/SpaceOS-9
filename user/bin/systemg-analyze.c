/*
 *
 *       user/bin/systemg-analyze.c
 *       Profile and analyze systemg system and service manager performance
 *
 *       2026/11/05 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#include "splibc.h"

#define SOCK_PATH "/run/systemg.sock"
#define MAX_ANALYZE_UNITS 64
#define MAX_DEPS 8

struct analyze_unit {
    char name[64];
    char type[32];
    char state[32];
    char substate[32];
    int pid;
    u64 queue_ts;
    u64 start_ts;
    u64 active_ts;
    u64 duration_us;
    char after[MAX_DEPS][64];
    int after_count;
    char requires[MAX_DEPS][64];
    int requires_count;
    char wants[MAX_DEPS][64];
    int wants_count;
    char desc[96];
};

static u64 g_kernel_usec = 0;
static u64 g_userspace_start_usec = 0;
static u64 g_userspace_finish_usec = 0;
static struct analyze_unit g_units[MAX_ANALYZE_UNITS];
static int g_unit_count = 0;

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

static u64 parse_u64(const char *s) {
    u64 val = 0;
    while (*s >= '0' && *s <= '9') {
        val = val * 10 + (u64)(*s - '0');
        s++;
    }
    return val;
}

static int string_ends_with(const char *str, const char *suffix) {
    size_t str_len = strlen(str);
    size_t suf_len = strlen(suffix);
    if (str_len < suf_len) {
        return 0;
    }
    return strcmp(str + str_len - suf_len, suffix) == 0;
}

static void strip_service_suffix(char *name) {
    size_t len = strlen(name);
    if (len > 8 && strcmp(name + len - 8, ".service") == 0) {
        name[len - 8] = 0;
    }
}

static int is_target_name(const char *name) {
    return strcmp(name, "graphical.target") == 0 ||
           strcmp(name, "multi-user.target") == 0 ||
           strcmp(name, "default.target") == 0;
}

static int connect_systemg(void) {
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) {
        return -1;
    }
    const char *sock = getenv("SYSTEMG_SOCK");
    if (sock == NULL || *sock == 0) {
        sock = SOCK_PATH;
    }
    struct sockaddr_un sun;
    memset(&sun, 0, sizeof(sun));
    sun.sun_family = AF_UNIX;
    strncpy(sun.sun_path, sock, sizeof(sun.sun_path) - 1);
    if (connect(fd, &sun, sizeof(sun)) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int send_command(const char *cmd, const char *arg, char **out_buf, size_t *out_len) {
    int fd = connect_systemg();
    if (fd < 0) {
        fprintf(2, "Failed to connect to bus: No such file or directory\n");
        return -1;
    }
    char req[512];
    if (arg != NULL && *arg != 0) {
        snprintf(req, sizeof(req), "%s %s\n", cmd, arg);
    } else {
        snprintf(req, sizeof(req), "%s\n", cmd);
    }
    write(fd, req, strlen(req));

    size_t capacity = 32768;
    char *buffer = (char *)malloc(capacity);
    if (buffer == NULL) {
        close(fd);
        return -1;
    }

    size_t total = 0;
    for (;;) {
        if (total + 1024 >= capacity) {
            capacity *= 2;
            char *new_buf = (char *)realloc(buffer, capacity);
            if (new_buf == NULL) {
                free(buffer);
                close(fd);
                return -1;
            }
            buffer = new_buf;
        }
        ssize_t n = read(fd, buffer + total, capacity - total - 1);
        if (n <= 0) {
            break;
        }
        total += (size_t)n;
    }
    buffer[total] = 0;
    close(fd);

    *out_buf = buffer;
    if (out_len != NULL) {
        *out_len = total;
    }
    return 0;
}

static void parse_csv_deps(char list[MAX_DEPS][64], int *count, const char *csv) {
    *count = 0;
    if (csv == NULL || *csv == 0) {
        return;
    }
    char buf[256];
    strncpy(buf, csv, sizeof(buf) - 1);
    buf[sizeof(buf) - 1] = 0;

    char *token = buf;
    while (token != NULL && *token != 0 && *count < MAX_DEPS) {
        char *comma = strchr(token, ',');
        if (comma != NULL) {
            *comma = 0;
        }
        char *item = trim(token);
        if (*item != 0) {
            strip_service_suffix(item);
            strncpy(list[*count], item, 63);
            list[*count][63] = 0;
            (*count)++;
        }
        token = comma ? comma + 1 : NULL;
    }
}

static int fetch_timings(void) {
    if (g_unit_count > 0) {
        return 0;
    }
    char *resp = NULL;
    size_t len = 0;
    if (send_command("TIMINGS", NULL, &resp, &len) < 0) {
        return -1;
    }

    char *line = resp;
    while (line != NULL && *line != 0) {
        char *next = strchr(line, '\n');
        if (next != NULL) {
            *next = 0;
            next++;
        }
        line = trim(line);
        if (strncmp(line, "KERNEL_USEC:", 12) == 0) {
            g_kernel_usec = parse_u64(line + 12);
        } else if (strncmp(line, "USERSPACE_START_USEC:", 21) == 0) {
            g_userspace_start_usec = parse_u64(line + 21);
        } else if (strncmp(line, "USERSPACE_FINISH_USEC:", 22) == 0) {
            g_userspace_finish_usec = parse_u64(line + 22);
        } else if (strncmp(line, "UNIT:", 5) == 0) {
            if (g_unit_count < MAX_ANALYZE_UNITS) {
                struct analyze_unit *u = &g_units[g_unit_count];
                memset(u, 0, sizeof(*u));
                char *p = line + 5;
                char *parts[13];
                int part_idx = 0;
                while (p != NULL && part_idx < 13) {
                    parts[part_idx++] = p;
                    char *pipe = strchr(p, '|');
                    if (pipe != NULL) {
                        *pipe = 0;
                        p = pipe + 1;
                    } else {
                        p = NULL;
                    }
                }
                if (part_idx >= 13) {
                    strncpy(u->name, parts[0], sizeof(u->name) - 1);
                    strncpy(u->type, parts[1], sizeof(u->type) - 1);
                    strncpy(u->state, parts[2], sizeof(u->state) - 1);
                    strncpy(u->substate, parts[3], sizeof(u->substate) - 1);
                    u->pid = atoi(parts[4]);
                    u->queue_ts = parse_u64(parts[5]);
                    u->start_ts = parse_u64(parts[6]);
                    u->active_ts = parse_u64(parts[7]);
                    u->duration_us = parse_u64(parts[8]);
                    parse_csv_deps(u->after, &u->after_count, parts[9]);
                    parse_csv_deps(u->requires, &u->requires_count, parts[10]);
                    parse_csv_deps(u->wants, &u->wants_count, parts[11]);
                    strncpy(u->desc, parts[12], sizeof(u->desc) - 1);
                    g_unit_count++;
                }
            }
        }
        line = next;
    }

    free(resp);
    return 0;
}

static void format_time_rel(char *buf, size_t size, u64 ts_us) {
    u64 rel_us = (ts_us >= g_userspace_start_usec) ? (ts_us - g_userspace_start_usec) : ts_us;
    if (rel_us >= 1000000ULL) {
        u64 ms = rel_us / 1000;
        snprintf(buf, size, "%lu.%03lus", (unsigned long)(ms / 1000), (unsigned long)(ms % 1000));
    } else if (rel_us >= 1000) {
        snprintf(buf, size, "%lums", (unsigned long)(rel_us / 1000));
    } else {
        snprintf(buf, size, "%luus", (unsigned long)rel_us);
    }
}

static void format_duration(char *buf, size_t size, u64 dur_us) {
    if (dur_us >= 1000000ULL) {
        u64 ms = dur_us / 1000;
        snprintf(buf, size, "%lu.%03lus", (unsigned long)(ms / 1000), (unsigned long)(ms % 1000));
    } else if (dur_us >= 1000) {
        snprintf(buf, size, "%lums", (unsigned long)(dur_us / 1000));
    } else {
        snprintf(buf, size, "%luus", (unsigned long)dur_us);
    }
}

static int find_unit(const char *name) {
    char clean_name[64];
    strncpy(clean_name, name, sizeof(clean_name) - 1);
    clean_name[sizeof(clean_name) - 1] = 0;
    strip_service_suffix(clean_name);
    for (int i = 0; i < g_unit_count; i++) {
        if (strcmp(g_units[i].name, clean_name) == 0) {
            return i;
        }
    }
    return -1;
}

static int cmd_time(void) {
    if (fetch_timings() < 0) {
        return 1;
    }
    u64 k_ms = g_kernel_usec / 1000;
    u64 u_ms = (g_userspace_finish_usec > g_userspace_start_usec)
                   ? (g_userspace_finish_usec - g_userspace_start_usec) / 1000
                   : 0;
    u64 tot_ms = k_ms + u_ms;

    printf("Startup finished in %lu.%03lus (kernel) + %lu.%03lus (userspace) = %lu.%03lus\n",
           (unsigned long)(k_ms / 1000), (unsigned long)(k_ms % 1000),
           (unsigned long)(u_ms / 1000), (unsigned long)(u_ms % 1000),
           (unsigned long)(tot_ms / 1000), (unsigned long)(tot_ms % 1000));

    int has_ddm = 0;
    for (int i = 0; i < g_unit_count; i++) {
        if (strcmp(g_units[i].name, "ddm") == 0 && strcmp(g_units[i].state, "active") == 0) {
            has_ddm = 1;
            break;
        }
    }
    const char *target = has_ddm ? "graphical.target" : "multi-user.target";
    printf("%s reached after %lu.%03lus in userspace.\n",
           target, (unsigned long)(u_ms / 1000), (unsigned long)(u_ms % 1000));
    return 0;
}

static int cmd_blame(void) {
    if (fetch_timings() < 0) {
        return 1;
    }

    struct analyze_unit *sorted[MAX_ANALYZE_UNITS];
    int count = 0;
    for (int i = 0; i < g_unit_count; i++) {
        if (g_units[i].duration_us > 0 || g_units[i].active_ts > 0 || strcmp(g_units[i].state, "active") == 0) {
            sorted[count++] = &g_units[i];
        }
    }

    for (int i = 0; i < count - 1; i++) {
        for (int j = 0; j < count - 1 - i; j++) {
            if (sorted[j]->duration_us < sorted[j + 1]->duration_us) {
                struct analyze_unit *tmp = sorted[j];
                sorted[j] = sorted[j + 1];
                sorted[j + 1] = tmp;
            }
        }
    }

    for (int i = 0; i < count; i++) {
        struct analyze_unit *u = sorted[i];
        char dur_buf[32];
        format_duration(dur_buf, sizeof(dur_buf), u->duration_us);
        printf("%8s %s.service\n", dur_buf, u->name);
    }
    return 0;
}

static int trace_critical_chain_single(const char *target_or_unit) {
    int curr_idx = -1;
    int depth = 0;

    if (target_or_unit == NULL || is_target_name(target_or_unit)) {
        int has_ddm = 0;
        int latest_idx = -1;
        u64 latest_ts = 0;

        for (int i = 0; i < g_unit_count; i++) {
            if (strcmp(g_units[i].name, "ddm") == 0 && strcmp(g_units[i].state, "active") == 0) {
                has_ddm = 1;
            }
            if (g_units[i].active_ts > latest_ts) {
                latest_ts = g_units[i].active_ts;
                latest_idx = i;
            }
        }

        const char *target_name;
        if (target_or_unit != NULL && strcmp(target_or_unit, "default.target") != 0) {
            target_name = target_or_unit;
        } else {
            target_name = has_ddm ? "graphical.target" : "multi-user.target";
        }

        char target_time[32];
        format_time_rel(target_time, sizeof(target_time), g_userspace_finish_usec);
        printf("%s @%s\n", target_name, target_time);

        curr_idx = latest_idx;
        depth = 1;
    } else {
        curr_idx = find_unit(target_or_unit);
        if (curr_idx < 0) {
            fprintf(2, "Unit %s not found.\n", target_or_unit);
            return 1;
        }
        depth = 0;
    }

    char visited[MAX_ANALYZE_UNITS];
    memset(visited, 0, sizeof(visited));

    while (curr_idx >= 0 && curr_idx < g_unit_count) {
        if (visited[curr_idx]) {
            break;
        }
        visited[curr_idx] = 1;

        struct analyze_unit *u = &g_units[curr_idx];
        char at_str[32];
        char dur_str[32];
        format_time_rel(at_str, sizeof(at_str), u->active_ts);
        format_duration(dur_str, sizeof(dur_str), u->duration_us);

        if (depth > 0) {
            for (int s = 0; s < (depth - 1) * 2; s++) {
                putchar(' ');
            }
            printf("└─");
        }
        if (u->duration_us > 0) {
            printf("%s.service @%s +%s\n", u->name, at_str, dur_str);
        } else {
            printf("%s.service @%s\n", u->name, at_str);
        }

        int next_idx = -1;
        u64 max_dep_ts = 0;

        for (int d = 0; d < u->after_count; d++) {
            int idx = find_unit(u->after[d]);
            if (idx >= 0 && !visited[idx] && g_units[idx].active_ts > 0) {
                if (g_units[idx].active_ts > max_dep_ts) {
                    max_dep_ts = g_units[idx].active_ts;
                    next_idx = idx;
                }
            }
        }
        for (int d = 0; d < u->requires_count; d++) {
            int idx = find_unit(u->requires[d]);
            if (idx >= 0 && !visited[idx] && g_units[idx].active_ts > 0) {
                if (g_units[idx].active_ts > max_dep_ts) {
                    max_dep_ts = g_units[idx].active_ts;
                    next_idx = idx;
                }
            }
        }

        curr_idx = next_idx;
        depth++;
    }

    return 0;
}

static int cmd_critical_chain(int argc, char **argv) {
    if (fetch_timings() < 0) {
        return 1;
    }

    printf("The time when unit became active or started is printed after the \"@\" character.\n");
    printf("The time the unit took to start is printed after the \"+\" character.\n\n");

    if (argc == 0) {
        return trace_critical_chain_single(NULL);
    }

    int failures = 0;
    for (int i = 0; i < argc; i++) {
        if (trace_critical_chain_single(argv[i]) != 0) {
            failures++;
        }
    }
    return failures > 0 ? 1 : 0;
}

static int cmd_plot(void) {
    if (fetch_timings() < 0) {
        return 1;
    }

    u64 k_ms = g_kernel_usec / 1000;
    u64 u_ms = (g_userspace_finish_usec > g_userspace_start_usec)
                   ? (g_userspace_finish_usec - g_userspace_start_usec) / 1000
                   : 0;

    u64 max_finish_usec = g_userspace_finish_usec;
    for (int i = 0; i < g_unit_count; i++) {
        if (g_units[i].active_ts > max_finish_usec) {
            max_finish_usec = g_units[i].active_ts;
        }
        if (g_units[i].start_ts + g_units[i].duration_us > max_finish_usec) {
            max_finish_usec = g_units[i].start_ts + g_units[i].duration_us;
        }
    }
    u64 total_time_ms = (max_finish_usec + 999) / 1000;
    if (total_time_ms < 100) {
        total_time_ms = 100;
    }

    int width = 1000;
    int margin_left = 220;
    int plot_width = 730;
    int row_height = 28;
    int margin_top = 110;
    int height = margin_top + (g_unit_count + 1) * row_height + 40;

    printf("<?xml version=\"1.0\" standalone=\"no\"?>\n");
    printf("<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n");
    printf("<svg width=\"%d\" height=\"%d\" viewBox=\"0 0 %d %d\" xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\">\n",
           width, height, width, height);
    printf("  <rect width=\"100%%\" height=\"100%%\" fill=\"#ffffff\"/>\n");

    /* Header text */
    printf("  <text x=\"20\" y=\"32\" font-family=\"DejaVu Sans, Arial, sans-serif\" font-size=\"16\" font-weight=\"bold\" fill=\"#111827\">systemg-analyze plot</text>\n");
    printf("  <text x=\"20\" y=\"56\" font-family=\"DejaVu Sans, Arial, sans-serif\" font-size=\"12\" fill=\"#4b5563\">Startup finished in %lu.%03lus (kernel) + %lu.%03lus (userspace) = %lu.%03lus</text>\n",
           (unsigned long)(k_ms / 1000), (unsigned long)(k_ms % 1000),
           (unsigned long)(u_ms / 1000), (unsigned long)(u_ms % 1000),
           (unsigned long)(total_time_ms / 1000), (unsigned long)(total_time_ms % 1000));

    /* Time grid */
    u64 step_ms = total_time_ms / 8;
    if (step_ms < 100) {
        step_ms = 100;
    } else if (step_ms < 250) {
        step_ms = 200;
    } else if (step_ms < 750) {
        step_ms = 500;
    } else if (step_ms < 1500) {
        step_ms = 1000;
    } else if (step_ms < 3500) {
        step_ms = 2000;
    } else if (step_ms < 7500) {
        step_ms = 5000;
    } else if (step_ms < 20000) {
        step_ms = 10000;
    } else {
        step_ms = (step_ms / 10000) * 10000;
        if (step_ms == 0) {
            step_ms = 10000;
        }
    }

    for (u64 tick_ms = 0; tick_ms <= total_time_ms; tick_ms += step_ms) {
        int tick_x = margin_left + (int)((tick_ms * (u64)plot_width) / total_time_ms);
        printf("  <line x1=\"%d\" y1=\"85\" x2=\"%d\" y2=\"%d\" stroke=\"#e5e7eb\" stroke-width=\"1\" stroke-dasharray=\"2,2\"/>\n",
               tick_x, tick_x, height - 30);
        printf("  <text x=\"%d\" y=\"80\" text-anchor=\"middle\" font-family=\"DejaVu Sans, Arial, sans-serif\" font-size=\"10\" fill=\"#6b7280\">%lu.%01lus</text>\n",
               tick_x, (unsigned long)(tick_ms / 1000), (unsigned long)((tick_ms % 1000) / 100));
    }
    printf("  <line x1=\"%d\" y1=\"85\" x2=\"%d\" y2=\"85\" stroke=\"#9ca3af\" stroke-width=\"1.5\"/>\n",
           margin_left, margin_left + plot_width);

    /* Row 0: Kernel */
    int row_y = margin_top;
    printf("  <text x=\"%d\" y=\"%d\" text-anchor=\"end\" font-family=\"monospace\" font-size=\"11\" font-weight=\"bold\" fill=\"#374151\">kernel</text>\n",
           margin_left - 10, row_y + 12);
    int k_bar_w = (int)((k_ms * (u64)plot_width) / total_time_ms);
    if (k_bar_w < 6) {
        k_bar_w = 6;
    }
    printf("  <rect x=\"%d\" y=\"%d\" width=\"%d\" height=\"16\" rx=\"3\" fill=\"#64748b\"/>\n",
           margin_left, row_y, k_bar_w);
    printf("  <text x=\"%d\" y=\"%d\" font-family=\"DejaVu Sans, Arial, sans-serif\" font-size=\"10\" fill=\"#ffffff\">%lu.%03lus</text>\n",
           margin_left + 6, row_y + 12, (unsigned long)(k_ms / 1000), (unsigned long)(k_ms % 1000));

    /* Rows 1..N: Services */
    for (int i = 0; i < g_unit_count; i++) {
        struct analyze_unit *u = &g_units[i];
        row_y += row_height;

        printf("  <text x=\"%d\" y=\"%d\" text-anchor=\"end\" font-family=\"monospace\" font-size=\"11\" fill=\"#374151\">%s.service</text>\n",
               margin_left - 10, row_y + 12, u->name);

        u64 start_ms = (u->start_ts > 0) ? (u->start_ts / 1000) : k_ms;
        u64 dur_ms = (u->duration_us + 999) / 1000;
        int bar_x = margin_left + (int)((start_ms * (u64)plot_width) / total_time_ms);
        int bar_w = (int)((dur_ms * (u64)plot_width) / total_time_ms);
        if (bar_w < 6) {
            bar_w = 6;
        }

        const char *bar_color;
        if (strcmp(u->state, "failed") == 0) {
            bar_color = "#ef4444";
        } else if (strcmp(u->type, "oneshot") == 0) {
            bar_color = "#10b981";
        } else {
            bar_color = "#3b82f6";
        }

        printf("  <rect x=\"%d\" y=\"%d\" width=\"%d\" height=\"16\" rx=\"3\" fill=\"%s\" opacity=\"0.9\"/>\n",
               bar_x, row_y, bar_w, bar_color);

        char dur_buf[32];
        format_duration(dur_buf, sizeof(dur_buf), u->duration_us);
        printf("  <text x=\"%d\" y=\"%d\" font-family=\"DejaVu Sans, Arial, sans-serif\" font-size=\"10\" fill=\"#1f2937\">+%s</text>\n",
               bar_x + bar_w + 6, row_y + 12, dur_buf);
    }

    printf("</svg>\n");
    return 0;
}

static int cmd_dot(int argc, char **argv) {
    if (fetch_timings() < 0) {
        return 1;
    }

    char clean_filter[64];
    clean_filter[0] = 0;
    if (argc >= 1 && argv[0] != NULL && argv[0][0] != 0) {
        strncpy(clean_filter, argv[0], sizeof(clean_filter) - 1);
        clean_filter[sizeof(clean_filter) - 1] = 0;
        strip_service_suffix(clean_filter);
    }

    printf("digraph systemg {\n");
    printf("  rankdir=LR;\n");
    printf("  bgcolor=\"#ffffff\";\n");
    printf("  node [shape=box, style=\"filled,rounded\", fontname=\"DejaVu Sans, sans-serif\", fontsize=10, fillcolor=\"#f3f4f6\", color=\"#9ca3af\"];\n");
    printf("  edge [fontname=\"DejaVu Sans, sans-serif\", fontsize=9];\n\n");

    for (int i = 0; i < g_unit_count; i++) {
        struct analyze_unit *u = &g_units[i];
        if (clean_filter[0] != 0 && strstr(u->name, clean_filter) == NULL) {
            continue;
        }

        const char *fill = "#f3f4f6";
        const char *stroke = "#9ca3af";
        if (strcmp(u->state, "active") == 0) {
            fill = "#dcfce7";
            stroke = "#22c55e";
        } else if (strcmp(u->state, "failed") == 0) {
            fill = "#fee2e2";
            stroke = "#ef4444";
        }
        printf("  \"%s.service\" [label=\"%s.service\\n(%s)\", fillcolor=\"%s\", color=\"%s\"];\n",
               u->name, u->name, u->state, fill, stroke);
    }
    printf("\n");

    for (int i = 0; i < g_unit_count; i++) {
        struct analyze_unit *u = &g_units[i];
        if (clean_filter[0] != 0 && strstr(u->name, clean_filter) == NULL) {
            continue;
        }

        for (int d = 0; d < u->after_count; d++) {
            printf("  \"%s.service\" -> \"%s.service\" [label=\"After\", color=\"#3b82f6\", style=\"solid\"];\n",
                   u->name, u->after[d]);
        }
        for (int d = 0; d < u->requires_count; d++) {
            printf("  \"%s.service\" -> \"%s.service\" [label=\"Requires\", color=\"#ef4444\", style=\"dashed\"];\n",
                   u->name, u->requires[d]);
        }
        for (int d = 0; d < u->wants_count; d++) {
            printf("  \"%s.service\" -> \"%s.service\" [label=\"Wants\", color=\"#8b5cf6\", style=\"dotted\"];\n",
                   u->name, u->wants[d]);
        }
    }

    printf("}\n");
    return 0;
}

static int cmd_dump(int argc, char **argv) {
    const char *arg = (argc >= 1) ? argv[0] : NULL;
    char *resp = NULL;
    size_t len = 0;
    if (send_command("DUMP", arg, &resp, &len) < 0) {
        return 1;
    }
    if (resp != NULL) {
        write(1, resp, len);
        free(resp);
    }
    return 0;
}

static int cmd_unit_paths(void) {
    const char *custom = getenv("SPACEOS_SERVICES");
    if (custom != NULL && *custom != 0) {
        printf("%s\n", custom);
    }
    printf("/etc/systemg\n"
           "/run/systemg\n"
           "/usr/lib/systemg\n"
           "/lib/systemg\n");
    return 0;
}

static int cmd_cat_config(int argc, char **argv) {
    if (argc < 1) {
        fprintf(2, "Too few arguments. Specify unit file name or path.\n");
        return 1;
    }
    int failures = 0;
    for (int i = 0; i < argc; i++) {
        const char *name = argv[i];
        char path[256];
        if (strchr(name, '/') != NULL) {
            strncpy(path, name, sizeof(path) - 1);
            path[sizeof(path) - 1] = 0;
        } else {
            char base[64];
            strncpy(base, name, sizeof(base) - 1);
            base[sizeof(base) - 1] = 0;
            if (!string_ends_with(base, ".service") && !string_ends_with(base, ".target")) {
                strcat(base, ".service");
            }
            const char *env_dir = getenv("SPACEOS_SERVICES");
            if (env_dir != NULL && *env_dir != 0) {
                snprintf(path, sizeof(path), "%s/%s", env_dir, base);
            } else {
                snprintf(path, sizeof(path), "/etc/systemg/%s", base);
            }
        }
        int fd = open(path, O_RDONLY);
        if (fd < 0) {
            fprintf(2, "Failed to open %s: %s\n", path, strerror(errno));
            failures++;
            continue;
        }
        printf("# %s\n", path);
        char buf[1024];
        ssize_t n;
        while ((n = read(fd, buf, sizeof(buf))) > 0) {
            write(1, buf, (size_t)n);
        }
        close(fd);
    }
    return failures > 0 ? 1 : 0;
}

static int is_known_unit_key(const char *key) {
    static const char *const keys[] = {
        "Description", "After", "Requires", "Wants", "Before", "Conflicts",
        "Documentation", "DefaultDependencies", "ConditionPathExists",
        "ConditionVirtualization", "AssertPathExists", "BindsTo", "PartOf", NULL
    };
    for (int i = 0; keys[i] != NULL; i++) {
        if (strcmp(keys[i], key) == 0) return 1;
    }
    return 0;
}

static int is_known_service_key(const char *key) {
    static const char *const keys[] = {
        "Type", "ExecStart", "ExecStartPre", "ExecStartPost", "ExecStop",
        "ExecStopPost", "ExecReload", "Restart", "RestartSec", "TimeoutSec",
        "Environment", "StandardOutput", "StandardError", "WorkingDirectory",
        "User", "Group", "KillMode", "RemainAfterExit", NULL
    };
    for (int i = 0; keys[i] != NULL; i++) {
        if (strcmp(keys[i], key) == 0) return 1;
    }
    return 0;
}

static int is_known_install_key(const char *key) {
    static const char *const keys[] = {
        "WantedBy", "RequiredBy", "Alias", "Also", NULL
    };
    for (int i = 0; keys[i] != NULL; i++) {
        if (strcmp(keys[i], key) == 0) return 1;
    }
    return 0;
}

static int verify_single_file(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        fprintf(2, "Failed to open unit file %s: %s\n", path, strerror(errno));
        return 1;
    }
    char buf[16384];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n < 0) {
        fprintf(2, "Failed to read unit file %s: %s\n", path, strerror(errno));
        return 1;
    }
    buf[n] = 0;

    int line_num = 0;
    int section = 0; /* 1: [Unit], 2: [Service], 3: [Install], -1: unknown */
    int has_service = 0;
    int has_exec_start = 0;
    int errors = 0;
    int warnings = 0;

    char *line = buf;
    while (line != NULL && *line != 0) {
        char *next = strchr(line, '\n');
        if (next != NULL) {
            *next = 0;
            next++;
        }
        line_num++;
        char *text = trim(line);
        if (*text == 0 || *text == '#' || *text == ';') {
            line = next;
            continue;
        }

        if (*text == '[') {
            size_t l = strlen(text);
            if (text[l - 1] != ']') {
                fprintf(2, "%s:%d: Syntax error: unclosed section header '%s'\n", path, line_num, text);
                errors++;
                section = -1;
            } else if (strcmp(text, "[Unit]") == 0) {
                section = 1;
            } else if (strcmp(text, "[Service]") == 0) {
                section = 2;
                has_service = 1;
            } else if (strcmp(text, "[Install]") == 0) {
                section = 3;
            } else if (strcmp(text, "[Socket]") == 0 || strcmp(text, "[Timer]") == 0 ||
                       strcmp(text, "[Mount]") == 0 || strcmp(text, "[Path]") == 0) {
                section = 4;
            } else {
                fprintf(2, "%s:%d: Warning: unknown section header '%s'\n", path, line_num, text);
                warnings++;
                section = -1;
            }
        } else {
            char *equals = strchr(text, '=');
            if (equals == NULL) {
                fprintf(2, "%s:%d: Syntax error: missing '=' in assignment '%s'\n", path, line_num, text);
                errors++;
            } else {
                *equals = 0;
                char *key = trim(text);
                char *val = trim(equals + 1);

                if (section == 1) {
                    if (!is_known_unit_key(key)) {
                        fprintf(2, "%s:%d: Warning: unknown directive '%s' in [Unit]\n", path, line_num, key);
                        warnings++;
                    }
                } else if (section == 2) {
                    if (!is_known_service_key(key)) {
                        fprintf(2, "%s:%d: Warning: unknown directive '%s' in [Service]\n", path, line_num, key);
                        warnings++;
                    } else if (strcmp(key, "ExecStart") == 0) {
                        has_exec_start = 1;
                        if (*val == 0) {
                            fprintf(2, "%s:%d: Syntax error: ExecStart command is empty\n", path, line_num);
                            errors++;
                        } else {
                            const char *cmd_path = val;
                            while (*cmd_path == '-' || *cmd_path == '@' || *cmd_path == '+' ||
                                   *cmd_path == '!' || *cmd_path == ':' || *cmd_path == '"') {
                                cmd_path++;
                            }
                            if (*cmd_path != '/') {
                                fprintf(2, "%s:%d: Warning: ExecStart path '%s' is not absolute\n", path, line_num, val);
                                warnings++;
                            }
                        }
                    } else if (strcmp(key, "Type") == 0) {
                        if (strcmp(val, "simple") != 0 && strcmp(val, "exec") != 0 &&
                            strcmp(val, "oneshot") != 0 && strcmp(val, "forking") != 0 &&
                            strcmp(val, "dbus") != 0 && strcmp(val, "notify") != 0 &&
                            strcmp(val, "idle") != 0) {
                            fprintf(2, "%s:%d: Warning: unknown unit type '%s'\n", path, line_num, val);
                            warnings++;
                        }
                    } else if (strcmp(key, "Restart") == 0) {
                        if (strcmp(val, "no") != 0 && strcmp(val, "always") != 0 &&
                            strcmp(val, "on-success") != 0 && strcmp(val, "on-failure") != 0 &&
                            strcmp(val, "on-abnormal") != 0 && strcmp(val, "on-abort") != 0 &&
                            strcmp(val, "on-watchdog") != 0) {
                            fprintf(2, "%s:%d: Warning: unknown restart policy '%s'\n", path, line_num, val);
                            warnings++;
                        }
                    } else if (strcmp(key, "RestartSec") == 0 || strcmp(key, "TimeoutSec") == 0) {
                        int valid = 1;
                        if (*val == 0) {
                            valid = 0;
                        } else {
                            for (const char *c = val; *c != 0; c++) {
                                if (*c < '0' || *c > '9') {
                                    if (strcmp(c, "s") != 0 && strcmp(c, "ms") != 0 &&
                                        strcmp(c, "min") != 0) {
                                        valid = 0;
                                    }
                                    break;
                                }
                            }
                        }
                        if (!valid) {
                            fprintf(2, "%s:%d: Warning: invalid %s value '%s'\n", path, line_num, key, val);
                            warnings++;
                        }
                    }
                } else if (section == 3) {
                    if (!is_known_install_key(key)) {
                        fprintf(2, "%s:%d: Warning: unknown directive '%s' in [Install]\n", path, line_num, key);
                        warnings++;
                    }
                } else if (section == 0) {
                    fprintf(2, "%s:%d: Warning: directive '%s' outside of any section\n", path, line_num, key);
                    warnings++;
                }
            }
        }
        line = next;
    }

    if (string_ends_with(path, ".service")) {
        if (!has_service) {
            fprintf(2, "%s: Error: Service unit file missing [Service] section\n", path);
            errors++;
        } else if (!has_exec_start) {
            fprintf(2, "%s: Error: [Service] section missing ExecStart=\n", path);
            errors++;
        }
    }

    if (errors == 0 && warnings == 0) {
        printf("Unit %s is valid.\n", path);
    } else if (errors == 0) {
        printf("Unit %s parsed with %d warning(s).\n", path, warnings);
    } else {
        printf("Unit %s failed verification with %d error(s) and %d warning(s).\n", path, errors, warnings);
    }

    return errors > 0 ? 1 : 0;
}

static int verify_directory(const char *dir_path) {
    DIR *dir = opendir(dir_path);
    if (dir == NULL) {
        fprintf(2, "Failed to open directory %s: %s\n", dir_path, strerror(errno));
        return 1;
    }

    int failures = 0;
    int count = 0;
    struct dirent *de;
    while ((de = readdir(dir)) != NULL) {
        if (de->d_name[0] == '.') {
            continue;
        }
        if (string_ends_with(de->d_name, ".service") || string_ends_with(de->d_name, ".target")) {
            char path[256];
            snprintf(path, sizeof(path), "%s/%s", dir_path, de->d_name);
            count++;
            if (verify_single_file(path) != 0) {
                failures++;
            }
        }
    }
    closedir(dir);

    if (count == 0) {
        printf("No unit files found in %s.\n", dir_path);
    }
    return failures > 0 ? 1 : 0;
}

static int cmd_verify(int argc, char **argv) {
    if (argc > 0) {
        int failures = 0;
        for (int i = 0; i < argc; i++) {
            struct stat st;
            if (stat(argv[i], &st) == 0 && S_ISDIR(st.st_mode)) {
                if (verify_directory(argv[i]) != 0) {
                    failures++;
                }
            } else {
                if (verify_single_file(argv[i]) != 0) {
                    failures++;
                }
            }
        }
        return failures > 0 ? 1 : 0;
    }

    const char *dir_path = getenv("SPACEOS_SERVICES");
    if (dir_path == NULL || *dir_path == 0) {
        dir_path = "/etc/systemg";
    }

    struct stat st;
    if (stat(dir_path, &st) == 0 && S_ISDIR(st.st_mode)) {
        return verify_directory(dir_path);
    }

    fprintf(2, "Failed to open unit directory %s: No such file or directory\n", dir_path);
    return 1;
}

static void print_help(void) {
    printf("systemg-analyze [OPTIONS...] COMMAND ...\n\n"
           "Profile and analyze systemg system and service manager performance.\n\n"
           "Commands:\n"
           "  time                              Print system boot-up time (default)\n"
           "  blame                             Print list of running units ordered by time to start\n"
           "  critical-chain [UNIT...]          Print tree of the time-critical chain of units\n"
           "  plot                              Output SVG graphic visualization of boot timeline\n"
           "  dot [PATTERN...]                  Output Graphviz dot dependency graph\n"
           "  dump [PATTERN...]                 Output state serialization of service manager\n"
           "  cat-config NAME|PATH...           Show configuration file contents of units\n"
           "  unit-paths                        List unit load search paths\n"
           "  verify [FILE|DIR...]              Check unit files for correctness\n"
           "  help                              Show this help\n\n"
           "Options:\n"
           "  -h, --help                        Show this help\n"
           "      --version                     Show package version\n"
           "  -q, --quiet                       Do not emit hints\n"
           "      --no-pager                    Do not start a pager\n"
           "      --system                      Operate on system manager (default)\n"
           "      --user                        Operate on user service manager\n");
}

int main(int argc, char **argv) {
    int arg_idx = 1;
    while (arg_idx < argc && argv[arg_idx][0] == '-') {
        if (strcmp(argv[arg_idx], "-h") == 0 || strcmp(argv[arg_idx], "--help") == 0 ||
            strcmp(argv[arg_idx], "help") == 0) {
            print_help();
            return 0;
        }
        if (strcmp(argv[arg_idx], "--version") == 0) {
            printf("systemg-analyze (SpaceOS-9 systemg) 2026.11\n");
            return 0;
        }
        if (strcmp(argv[arg_idx], "--no-pager") == 0 ||
            strcmp(argv[arg_idx], "-q") == 0 ||
            strcmp(argv[arg_idx], "--quiet") == 0 ||
            strcmp(argv[arg_idx], "--system") == 0 ||
            strcmp(argv[arg_idx], "--user") == 0) {
            arg_idx++;
            continue;
        }
        fprintf(2, "Unknown option '%s'. See 'systemg-analyze --help'.\n", argv[arg_idx]);
        return 1;
    }

    if (arg_idx >= argc) {
        return cmd_time();
    }

    const char *action = argv[arg_idx];

    if (strcmp(action, "--help") == 0 || strcmp(action, "-h") == 0 || strcmp(action, "help") == 0) {
        print_help();
        return 0;
    }
    if (strcmp(action, "--version") == 0) {
        printf("systemg-analyze (SpaceOS-9 systemg) 2026.11\n");
        return 0;
    }

    if (strcmp(action, "time") == 0) {
        return cmd_time();
    }
    if (strcmp(action, "blame") == 0) {
        return cmd_blame();
    }
    if (strcmp(action, "critical-chain") == 0) {
        return cmd_critical_chain(argc - arg_idx - 1, argv + arg_idx + 1);
    }
    if (strcmp(action, "plot") == 0) {
        return cmd_plot();
    }
    if (strcmp(action, "dot") == 0) {
        return cmd_dot(argc - arg_idx - 1, argv + arg_idx + 1);
    }
    if (strcmp(action, "dump") == 0) {
        return cmd_dump(argc - arg_idx - 1, argv + arg_idx + 1);
    }
    if (strcmp(action, "cat-config") == 0) {
        return cmd_cat_config(argc - arg_idx - 1, argv + arg_idx + 1);
    }
    if (strcmp(action, "unit-paths") == 0) {
        return cmd_unit_paths();
    }
    if (strcmp(action, "verify") == 0) {
        return cmd_verify(argc - arg_idx - 1, argv + arg_idx + 1);
    }

    fprintf(2, "Unknown command '%s'. See 'systemg-analyze --help'.\n", action);
    return 1;
}
