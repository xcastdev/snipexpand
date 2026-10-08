#!/usr/bin/env python3
"""Drive prepared isolated guest fixtures through QMP hardware keyboard events.

This never starts a VM, installs software, or accesses host desktop sockets. See
README.md for required setup and isolation checks. Test answers are public data.
"""

import argparse
import json
import socket
import subprocess
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--qmp", required=True)
parser.add_argument("--ssh-key", required=True)
parser.add_argument("--ssh-port", type=int, required=True)
parser.add_argument("--known-hosts", required=True)
parser.add_argument("--lifecycle", action="store_true")
parser.add_argument("--startup-only", action="store_true")
parser.add_argument("--target", choices=["chrome", "ghostty", "vscode"], required=True)
args = parser.parse_args()

# Reject host input/desktop sharing before issuing any desktop commands.
qemu_args = []
for process in Path("/proc").glob("[0-9]*/cmdline"):
    try:
        command = process.read_bytes().decode().split("\0")
    except (OSError, UnicodeError):
        continue
    if (
        command
        and "qemu-system" in command[0]
        and any(args.qmp in arg for arg in command)
    ):
        qemu_args = command
        break
if not qemu_args:
    raise SystemExit("isolation_unverified: no matching QEMU process")
for argument in qemu_args:
    if any(
        value in argument
        for value in [
            "/dev/input",
            "/dev/uinput",
            "input-linux",
            "usb-host",
            "vfio",
            "virtiofs",
            "-fsdev",
            "-virtfs",
            "-spice",
            "-vnc",
            "vhost-user",
        ]
    ):
        raise SystemExit("isolation_unverified: forbidden host sharing option")
if "-nodefaults" not in qemu_args or "-no-user-config" not in qemu_args:
    raise SystemExit("isolation_unverified: implicit QEMU configuration")

ssh = [
    "ssh",
    "-F",
    "/dev/null",
    "-o",
    "BatchMode=yes",
    "-o",
    "UserKnownHostsFile=" + args.known_hosts,
    "-p",
    str(args.ssh_port),
    "-i",
    args.ssh_key,
    "vagrant@127.0.0.1",
]


def remote(command):
    return subprocess.check_output(ssh + [command], text=True, timeout=10)


if remote("systemd-detect-virt").strip() != "kvm":
    raise SystemExit("isolation_unverified: guest is not KVM")
if remote("findmnt -rn -t 9p,virtiofs || true").strip():
    raise SystemExit("isolation_unverified: shared filesystem")

qmp = socket.socket(socket.AF_UNIX)
qmp.connect(args.qmp)
stream = qmp.makefile("rwb")
stream.readline()


def command(name, arguments=None):
    packet = {"execute": name}
    if arguments is not None:
        packet["arguments"] = arguments
    stream.write(json.dumps(packet).encode() + b"\n")
    stream.flush()
    while True:
        response = json.loads(stream.readline())
        if "error" in response:
            raise RuntimeError(response["error"])
        if "return" in response:
            return response["return"]


command("qmp_capabilities")


def keys(*sequence):
    for key in sequence:
        command(
            "send-key",
            {
                "keys": [{"type": "qcode", "data": part} for part in key.split("+")],
                "hold-time": 80,
            },
        )
        time.sleep(0.2)


def state():
    return json.loads(remote("cat /home/vagrant/" + args.target + "-state.json"))


def assert_text(expected):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        actual = state()
        if actual["text"] == expected:
            return actual
        time.sleep(0.1)
    raise AssertionError({"expected": expected, "actual": actual})


def prepare(trigger):
    if args.target == "ghostty":
        keys("ctrl+u")
    else:
        if args.target == "vscode":
            keys("f8")
        keys("ctrl+a", "backspace")
    keys("semicolon", *trigger, "spc")
    # Assert actual target text contains the physical committing Space before
    # submission. This also rejects wrong target/editor focus in the fixture.
    assert_text(";" + trigger + " ")
    time.sleep(0.3)


