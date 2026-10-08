# Deferred snippet prompts

Prompting is optional. SnipExpand detects a Space-triggered snippet and sends its
fields to a separately running frontend over its existing Unix socket. The
frontend presents a picker or form and returns answers. SnipExpand validates,
renders, and inserts the result. It never launches a frontend.

## Configuration

```yaml
matches:
  - trigger: ";hello"
    replace: "Hello {{name}}, your appointment is {{day}}.$|$"
    fields:
      - id: name
        label: Name
        type: text
        default: ""
      - id: day
        label: Day
        type: choice
        default: monday
        options:
          - id: monday
            label: Monday
            value: Monday
          - id: tuesday
            label: Tuesday
            value: Tuesday
```

Fields are ordered, uniquely named, and distinct from variable names and regex
captures. Choice defaults are option IDs. Empty text answers are valid. Every
declared field needs an explicit answer; a frontend may initialize it from the
default. Repeated references share an answer. Echo variables may refer to fields.
Nested plain snippets are supported; nested interactive snippets are rejected.

Prompted snippets wait for Space even when ordinary snippets use immediate mode.
Enter, Tab, and punctuation do not open prompts. An immediate plain prefix can
make a longer prompted trigger unreachable; configuration diagnostics report
that conflict. Plain and cursor-only snippets require no prompt handler.

Answers are literal data, including `{{...}}` and `$|$`, through echoes and
capitalization. Only authored text is transformed. Dates resolve at valid
submission. At most one authored cursor stop is supported; submitted marker text
does not count. When positioning is unreliable, the cursor stays at the end.
The replacement includes one trailing committing Space. Prompted `render` and
legacy `paste` report `requires_prompt`; v1 admission is physical Space only.
`list --json` includes optional field definitions on prompted rows.

## Connection and messages

Connect to `$XDG_RUNTIME_DIR/snipexpand.sock`. Legacy commands retain their wire
format and use one request per connection. A prompt connection begins with an
actual TAB after `prompt`, followed by a JSON object and LF:

```text
prompt<TAB>{"version":1,"type":"register"}<LF>
```

Later prompt messages are JSON objects followed by LF, without the prefix.
Multiline text uses JSON escapes. All messages use `version: 1`; unknown
properties, operations, and invalid shapes are rejected without reflecting their
content. The socket mode is 0600 and peer credentials must match the daemon user.
One live connection owns the registered session; IDs are opaque and cannot be
reused. A second handler receives `busy`. Reconnect creates a new session.

| Direction | Type | Additional properties |
| --- | --- | --- |
| Client → daemon | `register` | none |
| Daemon → client | `registered` | `session`, `lease_renewal_ms`, `lease_expiry_ms` |
| Client → daemon | `renew` | `session` |
| Daemon → client | `request` | `session`, `id`, `fields` |
| Client → daemon | `ack` | `session`, `id` |
| Client → daemon | `submit` | `session`, `id`, `answers` |
| Daemon → client | `validation_error` | `id`, `code` |
| Daemon → client | `accepted` | `id` |
| Client → daemon | `closed` | `session`, `id` |
| Client → daemon | `cancel` | `session`, `id` |
| Daemon → client | `result` | `id`, `outcome`, optional `code` |
| Client → daemon | `status` | none |
| Daemon → client | `status` | `available`, `pending` (ID or null) |
| Daemon → client | `error` | `code` |

A one-shot prompt status connection uses the same `prompt<TAB>` prefix. To submit:

```json
{"version":1,"type":"submit","session":"OPAQUE_SESSION","id":"OPAQUE_REQUEST","answers":{"name":{"type":"text","value":""},"day":{"type":"choice","option":"monday"}}}
```

The daemon resolves choice IDs to configured values. Extra/missing answers, wrong
answer types, unknown options, and oversized values are rejected before deletion.
Keep the form editable after `validation_error`. Invalid submissions and lease
renewals do not extend its completion deadline. Submission acceptance is not
insertion success.

## Closure and insertion

A request is dispatched after the committing physical Space is released. The
frontend acknowledges once the form is available. After valid answers and
transport preparation, the daemon waits for all physical keys to be released
before sending `accepted`. Keep the form focused until that message. Then remove
keyboard focus and tear down the local surface before sending `closed`.

