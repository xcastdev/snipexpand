# Isolated application acceptance fixtures

Run these fixtures only in a disposable KVM guest. They exercise physical Space
triggers, the production Quickshell service and controls, surface teardown, and
exact application text after current-focus injection. They do not establish
original-field safety or application acknowledgment.

## Isolation before desktop access

Use a private qcow2 overlay. The validated Ubuntu 24.04 guest used:

```sh
qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp 4 -m 6144 \
  -nodefaults -no-user-config \
  -drive file=/tmp/snipexpand-acceptance/guest.qcow2,if=virtio,format=qcow2 \
  -device virtio-vga -device qemu-xhci -device usb-kbd -device usb-tablet \
  -netdev user,id=guestnet,hostfwd=tcp:127.0.0.1:22342-:22 \
  -device virtio-net-pci,netdev=guestnet -display none \
  -serial file:/tmp/snipexpand-acceptance/serial.log -monitor none \
  -qmp unix:/tmp/snipexpand-acceptance/qmp.sock,server=on,wait=off
```

Do not add host input passthrough, shared mounts, SPICE, clipboard channels,
VNC, a guest agent, or host Wayland/D-Bus sockets. Before Sway starts, verify
`systemd-detect-virt` is `kvm`, `/proc/bus/input/devices` contains only emulated
guest devices, and `findmnt -rn -t 9p,virtiofs` is empty. The runner checks the
matching QEMU command line and guest virtualization before sending keys. It is
a check of this prepared fixture, not a general VM security auditor.

## Guest setup

Transfer files explicitly through localhost SSH with a disposable key; never
share the source tree as a host mount. Install Sway, seatd, Chrome, Ghostty and
VS Code inside the guest. Start a private `dbus-run-session sway` using a guest
runtime directory and software rendering if required. Save its environment as
`/home/vagrant/desktop.env`; `desktop.py` reads only its four desktop variables.

For Quickshell on Ubuntu, the validated guest installed `nix-bin`, enabled
`experimental-features = nix-command flakes`, then ran
`sudo nix profile install nixpkgs#quickshell`. Resolve its `/nix/store` executable
and launch it as the desktop user with `QT_QUICK_BACKEND=software`. This installs
nothing on the host.

Copy these fixtures to `/home/vagrant/prompt-acceptance`. Copy `config.yml` to
`~/.config/snipexpand/config.yml` and `matches.yml` into its `match` directory.
Transfer the candidate binary as `/home/vagrant/snipexpand`; check `ldd` and
`--help` inside the guest, or build it there if its ABI is incompatible. Start
the daemon using `desktop.py`. Prepare the real QML fixture with:

```sh
python3 tests/prompt-acceptance/prepare_qml.py \
  --source ~/.config/quickshell --destination /tmp/snipexpand-acceptance/qml
```

Transfer this fixture and run Quickshell with `-p` pointing to it. Only its
single-screen selector differs from the Hyprland user shell. The production
service, controls, focus handling and surface teardown are copied verbatim.

Start targets through the guest `desktop.py`:

- Chrome: run `fixture_server.py`, then open `http://127.0.0.1:8765` using native
  Wayland Chrome and a disposable profile. The textarea records trusted events.
- Ghostty: launch with `-e python3 /home/vagrant/prompt-acceptance/terminal_target.py`.
  The raw PTY fixture records text/caret without executing typed snippets.
- VS Code: use `--extensionDevelopmentPath` pointing to `vscode-extension`, a
  disposable `--user-data-dir`, `--ozone-platform=wayland`, and software rendering.
  Dismiss first-run dialogs. Set `chat.disableAIFeatures: true`,
  `workbench.editor.empty.hint: "hidden"`, and disable quick suggestions and
  automatic bracket/quote closing. F8 focuses the tracked test document. It
  starts with `READY` so empty-editor links cannot intercept initial clicks.

Each target atomically records its own guest-only state file. These contain
only predefined test data. The harness does not log or persist production answers.

## Run

```sh
python3 tests/prompt-acceptance/run.py \
  --qmp /tmp/snipexpand-acceptance/qmp.sock \
  --ssh-key /tmp/snipexpand-acceptance/key --ssh-port 22342 \
  --known-hosts /tmp/snipexpand-acceptance/known_hosts --target chrome
```

Repeat for `ghostty` and `vscode`. Cases assert original keyword plus physical
Space before submission, repeated picker success, defaults, empty text, BMP
Unicode, multiline text, literal braces/cursor markers, Escape, disable, and
reload. The original emoji fixture must fail preflight without deleting its
keyword; editing that same form to ASCII then submitting must succeed. This
explicitly distinguishes unsupported non-BMP transport from silent corruption.
Add `--lifecycle` for handler loss, missing handler and delayed daemon startup.
Use `--startup-only` to check just the initial socket-error recovery path.
Native failure uncertainty, session/deadline failures and worker races have
separate offline Rust tests.

Record the binary hash, copied QML revision, application versions and command
results. Do not infer target success from an `issued` result. Shut down the guest
and remove its private overlay, keys, screenshots and sockets after verification.
Never run the old Hyprland per-character binding probe.
