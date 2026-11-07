/*
 *
 *       modules/e1000/e1000.c
 *       8254x gigabit Ethernet driver module
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 */

#include "spk/module.h"

#define E1000_DRIVER_LABEL "e1000"
#define E1000_USE_MSIX 0
#define E1000_DISABLE_NS 0
#include "spk/e1000_core.h"

static const struct spk_pci_device_id e1000_ids[] = {
    { 0x8086u, 0x100eu, 0, 0, 0, 0, 0 },
    { 0x8086u, 0x1016u, 0, 0, 0, 0, 0 },
    { 0x8086u, 0x100fu, 0, 0, 0, 0, 0 },
    { 0x8086u, 0x1013u, 0, 0, 0, 0, 0 },
    { 0x8086u, 0x1019u, 0, 0, 0, 0, 0 },
    { 0, 0, 0, 0, 0, 0, 0 },
};

static int e1000_init(void)
{
    return e1000_core_init("e1000", e1000_ids);
}

static void e1000_exit(void)
{
    e1000_core_exit();
}

SPK_MODULE("e1000", "1.0", "GPL", e1000_init, e1000_exit);
