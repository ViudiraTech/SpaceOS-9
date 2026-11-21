/*
 *       packages/spk-tests/setuid-probe.c
 *       Prints the credentials seen after exec for the kernel setuid test
 *       2026/11/21 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "splibc.h"

int main(int argc, char **argv, char **envp) {
    (void)argc;
    (void)argv;
    (void)envp;
    printf("UID=%d EUID=%d GID=%d EGID=%d\n",
           (int)getuid(), (int)geteuid(), (int)getgid(), (int)getegid());
    return 0;
}
