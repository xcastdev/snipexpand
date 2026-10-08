# Safe deferred prompt insertion

Research date: 2026-10-07. Repository baseline:
`84fa42c699f19a3630ca7f049dfea2956cde0b92`.

## Recommendation

Use application integrations that capture and replace text in the component
that owns the editable buffer. Keep SnipExpand responsible for snippet matching,
transactions, deadlines, form IPC, validation of answers, and rendering. Give
the original application a narrow operation to validate and replace a captured
range once.

For the three requested targets, investigate a Chrome extension for supported
web controls, a VS Code extension for editor documents, and a Zsh ZLE integration
for shell prompts inside Ghostty. These are proposed integrations, not verified
implementations. They require an implementation-plan revision and fresh P1
experiments. The existing production gate remains blocked.

The sources examined do not establish a universal technique satisfying the
unchanged specification in arbitrary applications. In particular, externally
observing focus/text and later issuing input is weaker than validating and
mutating within the original buffer owner. Application integrations provide the
best starting point, but each still has limits described below.

## Verified capabilities and gaps

### Fcitx is useful transport, not guaranteed field identity

Fcitx documents that an input context can represent a window or a text field,
depending on the application. Its UUID therefore does not universally identify
an individual editable field. Its API exposes surrounding text, cursor geometry,
deletion, and string commits; the mutation methods do not accept an expected
field revision. [Fcitx `InputContext`, lines 41–44 and 153–183](https://github.com/fcitx/fcitx5/blob/master/src/lib/fcitx/inputcontext.h#L41).

Inference: adding a counter to the daemon or addon only counts events that reach
it. It does not establish an authoritative application editing generation or
make a later application-side mutation conditional on that generation.

### Wayland batching is not conditional replacement

Text-input-v3 batches deletion and insertion around the current cursor. Its
`done` description explicitly requires applying edits even when the serial
differs from the client's count of commits, while treating protocol state
synchronization separately. The mutation interface has no captured DOM field
ID, expected text, or expected editing-generation argument.
[Authored text-input-v3 XML, lines 346–407](https://github.com/wayland-mirror/wayland-protocols/blob/main/unstable/text-input/text-input-unstable-v3.xml#L346).

The original input-method-v2 XML likewise does not describe its serial as an
application-side compare-and-replace condition. It provides input popup surfaces
and a hardware keyboard grab, and allows only one input method per seat.
[Authored wlroots protocol XML, lines 249–308](https://github.com/swaywm/wlroots/blob/master/protocol/input-method-unstable-v2.xml#L249).

Inference: an IME-style form could route typing to an independent presentation
component while preserving ordinary target focus. This is worth considering as
a UI alternative. It cannot by itself supply missing surrounding text, field
identity, or mutation-time validation. It also differs from P1's specified form
focus roundtrip and must not silently replace that requirement.

The experimental protocol family does not supply the missing guarantee either:
`xx_text_input_v3.done` ignores serial from version 2; its version 1 description
also applies changes on a mismatching serial. The linked XML is the authoritative
source; its rendered documentation was readable during research, while direct
GitLab fetching was blocked. No installed support was verified.
[Experimental text-input source](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/experimental/xx-text-input/xx-text-input-v3.xml).

### Accessibility can identify objects, but reads and writes race

AT-SPI has object-specific text/caret queries and editable-text operations.
`DeleteText` takes offsets; `InsertText` takes a position and text;
`SetTextContents` takes text. None accepts an expected revision or expected
existing value. [AT-SPI Text XML](https://github.com/GNOME/at-spi2-core/blob/main/xml/Text.xml),
[EditableText XML, lines 8–54 and 86–103](https://github.com/GNOME/at-spi2-core/blob/main/xml/EditableText.xml#L8).

Inference: this can improve target discovery and diagnostics, but a programmatic
edit can happen between a separate read and write. Two separate deletion and
insertion calls also permit partial mutation. Accessibility alone does not prove
the specification's mutation-time guard.

### VS Code has the strongest existing version check

The extension API exposes a retained `TextEditor`, document identity, selections,
and a document version that increases for every edit, including undo/redo.
[VS Code document API](https://code.visualstudio.com/api/references/vscode-api#TextDocument).

The edit builder captures the current document version before invoking the
extension callback and sends it with the edit to the main thread.
[Extension-host implementation, lines 48–69, 459–465 and 628–632](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/api/common/extHostTextEditor.ts#L48).

The main-thread `applyEdits` implementation rejects a changed model version and
an unavailable editor before executing the edit. It does not take an expected
selection or prompt-token revocation argument.
[Main-thread implementation, lines 441–471](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/api/browser/mainThreadEditor.ts#L441).

Recommended experiment: retain the original editor/document instance, captured
version, range, and selection; invalidate on relevant editor changes; verify
the captured version inside the synchronous edit callback; use literal range
replacement. The model version protects the subsequent document-edit race.
Caret movement and revocation between extension-host validation and renderer
execution still need proof or additional renderer-side cooperation. Do not
claim the public extension API alone meets all strict race requirements.

### Chrome supports exact object targeting and synchronous range edits

Content scripts can retain references to actual DOM objects in their isolated
execution environment. Extension messages can target a particular tab and
document through `documentId`, rather than broadcasting to whichever frame is
active. [Chrome content scripts](https://developer.chrome.com/docs/extensions/develop/concepts/content-scripts#work_in_isolated_worlds),
[Chrome document-targeted messaging](https://developer.chrome.com/docs/extensions/reference/api/tabs#method-sendMessage).

For ordinary supported inputs/textareas, `setRangeText` replaces a specified
range and controls the resulting selection. It uses UTF-16 code-unit offsets.
[HTML range-editing algorithm](https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-setrangetext).

Recommended experiment: retain the original element object, document, value,
selection and captured range in memory; reject disconnected/replaced elements,
navigation, unsupported input types, and changed context. Validate and invoke
the native range operation synchronously without awaiting between them. This
avoids the external read/write gap for the immediate operation; it is not a
universal editing-history guarantee.

Important limit: programmatic assignments to `.value` do not necessarily fire
`input` events. An event observer is therefore insufficient to detect every
edit-and-restore sequence. Rich editors also maintain their own models; a DOM
change is not automatically a successful model edit.
[Mozilla's input-event documentation](https://developer.mozilla.org/en-US/docs/Web/API/Element/input_event).

Strict editing-generation guarantees require cooperation from the supported
control/editor model or additional browser-side instrumentation that is itself
proven reliable. Limit support explicitly; do not promise arbitrary websites,
browser chrome, closed shadow trees, or all contenteditable editors.

Chrome native messaging can connect an extension to a small local helper, which
could forward to SnipExpand's authenticated Unix socket. Content scripts must
route native messaging through the extension rather than opening it directly.
This transport is independent of the form frontend.
[Chrome native messaging](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging).

### Ghostty requires cooperation from the program editing the text

Ghostty's inspected GTK surface wiring includes IME preedit and commit handlers;
the inspected surface implementation does not register surrounding-text retrieval
or surrounding-text deletion handlers. This source observation is not a runtime
test across all Ghostty versions.
[GTK IM context wiring](https://github.com/ghostty-org/ghostty/blob/main/src/apprt/gtk/ui/1.2/surface.blp),
[GTK surface implementation](https://github.com/ghostty-org/ghostty/blob/main/src/apprt/gtk/class/surface.zig).

Zsh ZLE exposes writable `BUFFER` and `CURSOR`, custom widgets, and descriptor
callbacks. [ZLE widget and buffer APIs](https://zsh.sourceforge.io/Doc/Release/Zsh-Line-Editor.html#User_002dDefined-Widgets).

Recommended first experiment: a Space widget performs normal insertion first,
captures that ZLE invocation and buffer/caret, then waits for the independent
form. Suspend only that shell's line editing; keep the SnipExpand daemon
responsive. On valid completion, check and replace the same owned buffer,
assign the authored cursor position, and redraw. Do not run `accept-line`, emit
the answer into the PTY stream, or synthesize Backspaces. Handle signals,
timeouts, editor exit, and trap/callback reentrancy before claiming safety.

This is shell-prompt support inside Ghostty. Neovim, other TUIs, remote shells,
or arbitrary programs reading stdin need their own integrations. Bracketed
paste does not add editable-buffer identity or a replacement precondition.
Ghostty documents that bracketed-paste mode depends on the running program.
[Ghostty bracketed-paste reference](https://ghostty.org/docs/config/reference#clipboard-paste-bracketed-safe).

### Existing form expansion uses weaker focus assumptions

Espanso's configuration schema includes `post_form_delay`, described as a delay
after closing a form to let the target application regain focus. That is evidence
of a conventional focus-return strategy, not evidence of field-specific
transactional replacement. [Espanso schema](https://github.com/espanso/espanso/blob/dev/schemas/config.schema.json).

Inference: following that model could produce useful everyday forms with fewer
integrations, but it would not satisfy the supplied strict AC-9/P1 contract.
It is a different product decision requiring an explicit specification change.

## Proposed interface and upstream compatibility

The following is a design recommendation, not an existing API:

1. An application integration captures an opaque, one-use context ticket after
   the committing Space is present. Keep field references and edit state in the
   owner process. Correlate that ticket to the daemon's physical trigger event;
   do not capture whichever field is active later.
2. SnipExpand sends fields to the independent frontend and manages the pending
   request. The context integration and presentation frontend have separate roles.
3. SnipExpand renders validated answers and requests a guarded replacement using
   the ticket. The owner validates immediately before changing its buffer.
4. The owner returns committed, refused-without-mutation, or indeterminate.
   Cancellation/revocation must be serialized with the commit authorization;
   an IPC cancellation notification alone does not revoke work already executing.

A proposed `replace_if_unchanged(ticket, replacement, cursor)` operation must
validate owner/session, field lifetime, edit generation, range, caret policy,
deadline, and one-use authorization. The adapters examined do not all expose
every precondition natively; missing conditions remain feasibility work.

Keep the new interface and adapters in separate modules/packages. Ordinary
snippets retain their existing injector. This reduces changes to upstream
injection code and permits capability-specific prompt support. It adds adapter
setup and narrows supported editing contexts, which must be stated explicitly
in a revised plan rather than silently treating every application as supported.

## Next feasibility experiments

Start with Zsh ZLE in Ghostty because it directly owns buffer and cursor and can
freeze that invocation during the form. Then test VS Code's versioned replacement
and its caret/revocation race. Finally test Chrome's exact element targeting,
programmatic mutation detection, and supported widget models.

For each target, require successful insertion plus rejection of identical-owner
switches, target recreation, caret changes, edit-and-restore, teardown, deadline
expiry, and cancelled queued work. Keep Space-delivery ordering and event
correlation as explicit tests. Successful cancellation alone does not pass P1.

This research changed no production source or host configuration and performed
no new application acceptance run. It does not lift the blocked implementation
status recorded in the local work record. Upstream-source observations describe
the fetched branch heads; verify against the exact installed application versions
before relying on implementation details.

## Existing picker focus flow

Follow-up source investigation: 2026-10-07. Read the upstream
`silouanwright/snipexpand-omarchy` plugin through authenticated, read-only
`gh api` calls, and Omarchy's shared UI implementation on its `quattro` branch.
This traces source behavior; it is not a live test of the user's installed shell.

The picker does not retain or explicitly restore an original editable field:

1. `Panel.qml.pasteSnippet` calls `root.close()` and then
   `controller.pasteSnippet(snippet.trigger, snippet.source)`.
   [Plugin panel, lines 55–58](https://github.com/silouanwright/snipexpand-omarchy/blob/main/Panel.qml#L55).
2. The controller launches `snipexpand paste`, passing an optional source and
   the trigger. It passes no window, field, caret, context ticket, or revision.
   [Plugin controller, lines 157–165](https://github.com/silouanwright/snipexpand-omarchy/blob/main/SnipExpandController.qml#L157).
3. Shared `Panel.close()` calls `panelController.hide()`, which sets `open`
   to false. [Shared Panel](https://github.com/basecamp/omarchy/blob/quattro/shell/Ui/Panel.qml#L24),
   [PanelController](https://github.com/basecamp/omarchy/blob/quattro/shell/Ui/PanelController.qml#L15).
4. `KeyboardPanel` drops layer-shell keyboard ownership immediately on logical
   close by setting keyboard focus to `None`, even if its fade-out keeps the
   panel mapped. Its open-state policy uses Exclusive initially and then
   OnDemand. There is no saved application-window focus restoration in this
   close path. [KeyboardPanel, lines 87–100](https://github.com/basecamp/omarchy/blob/quattro/shell/Ui/KeyboardPanel.qml#L87).
5. The Rust CLI sleeps for its default 150 ms before sending the paste request.
   This is a fixed settle delay, not an acknowledgment that the old field has
   focus. [CLI option](../../../src/main.rs#L85),
   [CLI command](../../../src/main.rs#L345).
6. The daemon renders the selected configured snippet and uses ordinary
   `inject_expansion`. The manual expansion has `delete_count: 0` and empty
   `undo_text`; it inserts at the current target rather than replacing a
   previously captured keyword. [Paste handler](../../../src/daemon.rs#L331),
   [Manual expansion](../../../src/expander.rs#L293).
7. For its optional Fcitx path, the native addon asks for
   `lastFocusedInputContext()` when executing the request. It does not receive
   a retained picker-opening field identity. Keyboard paths likewise deliver
   to the current seat focus. [Native bridge](../../../fcitx5-addon/snipexpand.cpp#L68),
   [Ordinary injection](../../../src/daemon.rs#L619).

Inference: closing the panel lets the compositor return keyboard input to an
application, ordinarily the previously focused one. That application often
retains its own field and caret while blurred, so the existing picker can appear
to preserve the original insertion point without recording it. Neither the
plugin nor the traced close/paste path proves which field receives the result.
Actual compositor policy and application behavior still require runtime testing.

If a different window or field becomes active during the interval, or the
original field changes or is recreated, this path does not compare it with a
captured original context. Its success for ordinary snippet selection therefore
does not establish the deferred replacement guarantee in P1/AC-9. Reusing the
picker UI and close behavior is reasonable; reusing this paste path as strict
original-context authorization is not supported by the inspected source.