targets = {
    "chrome": "google-chrome",
    "ghostty": "com.mitchellh.ghostty",
    "vscode": "com.microsoft.VSCode",
}
app = targets[args.target]
remote(
    "python3 /home/vagrant/prompt-acceptance/desktop.py swaymsg "
    + repr('fullscreen disable; [app_id="' + app + '"] focus; fullscreen enable')
)

if not args.startup_only:
    cases = [
        ("pick", "chosen ", "ret"),
        ("pick", "chosen ", "ret"),
        ("form", "Alice|literal ", "ctrl+ret"),
        ("empty", "ab ", "ctrl+ret"),
        ("bmp", "é{{secret}}$|$\n下一行 ", "ctrl+ret"),
    ]
    for trigger, expected, submit in cases:
        prepare(trigger)
        keys(submit)
        observed = assert_text(expected)
        assert observed["caret"] == len(expected), observed
        print(
            json.dumps(
                {
                    "target": args.target,
                    "case": trigger,
                    "result": "PASS",
                    "caret": observed["caret"],
                },
                ensure_ascii=False,
            ),
            flush=True,
        )
    prepare("pick")
    keys("esc")
    assert_text(";pick ")
    print(
        json.dumps({"target": args.target, "case": "cancel", "result": "PASS"}),
        flush=True,
    )
    for action in ["disable", "reload"]:
        prepare("pick")
        remote(
            "python3 /home/vagrant/prompt-acceptance/desktop.py /home/vagrant/snipexpand "
            + action
        )
        time.sleep(0.3)
        assert_text(";pick ")
        if action == "disable":
            remote(
                "python3 /home/vagrant/prompt-acceptance/desktop.py /home/vagrant/snipexpand enable"
            )
        print(
            json.dumps({"target": args.target, "case": action, "result": "PASS"}),
            flush=True,
        )

    prepare("literal")
    keys("ctrl+ret")
    time.sleep(0.5)
    assert_text(";literal ")
    keys("ctrl+a", *"corrected", "ctrl+ret")
    assert_text("corrected ")
    print(
        json.dumps(
            {
                "target": args.target,
                "case": "non_bmp_refusal_and_correction",
                "result": "PASS",
            }
        ),
        flush=True,
    )

if args.lifecycle or args.startup_only:
    if not args.startup_only:
        prepare("pick")
        remote("pkill -x .quickshell-wra")
        time.sleep(0.3)
        assert_text(";pick ")
        print(
            json.dumps(
                {"target": args.target, "case": "handler_loss", "result": "PASS"}
            ),
            flush=True,
        )
        prepare("pick")
        time.sleep(0.5)
        assert_text(";pick ")
        print(
            json.dumps({"target": args.target, "case": "no_handler", "result": "PASS"}),
            flush=True,
        )
    remote("pkill -x .quickshell-wra || true")
    remote("pkill -x snipexpand")
    # Start the real frontend while there is no socket, then create the socket.
    # This specifically tests the initial ServerNotFoundError recovery path.
    remote(
        "nohup env QT_QUICK_BACKEND=software python3 /home/vagrant/prompt-acceptance/desktop.py "
        "$(sudo readlink -f /root/.nix-profile/bin/quickshell) -p /home/vagrant/qml "
        ">/home/vagrant/qml-reconnect.log 2>&1 </dev/null &"
    )
    time.sleep(1)
    remote(
        "nohup python3 /home/vagrant/prompt-acceptance/desktop.py /home/vagrant/snipexpand "
        ">/home/vagrant/daemon-reconnect.log 2>&1 </dev/null &"
    )
    time.sleep(2)
    prepare("pick")
    keys("ret")
    assert_text("chosen ")
    print(
        json.dumps(
            {"target": args.target, "case": "startup_reconnect", "result": "PASS"}
        ),
        flush=True,
    )
