#!/usr/bin/env python3
"""Copy production frontend, adapting only monitor selection for single-screen Sway."""

import argparse
import shutil
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--source", type=Path, required=True)
parser.add_argument("--destination", type=Path, required=True)
args = parser.parse_args()
for relative in [
    "services/SnipExpandService.qml",
    "services/OverlayService.qml",
    "theme/Theme.qml",
    "theme/Scrim.qml",
    "theme/qmldir",
    "modules/snippets/SnippetPrompt.qml",
]:
    destination = args.destination / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(args.source / relative, destination)
(args.destination / "services/qmldir").write_text(
    "singleton SnipExpandService 1.0 SnipExpandService.qml\nsingleton OverlayService 1.0 OverlayService.qml\n"
)
prompt = args.destination / "modules/snippets/SnippetPrompt.qml"
text = prompt.read_text().replace("import Quickshell.Hyprland\n", "")
start = text.index("    property var monitors:")
end = text.index("\n\n", start)
prompt.write_text(
    text[:start]
    + "    property var monitors: active ? Quickshell.screens : []"
    + text[end:]
)
(args.destination / "shell.qml").write_text("""import QtQuick
import Quickshell
import "modules/snippets"
import "services"
Scope {
    SnippetPrompt {}
    Connections {
        target: SnipExpandService
        function onRequestReady() { console.log("AC_REQUEST"); }
        function onClosureRequested() { console.log("AC_ACCEPTED"); }
        function onPhaseChanged() { console.log("AC_PHASE", SnipExpandService.phase); }
    }
}
""")
