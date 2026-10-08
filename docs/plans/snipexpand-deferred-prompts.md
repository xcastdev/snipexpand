Github Tracked: false
Status: completed
Github Issue: none

# Deferred prompts over local IPC

## Goal

Allow Space-triggered snippets to request predefined choices or multiple field
values from an independent frontend. Keep fork changes localized so upstream
updates remain easy to merge.

The core must remain frontend independent and use the existing
`$XDG_RUNTIME_DIR/snipexpand.sock`. The first frontend lives in the user's
`~/.config/quickshell`, outside this repository. Follow the existing Rust,
Serde, Tokio, schema, test, and CI conventions. Limit integration edits and
avoid unrelated refactoring or new dependencies without demonstrated need.

The user supplied an approved specification and reviewed implementation outline.
On 2026-10-07, the user selected Quickshell for forms and pickers and explicitly
deferred the strict original-field guarantee. The initial version uses the
existing picker approach: close the frontend, wait for focus to settle, then
insert into the focused application. Application integrations and the strict
context-proof gate are deferred. Historical P1 evidence below remains unchanged.

## Scope

- In: text and choice fields; versioned, bounded prompt IPC; handler sessions and
  leases; asynchronous transactions; literal rendering; authored cursor placement;
  current-focus insertion after frontend closure; diagnostics; Quickshell forms
  and pickers through a frontend-independent protocol.
- Out: strict original-field authorization, application-specific context
  integrations, clipboard variables, arbitrary
  providers, desktop notifications, automatic frontend launching, recursive
  interactive snippets, idle debounce, multiple cursor stops, installation,
  upstream push, and release.

Plain snippets expand directly. Cursor-only snippets need no handler. Repeated
field references share one answer. Empty submitted text is valid. Cancellation
leaves the keyword and committing Space untouched. Values support Unicode and
multiline text, including literal `{{...}}` and `$|$`.

## Acceptance criteria

- [x] AC-1: Validate stable field and option IDs, labels, declaration order,
  defaults, and actionable errors.
- [x] AC-2: Preserve valid configurations, legacy IPC, plain snippets, and
  cursor-only snippets.
- [x] AC-3: Authenticate one handler as the daemon user and bind it to its
  connection/session. Reject a second handler as busy. Reconnection creates a
  new session.
- [x] AC-4: Space creates an opaque transaction ID and sends definitions without
  deleting target text.
- [x] AC-5: Waiting does not block keyboard processing, management IPC, pause,
  reload, or shutdown. Only one transaction is active; do not queue more matches.
- [x] AC-6: Validate owning-session submissions, option IDs, answer presence,
  and message shape before deletion. Allow corrections without extending time.
- [x] AC-7: Cancel before mutation on missing handler, failed delivery, missing
  acknowledgment, disconnect, expired lease, or completion timeout. Reconnection
  cannot revive requests.
- [x] AC-8: Cancel on user cancellation, pause, reload, or shutdown. Notify the
  handler when reachable. Strict original-target invalidation is deferred.
- [x] AC-9: Form input cannot trigger expansion. Close the Quickshell frontend
  and allow focus to settle before current-focus insertion. Original-field,
  caret, and editing-generation verification are deferred by user instruction.
- [x] AC-10: Preserve answers literally through interpolation, echo dependencies,
  and capitalization. Resolve dates once after valid submission. Prepare output
  and transport before deletion. Use the authored cursor marker only where
  reliable; otherwise place the cursor at the end.
- [x] AC-11: Replace keyword and committing Space once, preserving one trailing
  Space. Report interrupted or indeterminate insertion after possible mutation;
  never blindly retry or restore.
- [x] AC-12: Reject duplicate, expired, wrong-session, and cancelled replies.
  IDs cannot be reused.
- [x] AC-13: Report availability, pending state, sanitized errors, and request
  IDs through IPC/logs. Never log or persist answers, resolved text, expected
  keyword text, or ordinary typing. Send no desktop notifications.
- [x] AC-14: Restrict IPC to the daemon user and bound connections, frames,
  queues, fields, answers, and retained state. Slow clients cannot stall work.
- [x] AC-15: Test lifecycle, transport, rendering, privacy, cancellation, races,
  and native failures offline. Verify prompted insertion in controlled Chrome,
  Ghostty, and VS Code targets in isolation. These checks do not establish the
  deferred original-field guarantee.

