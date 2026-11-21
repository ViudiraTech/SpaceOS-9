/*
 *       packages/hello/hello-v2.c
 *       hello 0.2 payload for upgrade tests
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "splibc.h"

int main(int argc, char **argv, char **envp) {
    (void)argc;
    (void)argv;
    (void)envp;
    printf("hello v2 from splibc (pid %d)\n", getpid());
    return 0;
}
