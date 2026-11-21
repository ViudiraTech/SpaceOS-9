/*
 *       packages/spk-tests/sudo-edit-probe.c
 *       Appends a line to the file passed by sudoedit
 *       2026/11/21 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "splibc.h"

int main(int argc, char **argv, char **envp) {
    (void)envp;
    if (argc < 2) {
        return 1;
    }
    int fd = open(argv[1], O_WRONLY | O_APPEND);
    if (fd < 0) {
        return 1;
    }
    if (write(fd, "edited\n", 7) != 7) {
        close(fd);
        return 1;
    }
    close(fd);
    return 0;
}