## Timing and bounds

Use configurable defaults: acknowledgment 2 seconds, expected lease renewal
every 5 seconds, lease expiry 15 seconds, completion 5 minutes after
acknowledgment. Use monotonic time and check expiry on admission, not just timers.
Invalid submissions and renewals never extend the completion deadline.

Proposed limits: 16 connections; 64 KiB frames and aggregate requests; 32 queued
outbound messages per connection; 64 fields; 128 options per field; 4 KiB per
answer. Validate aggregate encoded size at configuration loading and runtime.
Reject output beyond the native bridge's 4,000-byte limit before deletion unless
that capability is safely extended.

The protocol must support register/registered, renewal, request/acknowledgment,
submit/cancel, one terminal result or cancellation, structured errors, and status.
Bind transactions to daemon instance, handler session, snippet revision,
and deadlines. Resolve choice IDs to configured values in the daemon.
Submission acceptance is not insertion success.

## Plan

- [x] P1: `src/config.rs`, `schemas/match.schema.json`,
  `schemas/config.schema.json`: add an optional ordered `fields` array to
  matches and nested `prompt` settings with defaults. Field shape:
  `{id, label, type: text|choice, default?, options?}`; choice options are
  `{id, label, value}`. Text defaults are strings; choice defaults are option
  IDs. Reference fields using existing `{{id}}` syntax. Empty text is valid;
  every declared field requires an answer even when a default exists. Reject
  unknown properties, duplicate/invalid IDs, collisions with variables or
  regex captures, invalid defaults, empty choice lists, text-field options,
  cycles, and nested references to prompted snippets. Preserve fields during
  configuration serialization and generated-file edits. Bound UTF-8 answer,
  definition, and full encoded request sizes, including protocol envelope
  overhead. Settings validate positive durations and bounded resource limits;
  the 4,000-byte prepared-output ceiling cannot be configured upward beyond
  supported transport capability. Tests cover schema/runtime parity and old
  configuration roundtrips. Covers AC-1/2/6/10/14.
- [x] P2: `src/expander.rs`, `src/template.rs`, new `src/prompt.rs`: distinguish
  a deferred candidate from a rendered expansion at the existing matching
  boundary. Reuse compiled matches, suffix checks, captures, profiles, and
  dependency evaluation. Prompted matches wait for physical Space regardless
  of global Immediate/Space mode; never expand on immediate input, punctuation,
  Enter, or Tab. On Space, keep existing match order/boundary precedence and
  report prompted triggers unreachable because of immediate plain prefixes.
  Snapshot selected definitions, captures, typed-case metadata, original
  keyword plus Space, deletion count, and configuration revision in memory.
  Do not resolve dates until valid submission; share one render instant.
  Preserve authored/literal segment provenance through interpolation, echo,
  and nested plain matches. Transform authored text for propagated case while
  maintaining capitalization position across literal segments; answer bytes
  remain unchanged. Authored markers, including markers inherited from echo
  or nested plain snippets, remain cursor nodes; literal answer markers stay
  text. Reject multiple authored stops. Use the end fallback for unreliable
  multiline/grapheme positioning. Include one trailing committing Space and
  its effect on the cursor offset. Keep ordinary rendering behavior intact.
  Tests cover literal markers/braces, echoes, case boundaries, repeated
  answers, dates, defaults, regex captures, and default-mode matching. Covers
  AC-1/2/4/10/11.
- [x] P3: `src/ipc.rs`, `src/prompt.rs`: retain the existing socket and legacy
  command/response shapes. Add a first-frame `prompt\t` prefix carrying a
  strict JSON v1 registration/status message; subsequent prompt frames are
  newline-delimited JSON with explicit `version` and `type`. Each submission
  identifies its session/request and provides typed text or option-ID answers.
  Split listener acceptance from connection reading using bounded workers and
  channels. Keep buffered bytes across prompt frames; legacy connections
  remain one command per connection. Authenticate peer effective UID, bind
  session ownership to the connection, and create the socket mode 0600.
  Enforce frame limits before unbounded allocation, bounded incoming/outgoing
  queues, timed writes, and first-frame/legacy idle deadlines. Default initial
  frame deadline is 2 seconds; valid registered idle handlers use the lease
  instead. Cap total accepted connections at 16; one persistent handler;
  promptly refuse excess connections. Do not promise management admission
  under connection exhaustion. Accepted management requests must progress
  independently of stalled readers/writers, with bounded event scheduling.
  Workers write responses outside the daemon event loop. Do not echo raw JSON,
  parser errors, unknown properties, or enum values. Add prompt status without
  changing legacy status JSON. Tests cover legacy half-close, coalesced prompt
  frames, split frames, unauthorized peers, malformed input, full queues,
  slow clients, bounded overload, and cancellation-safe event delivery. Covers
  AC-2/3/5/7/12/13/14.