`closed` is a client assertion, followed by a configurable settling interval; it
is not compositor or application acknowledgment. **Insertion uses the application
that receives focus after the form closes. Original field identity, unchanged
text, and caret position are not guaranteed.** Avoid moving or changing the target
while filling a form. New physical keyboard input during closure, settling, or
queued insertion invalidates the request.

Pause, reload (including failed reload), policy changes, device loss, shutdown,
handler loss, and expired deadlines cancel pending work. No handler means no
deletion. Cancellation preserves the keyword and Space as far as SnipExpand's own
actions are concerned; user or application edits are not restored. Prompt input
does not enter the ordinary matching or undo paths. Only one transaction is
active and further prompted matches are not queued. Competing manual paste is
refused while a transaction or uncertain insertion owns the worker.

The daemon prepares the whole replacement before deletion. Dynamic Wayland text
maps support submitted BMP Unicode. Non-BMP text, including many emoji, is
rejected before deletion because some applications truncate virtual-keyboard
keysyms; this version has no proven lossless prompt transport for it. Uinput
requires complete active-keymap coverage;
unsupported text leaves the form correctable. Cursor positioning is conservative:
the keyboard operation uses the authored stop for single-line ASCII output and
otherwise leaves the cursor at the end. Normal snippets retain their behavior.

Terminal `outcome` values:

| Value | Meaning |
| --- | --- |
| `cancelled` | Operation revoked before mutation |
| `issued` | Keyboard operations dispatched; application receipt is not acknowledged |
| `failed` | Injection could not begin |
| `indeterminate` | Editing may have started or its completion is uncertain |

There is one logical terminal outcome. Delivery is best effort after disconnect.
Never retry or restore after an indeterminate result. If a timeout occurs while
the injection worker could still mutate, its exclusive fence remains occupied
until completion or confirmed retirement; late results do not emit another
outcome. Queue admission is nonblocking and cancellation competes atomically with
the worker's claim immediately before its first editing event.

## Settings and limits

Set optional `prompt` keys in `config.yml`. Defaults:

| Key | Default |
| --- | ---: |
| `ack_timeout_ms` | 2000 |
| `lease_renewal_ms` | 5000 |
| `lease_expiry_ms` | 15000 |
| `completion_timeout_ms` | 300000 |
| `release_timeout_ms` | 2000 |
| `close_timeout_ms` | 2000 |
| `focus_settle_ms` | 150 |
| `max_connections` | 16 |
| `max_frame_bytes` | 65536 |
| `max_request_bytes` | 65536 |
| `max_queued_messages` | 32 |
| `max_fields` | 64 |
| `max_options` | 128 |
| `max_answer_bytes` | 4096 |
| `max_output_bytes` | 4000 |

Resource limits can be reduced but cannot exceed these supported ceilings.
Byte limits count UTF-8 and encoded JSON; full requests include envelope overhead.
The output limit includes the trailing Space. Timing uses monotonic time and is
checked at operation admission as well as timer wakeups. Completion starts on
acknowledgment and bounds preparation, closure, and settling before mutation.

Transport connection/frame/queue limits are captured at daemon startup; changes
require restart. Other prompt settings reload after cancelling existing work.
Unclassified clients have a fixed initial-frame deadline. Slow clients cannot
stall accepted management connections; connection exhaustion causes bounded
refusal, without a promise of admission under flooding.

Logs/status contain IDs, availability, and sanitized error codes. They do not
contain answers, rendered text, expected keyword text, or ordinary key events.
Frontends must not persist answers or log protocol payloads. No desktop
notifications, clipboard providers, scripts, or per-character compositor bindings
are involved.

## Frontends and rollback

Any same-user frontend can implement this protocol. Quickshell can use its
[Socket](https://quickshell.org/docs/v0.2.0/types/Quickshell.Io/Socket/) and
[SplitParser](https://quickshell.org/docs/v0.2.1/types/Quickshell.Io/SplitParser/)
types; SnipExpand has no Quickshell build or runtime dependency. The user's
Quickshell adapter lives in their separate configuration repository.

Before rolling back, restore the previous compatible configuration together with
the previous binary. Older versions reject `fields` and `prompt` settings rather
than silently ignoring them. Remove the external adapter's shell/component
registration if rolling back its configuration.
