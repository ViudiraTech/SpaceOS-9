/*
 *
 *       user/bin/systemctl.c
 *       Command-line client for the systemg service manager
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#include "splibc.h"

#define SOCK_PATH "/run/systemg.sock"

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

static int connect_systemg(void) {
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) {
        return -1;
    }
    struct sockaddr_un sun;
    memset(&sun, 0, sizeof(sun));
    sun.sun_family = AF_UNIX;
    strncpy(sun.sun_path, SOCK_PATH, sizeof(sun.sun_path) - 1);
    if (connect(fd, &sun, sizeof(sun)) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

static int send_command(const char *cmd, const char *arg, char *resp, size_t resp_len) {
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

    size_t total = 0;
    while (total + 1 < resp_len) {
        ssize_t n = read(fd, resp + total, resp_len - total - 1);
        if (n <= 0) {
            break;
        }
        total += n;
        resp[total] = 0;
        if (strcmp(resp, "OK\n") == 0 || strncmp(resp, "ERR:", 4) == 0) {
            break;
        }
        if (strcmp(resp, "active\n") == 0 || strcmp(resp, "inactive\n") == 0 || strcmp(resp, "failed\n") == 0) {
            break;
        }
    }
    resp[total] = 0;
    close(fd);
    return 0;
}

static void print_help(void) {
    printf("systemctl [OPTIONS...] COMMAND [UNIT...]\n\n"
           "Query or send control commands to the systemg service manager.\n\n"
           "Unit Commands:\n"
           "  list-units                      List units currently in memory\n"
           "  start UNIT...                   Start (activate) one or more units\n"
           "  stop UNIT...                    Stop (deactivate) one or more units\n"
           "  restart UNIT...                 Start or restart one or more units\n"
           "  status [UNIT...]                Show runtime status about units\n"
           "  is-active UNIT...               Check whether units are active\n"
           "  is-failed UNIT...               Check whether units are failed\n\n"
           "Manager Commands:\n"
           "  daemon-reload                   Reload systemg unit configuration\n"
           "  help                            Show this help\n");
}

static int cmd_list_units(void) {
    char resp[4096];
    if (send_command("LIST", NULL, resp, sizeof(resp)) < 0) {
        return 1;
    }

    printf("  %-28s %-6s %-8s %-9s %s\n", "UNIT", "LOAD", "ACTIVE", "SUB", "DESCRIPTION");
    int count = 0;
    char *line = resp;
    while (line != NULL && *line != 0) {
        char *next = strchr(line, '\n');
        if (next != NULL) {
            *next = 0;
            next++;
        }
        if (strncmp(line, "COUNT:", 6) == 0) {
            line = next;
            continue;
        }
        char *pipe1 = strchr(line, '|');
        if (pipe1 != NULL) {
            *pipe1 = 0;
            char *pipe2 = strchr(pipe1 + 1, '|');
            if (pipe2 != NULL) {
                *pipe2 = 0;
                char *pipe3 = strchr(pipe2 + 1, '|');
                if (pipe3 != NULL) {
                    *pipe3 = 0;
                    char *pipe4 = strchr(pipe3 + 1, '|');
                    if (pipe4 != NULL) {
                        *pipe4 = 0;
                        char *pipe5 = strchr(pipe4 + 1, '|');
                        char *unit = line;
                        char *load = pipe1 + 1;
                        char *active = pipe2 + 1;
                        char *sub = pipe3 + 1;
                        char *desc = (pipe5 != NULL) ? (pipe5 + 1) : (pipe4 + 1);

                        const char *bullet;
                        if (strcmp(active, "active") == 0) {
                            bullet = "\033[1;32m●\033[0m";
                        } else if (strcmp(active, "failed") == 0) {
                            bullet = "\033[1;31m●\033[0m";
                        } else {
                            bullet = "\033[2m○\033[0m";
                        }
                        printf("%s %-28s %-6s %-8s %-9s %s\n",
                               bullet, unit, load, active, sub, desc);
                        count++;
                    }
                }
            }
        }
        line = next;
    }

    printf("\nLOAD   = Reflects whether the unit definition was properly loaded.\n"
           "ACTIVE = The high-level unit activation state, i.e. generalization of SUB.\n"
           "SUB    = The low-level unit activation state, values depend on unit type.\n\n"
           "%d loaded units listed.\n",
           count);
    return 0;
}

static int cmd_status(const char *unit_name) {
    if (unit_name == NULL || *unit_name == 0) {
        return cmd_list_units();
    }
    char resp[4096];
    if (send_command("STATUS", unit_name, resp, sizeof(resp)) < 0) {
        return 1;
    }
    if (strncmp(resp, "ERR:", 4) == 0) {
        fprintf(2, "Unit %s could not be found.\n", unit_name);
        return 3;
    }

    char name[64] = {0};
    char desc[96] = {0};
    char loaded[128] = {0};
    char state[64] = {0};
    char pid[32] = {0};
    char uptime[32] = {0};
    char restarts[32] = {0};

    char *line = resp;
    while (line != NULL && *line != 0) {
        char *next = strchr(line, '\n');
        if (next != NULL) {
            *next = 0;
            next++;
        }
        if (strncmp(line, "NAME:", 5) == 0) {
            strncpy(name, line + 5, sizeof(name) - 1);
        } else if (strncmp(line, "DESC:", 5) == 0) {
            strncpy(desc, line + 5, sizeof(desc) - 1);
        } else if (strncmp(line, "LOADED:", 7) == 0) {
            strncpy(loaded, line + 7, sizeof(loaded) - 1);
        } else if (strncmp(line, "STATE:", 6) == 0) {
            strncpy(state, line + 6, sizeof(state) - 1);
        } else if (strncmp(line, "PID:", 4) == 0) {
            strncpy(pid, line + 4, sizeof(pid) - 1);
        } else if (strncmp(line, "UPTIME:", 7) == 0) {
            strncpy(uptime, line + 7, sizeof(uptime) - 1);
        } else if (strncmp(line, "RESTARTS:", 9) == 0) {
            strncpy(restarts, line + 9, sizeof(restarts) - 1);
        }
        line = next;
    }

    const char *bullet;
    const char *state_color;
    if (strstr(state, "active") != NULL) {
        bullet = "\033[1;32m●\033[0m";
        state_color = "\033[1;32m";
    } else if (strstr(state, "failed") != NULL) {
        bullet = "\033[1;31m●\033[0m";
        state_color = "\033[1;31m";
    } else {
        bullet = "\033[2m○\033[0m";
        state_color = "\033[2m";
    }

    printf("%s %s - %s\n", bullet, name, desc);
    printf("     Loaded: %s\n", loaded);

    int up_sec = atoi(uptime);
    if (up_sec > 60) {
        printf("     Active: %s%s\033[0m since %dmin %ds ago\n",
               state_color, state, up_sec / 60, up_sec % 60);
    } else {
        printf("     Active: %s%s\033[0m since %ds ago\n",
               state_color, state, up_sec);
    }

    int main_pid = atoi(pid);
    if (main_pid > 0) {
        printf("   Main PID: %d (%s)\n", main_pid, name);
    }
    int r = atoi(restarts);
    if (r > 0) {
        printf("   Restarts: %d\n", r);
    }
    return (strstr(state, "active") != NULL) ? 0 : 3;
}

static int cmd_simple(const char *cmd, const char *unit_name) {
    if (unit_name == NULL || *unit_name == 0) {
        fprintf(2, "systemctl: %s requires a unit name\n", cmd);
        return 1;
    }
    char resp[512];
    if (send_command(cmd, unit_name, resp, sizeof(resp)) < 0) {
        return 1;
    }
    if (strncmp(resp, "ERR:", 4) == 0) {
        fprintf(2, "Failed to %s %s: %s\n", cmd, unit_name, resp + 4);
        return 1;
    }
    return 0;
}

static int cmd_is_active(const char *unit_name) {
    if (unit_name == NULL || *unit_name == 0) {
        return 3;
    }
    char resp[512];
    if (send_command("IS_ACTIVE", unit_name, resp, sizeof(resp)) < 0) {
        return 3;
    }
    char *s = trim(resp);
    printf("%s\n", s);
    return (strcmp(s, "active") == 0) ? 0 : 3;
}

static int cmd_is_failed(const char *unit_name) {
    if (unit_name == NULL || *unit_name == 0) {
        return 1;
    }
    char resp[512];
    if (send_command("IS_FAILED", unit_name, resp, sizeof(resp)) < 0) {
        return 1;
    }
    char *s = trim(resp);
    printf("%s\n", s);
    return (strcmp(s, "failed") == 0) ? 0 : 1;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        return cmd_list_units();
    }

    const char *action = argv[1];
    if (strcmp(action, "--help") == 0 || strcmp(action, "-h") == 0 || strcmp(action, "help") == 0) {
        print_help();
        return 0;
    }
    if (strcmp(action, "list-units") == 0) {
        return cmd_list_units();
    }
    if (strcmp(action, "status") == 0) {
        return cmd_status(argc >= 3 ? argv[2] : NULL);
    }
    if (strcmp(action, "start") == 0) {
        return cmd_simple("START", argc >= 3 ? argv[2] : NULL);
    }
    if (strcmp(action, "stop") == 0) {
        return cmd_simple("STOP", argc >= 3 ? argv[2] : NULL);
    }
    if (strcmp(action, "restart") == 0) {
        return cmd_simple("RESTART", argc >= 3 ? argv[2] : NULL);
    }
    if (strcmp(action, "is-active") == 0) {
        return cmd_is_active(argc >= 3 ? argv[2] : NULL);
    }
    if (strcmp(action, "is-failed") == 0) {
        return cmd_is_failed(argc >= 3 ? argv[2] : NULL);
    }
    if (strcmp(action, "daemon-reload") == 0 || strcmp(action, "reload") == 0) {
        char resp[512];
        if (send_command("RELOAD", NULL, resp, sizeof(resp)) < 0) {
            return 1;
        }
        return 0;
    }

    /* Fallback: if single argument looks like a unit name, run status */
    if (argc == 2 && action[0] != '-') {
        return cmd_status(action);
    }

    fprintf(2, "Unknown command: %s. See 'systemctl --help'.\n", action);
    return 1;
}