- [x] P4: `src/prompt.rs`: implement a single-owner transaction state machine
  with injectable monotonic time and effects returned to the daemon. Support
  register/registered, renewal, request/ack, submit, accepted/validation_error,
  closed, cancel, result, structured errors, and status. Use a daemon nonce
  from OS randomness plus nonreusing bounded counters for opaque session and
  request IDs; fail before counter wrap. States cover trigger release, ack,
  editing, preparation, submission-key release, closure, settling, queued
  commit, committing, and terminal outcome. Ack deadline starts on request
  dispatch; lease renewal and invalid submissions never extend the original
  completion deadline. Check expiry on every admission/effect, not only timer
  wakeups. Completion deadline includes preparation/closure/settling before
  mutation. Add configurable release/close timeouts (defaults 2 seconds each),
  bounded by completion and lease expiry where applicable. Invalid answers or
  unsupported output remain correctable within the same deadline. Freeze
  prepared output after acceptance and reject duplicate submits. Deliver one
  logical terminal outcome, best effort when a connection is unavailable;
  reject stale IDs without accumulating permanent tombstones. Tests exercise
  exact deadline boundaries and adversarial orderings. Covers AC-3 through
  AC-8 and AC-11 through AC-14.
- [x] P5: `src/daemon.rs`, minimal `src/main.rs` module wiring,
  `src/keyboard.rs` only where needed for held-key state: route prompt input
  before legacy shortcuts, profile refresh, pending expansion, and undo.
  Continue tracking physical held keys per device, including repeats and
  modifiers, while suppressing legacy decoding/matching. Seed held state on
  startup/hotplug or withhold admission until release state is known. Clear
  old input/undo state at prompt admission and termination. Dispatch a request
  only after the physical committing Space and held keys are released; verify
  delivery ordering in the isolated guest rather than assuming evdev ordering
  proves target delivery. After a valid prepared submission, send `accepted`
  only when all physical keys, including submitting Enter/Space, are released.
  Require a frontend `closed` assertion, then use a nonblocking configurable
  focus-settle interval (default 150 ms). New presses/repeats during closure,
  settling, or queued commit cancel before mutation. Cancel on disable/pause,
  manual or automatic reload even when loading fails, group/profile policy
  mutation, device loss, stream end, shutdown, and handler failure. Suspend
  profile refresh during form editing; reevaluate exclusions after closure.
  Reject competing manual paste while pending/committing. `list --json` may
  add optional field metadata only to new prompted rows; prompted `render`
  and legacy `paste` fail clearly with `requires_prompt`, since v1 admission
  is Space-triggered. Preserve existing plain CLI output. Relevant files also
  include `src/preview.rs`. Covers AC-2/4/5/7/8/9/12/13.
