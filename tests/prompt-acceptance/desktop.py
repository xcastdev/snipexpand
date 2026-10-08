#!/usr/bin/env python3
"""Execute only inside disposable guest's private desktop session."""

import os
import sys

env = os.environ.copy()
with open("/home/vagrant/desktop.env") as desktop:
    for line in desktop:
        key, _, value = line.rstrip("\n").partition("=")
        if key in (
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "WAYLAND_DISPLAY",
            "SWAYSOCK",
        ):
            env[key] = value
os.execvpe(sys.argv[1], sys.argv[1:], env)
