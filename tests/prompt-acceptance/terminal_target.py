#!/usr/bin/env python3
"""Raw line editor fixture running inside the guest Ghostty PTY."""

import codecs
import json
import os
import sys
import termios
import tty
from pathlib import Path

saved = termios.tcgetattr(0)
text = ""
caret = 0
decoder = codecs.getincrementaldecoder("utf-8")()
try:
    tty.setraw(0)
    while True:
        value = decoder.decode(os.read(0, 1))
        if not value:
            continue
        if value in ("\x7f", "\b"):
            if caret:
                text = text[: caret - 1] + text[caret:]
                caret -= 1
        elif value == "\x03":
            break
        elif value == "\x15":
            text = ""
            caret = 0
        elif value == "\x1b":
            # Fixture has no authored cursor marker in terminal cases.
            os.read(0, 2)
        else:
            if value == "\r":
                value = "\n"
            text = text[:caret] + value + text[caret:]
            caret += len(value)
        Path("/home/vagrant/ghostty-state.tmp").write_text(
            json.dumps({"text": text, "caret": caret})
        )
        Path("/home/vagrant/ghostty-state.tmp").replace(
            "/home/vagrant/ghostty-state.json"
        )
        sys.stdout.write("\r\x1b[2K" + text.replace("\n", "\\n"))
        sys.stdout.flush()
finally:
    termios.tcsetattr(0, termios.TCSANOW, saved)