- [x] P6: `src/injector.rs`, `src/prompt.rs`, small daemon completion hooks:
  add an atomic prepared prompted operation to the existing injection worker.
  Do not call ordinary `inject_expansion` or compose the existing void methods
  into a purported reliable replacement. The existing queue is bounded to
  512 commands, but its blocking per-key sends and swallowed errors are
  unsuitable. Admit a whole bounded operation via nonblocking `try_send`.
  Worker-side preparation verifies output bytes (including trailing Space),
  operation count, cursor plan, transport availability, and complete character
  coverage before any deletion. Prepare dynamic Wayland text keymaps for answer
  characters while retaining configured characters and ordinary keycode
  restrictions. For uinput, preflight complete active-map coverage or an
  available direct transport; unsupported output leaves the form correctable
  without deletion. Never use lossy character skipping or post-deletion
  `wtype`. Keep one worker-owned prepared handle with expiry/drop cleanup.
  Reuse fallible transport primitives and existing timing policies; preserve
  ordinary commands. Snapshot the post-close transport policy. Keep target-
  independent preparation distinct from target-specific admission. Immediately
  before mutation, check revocation/deadlines and atomically claim Pending ->
  Committing against Pending -> Cancelled. Execute the replacement as one
  worker command, check failures for deletion/text/cursor/flush, release
  synthesized modifiers on failure, and sanitize backend errors. Fallback is
  allowed only after a proven pre-mutation refusal. Report `issued` for successful
  keyboard transport dispatch, without claiming application acknowledgment;
  distinguish direct committed, failed-before-mutation, and indeterminate or
  interrupted-after-possible-mutation outcomes. No automatic retry/restore.
  On acknowledgment timeout, emit at most one indeterminate outcome and keep
  the commit fence occupied until completion or confirmed worker retirement;
  drain late results without another terminal event. Shutdown revokes queued
  work; claimed work cannot be reported as cancelled-before-mutation. Bound
  shutdown observation and retain uncertainty if completion is unavailable.
  Fake transport/worker tests exercise preparation failure, full queue,
  cancellation-versus-claim, every native failure point, timeout, late success,
  shutdown, and no competing operation while uncertain. Covers AC-5/7/8/9/10/
  11/12/14/15.
- [x] P7: external `~/.config/quickshell/services/SnipExpandService.qml`,
  `services/qmldir`, `modules/snippets/SnippetPrompt.qml`, and `shell.qml`:
  add a Socket/SplitParser client for the same protocol. Follow existing
  singleton, Theme, Scrim, focused-monitor, and OverlayService conventions.
  A single choice field uses a searchable picker; multiple text/choice fields
  share one form. Support multiline Unicode text, defaults, explicit empty
  values, and option IDs. Retain the form on validation errors. Wait for
  daemon `accepted` after submission-key release before hiding. Disable
  keyboard focus and tear down the local surface before sending `closed`;
  this is a client assertion followed by daemon settling, not compositor or
  application proof. Escape, dismissal, or overlay displacement cancels.
  Disconnect closes the form and clears private state; reconnect obtains a
  new session without restoring answers. Renew the advertised lease. Make
  minimal additive edits to the already-modified `services/qmldir`; preserve
  all existing local work and older snippet probes. Do not reuse or rewrite
  the character picker, copy answers to clipboard, store recents, or log
  message/answer content. No Omarchy dependency. Use new dedicated offline
  test fixtures, not a second unused reference frontend. Covers AC-3/6/7/8/9/
  10/13/15.
- [x] P8: `README.md`, new `docs/prompt-ipc.md`, examples, focused tests in
  affected Rust modules, and a new Quickshell prompt test harness: document
  configuration, protocol lifecycle, bounds, terminal meanings, current-focus
  risk, unsupported transports, and paired binary/config rollback. Remove
  decoded-character logging in `src/daemon.rs` and ordinary per-key event
  debug logging in `src/keyboard.rs`; test debug/trace privacy with distinctive
  secret answers, malformed property/enum values, expected keyword text, and
  backend errors containing characters. Translate prompt helper failures into
  stable codes before returning/logging. No private-data Debug derives on
  transaction/answer containers. Run offline lifecycle/render/transport/race/
  privacy tests and full repository CI checks. Add a private offscreen QML
  harness with a fake daemon; isolate HOME/XDG directories and remove inherited
  Wayland/X11/D-Bus access. Then verify Chrome, Ghostty, and VS Code in a guest
  with guest-only keyboard, private Wayland/D-Bus/runtime, no host mounts/input
  devices/clipboard sharing. Verify actual Space delivery, frontend teardown,
  exact replacement plus trailing Space, defaults, multiline/Unicode, cancel,
  empty answers, handler loss, pause/reload, repeated attempts, and uncertain
  native failures. No host service/install or running-shell reload. Report
  missing acceptance checks as missing, never infer them from offline tests.
  Covers AC-1 through AC-15.

Implement serially in this dependency order. Keep cohesive prompt-specific
logic in the new module; split it only if its size warrants a directory. Avoid
duplicating the matcher/render engine, generic provider frameworks, or unrelated
cleanup. Preserve valid legacy wire bytes and CLI output. The core contribution
must build and test without Quickshell; its adapter remains a separate change
in the user's configuration repository. Upstream acceptance remains a maintainer
decision. Slug: `snipexpand-deferred-prompts`.

