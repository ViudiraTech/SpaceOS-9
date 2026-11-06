/*
 *
 *       modules/hello/hello.c
 *       Reference loadable module
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#include "spk/module.h"

static int hello_init(void) {
    spk_printk("hello: Hello, world from a SpaceKernel module!\n");
    spk_printk("hello: abi=%u vermagic=%s uptime=%llu us\n",
               SPK_MODULE_ABI_VERSION,
               SPK_VERMAGIC,
               (unsigned long long)spk_uptime_us());
    spk_printk("HELLO_MODULE_OK\n");
    return 0;
}

static void hello_exit(void) {
    spk_printk("hello: module unloaded\n");
}

SPK_MODULE("hello", "1.0", "GPL", hello_init, hello_exit);
