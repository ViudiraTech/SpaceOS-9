#!/bin/sh
#
#       user/desktop/assets/session.sh
#       SPUI user session started by DDM after authentication
#
#       2026/11/01 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

# DDM has already started Xorg, created the per-user Xauthority file and set
# HOME, USER, LOGNAME, SHELL, DISPLAY, XAUTHORITY and XDG_RUNTIME_DIR.  This
# script must not start another X server and must not run anything as root.

export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
export XDG_CURRENT_DESKTOP=SPUI

exec /usr/bin/spui shell