Never reproduce the disabled Quickshell/`hyprctl eval` probe or register
per-character Hyprland bindings. Rollback must restore both the previous binary
and compatible configuration.

## Validation

- [x] `cargo fmt --check`.
- [x] `cargo clippy --all-targets -- -D warnings`.
- [x] `cargo build`.
- [x] `cargo test` (includes focused protocol/state/render/privacy tests).
- [x] `cargo run -- --help`.
- [x] `cmake -S fcitx5-addon -B target/fcitx5-addon`.
- [x] `cmake --build target/fcitx5-addon`.
- [x] `ctest --test-dir target/fcitx5-addon --output-on-failure`.
- [x] In the Quickshell repository, add/run
  `python3 scripts/test-snipexpand-prompts.py --offline` with isolated service
  and UI fixtures. Do not run the existing disabled live preflight script.
- [x] `qmllint services/SnipExpandService.qml modules/snippets/SnippetPrompt.qml`
  with installed Quickshell import paths; classify missing import metadata
  separately from code errors and confirm actual loading in the offline harness.
- [x] Isolated Chrome, Ghostty, and VS Code acceptance.
- [x] Inspect `git diff --stat`, `git diff --check`, and `git status --short`
  in both repositories after implementation; check only intended paths changed.

Native tests require Fcitx development headers; a standalone suffix test still
depends on Fcitx utilities. If prerequisites are missing, report the check as
unavailable rather than claiming a dependency-free substitute.

## Outcome

2026-10-07: Implemented and independently validated against baseline
`84fa42c699f19a3630ca7f049dfea2956cde0b92`. Verdict: **PASS**, with the
transport capability limits below. P1–P8 and AC-1–AC-15 cover the amended
current-focus specification; the historical strict-field experiment remains
blocked and does not establish original-field safety.

Core changes are localized in new `src/fields.rs`, `src/prompt.rs`, and
`src/prompt_injection.rs`, with integration in configuration/schema loading,
matching/rendering, existing IPC, daemon routing, keyboard state and injection.
The same socket serves legacy management and authenticated versioned prompt
sessions. No dependency or lockfile change was needed. The prepared worker
operation validates before deletion, atomically races cancellation against first
mutation, and reports uncertainty without retrying. Rendering preserves literal
answer provenance. Prompt-specific app detection is asynchronous and bounded.
`docs/prompt-ipc.md` documents configuration, lifecycle, limits and rollback;
README links to it. Reproducible guest-only fixtures live in
`tests/prompt-acceptance/`.

External frontend changes are confined to
`~/.config/quickshell/services/SnipExpandService.qml`,
`modules/snippets/{SnippetPrompt.qml,qmldir}`,
`scripts/test-snipexpand-prompts.py`,
`tests/snipexpand-prompts/Harness.qml`, and additive registration/imports in
`services/qmldir` and `shell.qml`. Existing unrelated Home Assistant,
notification, research and older snippet-probe work is preserved. The frontend
uses existing Socket/SplitParser, Theme, Scrim and OverlayService conventions;
core builds/tests do not require Quickshell or Omarchy.

Fresh adversarial implementation review found and drove fixes for Loader/editor
focus, legacy custom separators, destination-profile timing, unbounded app
queries, and date-authored cursor validation. A separate fresh adversarial
validator verified the final Rust candidate and the final QML reconnect repair,
with no remaining findings. Live experiments additionally reproduced non-BMP
truncation and initial failed-socket recovery. Quickshell 0.3.1 retains a failed
native socket after an initial connection error; retries now recreate the Socket.
The private offscreen test starts its fake daemon late and tests reconnection.
These fixes were completed under the user's explicit instruction to implement
fully and fix all review/validation findings.

Validation results:

- `cargo fmt --check`: PASS.
- `cargo clippy --all-targets -- -D warnings`: PASS.
- `cargo build`: PASS.
- `cargo test`: PASS, 157 unit tests plus 16 authoring, 8 groups and 7 packs
  integration tests (188 total). Includes actual TRACE-log privacy capture,
  secret malformed fields/enums/backend errors, preparation failures,
  cancellation/native races, bounds, legacy compatibility and lifecycle.
