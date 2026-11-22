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
export XMODIFIERS="@im=spime"

# Start every XDG autostart entry. Session helpers (input methods, tray
# agents) ship their own .desktop file instead of being named here.
for entry in /etc/xdg/autostart/*.desktop; do
    [ -f "$entry" ] || continue
    exec_line=$(sed -n 's/^Exec=//p' "$entry" | head -n 1)
    if [ -n "$exec_line" ]; then
        sh -c "$exec_line" >/dev/null 2>&1 &
    fi
done

exec /usr/bin/spui shell
