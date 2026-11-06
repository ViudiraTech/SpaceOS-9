/*
 *
 *       user/splibc/include/journal.h
 *       Standard Journal definitions and on-disk binary format
 *
 *       2026/11/06 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#ifndef JOURNAL_H
#define JOURNAL_H

#include "splibc.h"

#define JOURNAL_HEADER_MAGIC 0x4c504b5348485248ULL /* "LPKSHHRH" - systemd journal signature */
#define JOURNAL_ENTRY_MAGIC  0x454e5452594f424aULL /* "ENTRYOBJ" */

#define JOURNAL_VERSION 1
#define MAX_PAYLOAD_SIZE 4096

#define JOURNAL_SOCKET_PATH       "/run/systemd/journal/socket"
#define JOURNAL_DEV_LOG_PATH      "/run/systemd/journal/dev-log"
#define JOURNAL_DEV_LOG_SYMLINK   "/dev/log"
#define JOURNAL_STDOUT_PATH       "/run/systemd/journal/stdout"
#define JOURNAL_QUERY_PATH        "/run/systemd/journal/query.sock"
#define JOURNAL_DIR_SYSTEM        "/var/log/journal"
#define JOURNAL_DIR_VOLATILE      "/run/log/journal"
#define JOURNAL_FILE_SYSTEM       "/var/log/journal/system.journal"
#define JOURNAL_FILE_VOLATILE     "/run/log/journal/system.journal"

#define LOG_EMERG   0
#define LOG_ALERT   1
#define LOG_CRIT    2
#define LOG_ERR     3
#define LOG_WARNING 4
#define LOG_NOTICE  5
#define LOG_INFO    6
#define LOG_DEBUG   7

#define LOG_KERN    (0<<3)
#define LOG_USER    (1<<3)
#define LOG_DAEMON  (3<<3)

enum journal_transport {
    TRANSPORT_KERNEL = 0,
    TRANSPORT_JOURNAL = 1,
    TRANSPORT_SYSLOG = 2,
    TRANSPORT_STDOUT = 3,
};

struct journal_header {
    u64 magic;
    u32 version;
    u32 header_size;
    u64 entry_count;
    u64 head_entry_offset;
    u64 tail_entry_offset;
    char machine_id[33];
    char boot_id[33];
    u64 min_seq;
    u64 max_seq;
    u64 min_realtime_us;
    u64 max_realtime_us;
    u64 file_size;
    u8 reserved[48];
} __attribute__((packed));

struct journal_entry_hdr {
    u64 magic;
    u32 entry_size;
    u32 payload_size;
    u64 seq_num;
    u64 realtime_usec;
    u64 monotonic_usec;
    u8  priority;
    u8  facility;
    u8  transport;
    u8  reserved;
    u32 pid;
    u32 uid;
    u32 gid;
    u16 unit_len;
    u16 comm_len;
    u16 ident_len;
    u32 message_len;
} __attribute__((packed));

#endif