- `cargo run -- --help`: PASS.
- Host native configure was unavailable because Fcitx5Core development files
  were absent. Inside the isolated guest with Fcitx 5.1.7 SDK,
  `cmake -S fcitx5-addon -B target/fcitx5-addon`,
  `cmake --build target/fcitx5-addon`, and
  `ctest --test-dir target/fcitx5-addon --output-on-failure`: PASS, 1/1 test.
- In the external Quickshell repository,
  `python3 scripts/test-snipexpand-prompts.py --offline`: PASS, including delayed
  startup, registration/renewal, defaults/empty/Unicode/multiline/literal answers,
  validation correction, picker search, surface closure, overlay cancellation,
  disconnect clearing and new-session registration. No desktop/device access.
- `qmllint -I /usr/lib/qt6/qml services/SnipExpandService.qml
  modules/snippets/SnippetPrompt.qml`: PASS, no diagnostics.
- Final intended-path `git diff --check`, `git diff --stat`, and
  `git status --short` inspections in both repositories: PASS. No generated
  build artifacts or unrelated user changes were added to this implementation.

Application acceptance used a disposable KVM guest with guest-only keyboards,
private Sway/Wayland/D-Bus/runtime, no shared host mounts, clipboard sharing,
input passthrough or host desktop sockets. Chrome 155.0.8059.39, Ghostty 1.3.1
and VS Code 1.141.0 each passed nine checks: repeated picker, form defaults,
empty answers, BMP Unicode/multiline/literal markers, Escape, disable, reload,
and unsupported non-BMP refusal followed by same-form correction. VS Code also
passed handler loss, no handler, and delayed daemon startup recovery: **30
subchecks passed**. Target fixtures verified actual physical Space before form
submission and exact replacement plus trailing Space/caret afterward. Real
Quickshell 0.3.1 controls/service ran in the guest; only the monitor selector was
adapted from Hyprland to the guest's single Sway screen.

Exact application commands:

```sh
python3 tests/prompt-acceptance/run.py \
  --qmp /tmp/snipexpand-acceptance/qmp.sock \
  --ssh-key /tmp/snipexpand-acceptance/key --ssh-port 22342 \
  --known-hosts /tmp/snipexpand-acceptance/known_hosts --target chrome
# Same command with --target ghostty.
# VS Code used --target vscode --lifecycle; its insertion/loss checks passed,
# then initial startup recovery failed and was fixed in production QML.
# The affected check alone then passed with --target vscode --startup-only.
```

Validated binary SHA256:
`c3f27208ad32075a6fa781da120424bb9bc76ef07219f66b26e89b842e10ed05`.
Final production service SHA256:
`2991f8e8d0d2a789a928f2be0a4112fbfb351450e3c32583549907d0e8c32b4c`.
Final production form SHA256:
`c64509014a006dba5f06df090bc0e3995d80b776b30d873e7e0738cb55602f1b`.
Unchanged successful insertion checks were not rerun after the socket-only fix;
the focused startup check and complete offscreen adapter test passed afterward.
Guest shutdown succeeded and QEMU exited; its overlay, disposable keys,
screenshots, sockets and temporary fixture directory were removed.

Capability limits and residual risks:

- Current-focus insertion intentionally does not verify original field/caret.
  Closing a frontend and waiting is not compositor/application authorization.
- The live VS Code test observed non-BMP virtual-keyboard keysym truncation.
  Prompt preparation now rejects non-BMP output, including many emoji, before
  deletion and keeps the form open for correction. BMP Unicode, multiline text
  and literal markers passed live tests. Renderer/frontend Unicode handling
  remains literal; unsupported transport coverage is a correctable refusal,
  never silent skipping or a claimed successful emoji insertion.
- Reliable authored cursor placement is conservative (ASCII, single line);
  other output uses the end fallback. Uinput requires complete active-map
  coverage. Prepared output remains bounded to 4,000 bytes including Space.
- `issued` means keyboard operations dispatched, not an application
  acknowledgment. Possible native mutation remains indeterminate and is never
  blindly restored/retried. Native failure checks use offline fake transports.
- No host binary installation, daemon/service restart, running-shell reload,
  Git commit/push, upstream submission or release was performed. Rollback must
  restore the previous binary together with compatible configuration.

## Historical outcome: strict P1 blocked, 2026-10-07

Baseline: `84fa42c699f19a3630ca7f049dfea2956cde0b92`, matching the handoff.
The initial working tree contained only untracked `FORK.md`, which was preserved.
No production source, host configuration, installed binary, or service was
changed. This record is the only repository addition.

