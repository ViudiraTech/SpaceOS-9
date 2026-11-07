/*
 *
 *       modules/include/spk/e1000_regs.h
 *       Register, bit and descriptor definitions shared by e1000/e1000e
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 * Offsets and bit values follow the Intel 8254x/82574 documentation and the
 * definitions used by Linux's drivers/net/ethernet/intel/{e1000,e1000e}.
 */

#ifndef SPK_E1000_REGS_H
#define SPK_E1000_REGS_H

#define E1000_CTRL 0x00000
#define E1000_STATUS 0x00008
#define E1000_EECD 0x00010
#define E1000_EERD 0x00014
#define E1000_CTRL_EXT 0x00018
#define E1000_FCAH 0x0002c
#define E1000_FCAL 0x00028
#define E1000_FCT 0x00030
#define E1000_VET 0x00038
#define E1000_ICR 0x000c0
#define E1000_ITR 0x000c4
#define E1000_ICS 0x000c8
#define E1000_IMS 0x000d0
#define E1000_IMC 0x000d8
#define E1000_EIAC 0x000dc
#define E1000_IVAR 0x000e4
#define E1000_RCTL 0x00100
#define E1000_FCTTV 0x00170
#define E1000_TCTL 0x00400
#define E1000_TIPG 0x00410
#define E1000_PBA 0x01000
#define E1000_RXCSUM 0x05000
#define E1000_MTA 0x05200
#define E1000_RA 0x05400
#define E1000_VFTA 0x05600
#define E1000_WUC 0x05800
#define E1000_RDBAL 0x02800
#define E1000_RDBAH 0x02804
#define E1000_RDLEN 0x02808
#define E1000_RDH 0x02810
#define E1000_RDT 0x02818
#define E1000_RDTR 0x02820
#define E1000_RADV 0x0282c
#define E1000_TDBAL 0x03800
#define E1000_TDBAH 0x03804
#define E1000_TDLEN 0x03808
#define E1000_TDH 0x03810
#define E1000_TDT 0x03818
#define E1000_TIDV 0x03820
#define E1000_TADV 0x0382c

#define E1000_CTRL_RST 0x04000000
#define E1000_CTRL_SLU 0x00000040
#define E1000_CTRL_LRST 0x00000008
#define E1000_CTRL_ASDE 0x00000020
#define E1000_CTRL_FRCSPD 0x00000800
#define E1000_CTRL_FRCDPX 0x00001000
#define E1000_CTRL_ILOS 0x00000080
#define E1000_CTRL_PHY_RST 0x80000000

#define E1000_STATUS_LU 0x00000002

#define E1000_EERD_START 0x00000001
#define E1000_EERD_DONE 0x00000010
#define E1000_EERD_ADDR_SHIFT 8
#define E1000_EERD_DATA_SHIFT 16

#define E1000_RCTL_EN 0x00000002
#define E1000_RCTL_UPE 0x00000008
#define E1000_RCTL_MPE 0x00000010
#define E1000_RCTL_LPE 0x00000020
#define E1000_RCTL_BAM 0x00008000
#define E1000_RCTL_SZ_2048 0x00000000
#define E1000_RCTL_BSEX 0x02000000
#define E1000_RCTL_SECRC 0x04000000

#define E1000_TCTL_EN 0x00000002
#define E1000_TCTL_PSP 0x00000008
#define E1000_TCTL_CT 0x00000ff0
#define E1000_TCTL_COLD 0x003ff000
#define E1000_TCTL_DEFAULT \
    (E1000_TCTL_EN | E1000_TCTL_PSP | (0x10u << 4) | (0x40u << 12))

#define E1000_TIPG_DEFAULT 0x0060200a /* 10, 8, 6 for copper */

#define E1000_ICR_TXDW 0x00000001
#define E1000_ICR_LSC 0x00000004
#define E1000_ICR_RXSEQ 0x00000008
#define E1000_ICR_RXDMT0 0x00000010
#define E1000_ICR_RXO 0x00000040
#define E1000_ICR_RXT0 0x00000080
#define E1000_ICR_RXQ0 0x00100000
#define E1000_ICR_TXQ0 0x00400000
#define E1000_ICR_OTHER 0x01000000

#define E1000_IMS_ENABLE \
    (E1000_ICR_TXDW | E1000_ICR_LSC | E1000_ICR_RXSEQ | E1000_ICR_RXDMT0 | \
     E1000_ICR_RXO | E1000_ICR_RXT0 | E1000_ICR_RXQ0 | E1000_ICR_TXQ0 | \
     E1000_ICR_OTHER)

#define E1000_IVAR_INT_ALLOC_VALID 0x08u
#define E1000_IVAR_TX_INT_EVERY_WB (1u << 31)

#define E1000_RAH_AV 0x80000000

/* 16-byte legacy descriptors. Bitfields are assembled by hand so the C
 * compiler cannot reorder them. */
struct e1000_tx_desc {
    unsigned long long buffer_addr;
    unsigned int lower; /* [15:0] length, [23:16] cso, [31:24] command */
    unsigned int upper; /* [7:0] status, [15:8] css, [31:16] special */
} __attribute__((packed));

struct e1000_rx_desc {
    unsigned long long buffer_addr;
    unsigned short length;
    unsigned short checksum;
    unsigned char status;
    unsigned char errors;
    unsigned short special;
} __attribute__((packed));

#define E1000_TXD_CMD_EOP 0x01000000
#define E1000_TXD_CMD_IFCS 0x02000000
#define E1000_TXD_CMD_RS 0x08000000
#define E1000_TXD_STAT_DD 0x00000001

#define E1000_RXD_STAT_DD 0x01
#define E1000_RXD_STAT_EOP 0x02

#define E1000_RING_COUNT 256
#define E1000_RX_BUFFER_SIZE 2048
#define E1000_REG_BAR_SIZE 0x20000

#endif /* SPK_E1000_REGS_H */