Environment checks:

- `virsh -c qemu:///session list --all` and
  `virsh -c qemu:///system list --all` found no defined guests.
- `timeout 3s qemu-system-x86_64 -machine q35,accel=kvm -m 128 -nodefaults
  -display none -monitor none -serial none -S` ran until the intended timeout
  (exit 124), confirming QEMU could initialize KVM.
- A disposable qcow2 overlay used the existing Ubuntu 24.04 Vagrant image.
  QEMU ran with `-nodefaults -no-user-config`, emulated VGA/keyboard/tablet,
  private disk, loopback-only SSH forwarding, and a local QMP socket. No host
  input passthrough, filesystem sharing, SPICE, clipboard channel, guest agent,
  or host desktop sockets were configured.
- Guest `systemd-detect-virt` returned `kvm`. `/proc/bus/input/devices` listed
  QEMU/virtual input devices. `findmnt -rn -t 9p,virtiofs` returned no mounts.
- Only inside the disposable guest, installed Sway 1.9, Fcitx5 5.1.7,
  Google Chrome 155.0.8059.39, development headers, and a GTK form fixture.

The throwaway probe compiled using:

```sh
c++ -std=c++20 -fPIC -shared -Wl,--no-undefined \
  /home/vagrant/probe.cpp -o /home/vagrant/libp1probe.so \
  $(pkg-config --cflags --libs Fcitx5Module)
```

It exposes read-only `Capture` and `Inspect` D-Bus methods at
`/io/github/snipexpand/P1`, interface `io.github.snipexpand.P1`. It compares
context UUID, surrounding text and caret in memory and counts context events.
It has no deletion or insertion method and returns no text or UUID values.

Chrome ran natively with `--ozone-platform=wayland --enable-wayland-ime
--wayland-text-input-version=3`. QMP `send-key` generated guest hardware key
events. A guest screenshot showed the fixture and committing Space in field A;
the screenshot was discarded after inspection. This visual observation is not
an automated proof of Space delivery ordering relative to capture.

Calls used the guest's private desktop environment:

```sh
python3 /home/vagrant/desktop.py gdbus call --session \
  --dest org.fcitx.Fcitx5 --object-path /io/github/snipexpand/P1 \
  --method io.github.snipexpand.P1.Capture
python3 /home/vagrant/desktop.py gdbus call --session \
  --dest org.fcitx.Fcitx5 --object-path /io/github/snipexpand/P1 \
  --method io.github.snipexpand.P1.Inspect
```

Observed results:

- Initial capture returned `captured-without-surrounding`; inspection reported
  frontend `wayland_v2`, focused `1`, surrounding validity `0`.
- F6 switched from A to a distinct B field with identical fixture content and
  caret placement. `same_context` remained `1`; one focus-out, one focus-in,
  and one surrounding update were observed. The UUID identifies a broader
  input context, not the original DOM field.
- A GTK form focus roundtrip also produced one focus-out, one focus-in, and
  one surrounding update. Surrounding validity subsequently became `1`, but
  the prior capture was invalid, so equality against it is not safety evidence.
- After restarting the probe and retyping the fixture into A, capture again
  returned `captured-without-surrounding`. Inspection reported
  `surrounding_valid: 0` and `fixture_suffix_verified: 0`.

The current experiment cannot capture a verified original editing context or
authorize a safe commit across form focus. It therefore does not pass P1.
This does not establish impossibility for all bridge implementations or all
application/compositor versions. A stronger bridge remains unproven.

No insertion was attempted. Caret-change, programmatic-edit, recreation,
revocation races, Ghostty, VS Code, and successful commit acceptance remain
untested. The GTK window is a presentation fixture, not a completed IPC client.
Rust validation was not run because no Rust source or configuration was changed.

QEMU was stopped through QMP. Its overlay, serial log, and desktop captures were
removed. Throwaway source remains temporarily at
`/tmp/snipexpand-p1.Y2RO28/{probe.cpp,target.html,form.py,desktop.py,qmp.py}`;
that directory is ephemeral and is not part of the product.

The subsequent user instruction defers this strict gate. If strict safety is
reintroduced later, resume its experiment with trustworthy field identity,
editing generation, and authorization checked at the mutation boundary.
