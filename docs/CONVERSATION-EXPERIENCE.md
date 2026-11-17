# Conversation experience

Philium offers two presentations of the same session. Conversation emphasizes readable Markdown and compact tool entries. Terminal uses a dense chronological transcript with tool details expanded by default. Terminal is not a command shell.

The view preference is shared between Agents and Chat and stored locally as `philium.conversation.view`. Changing it must not recreate the session or composer, send a message, interrupt a job, change permissions, or discard a draft. If browser storage is unavailable, the preference works in memory.

## Output and activity contract

- Assistant replies render Markdown without executing raw HTML, loading remote images, or enabling script/data links. User and tool content remains literal.
- Copy reply copies source Markdown. Copy code copies code contents. Clipboard failures are visible, not reported as success.
- Waiting for model response, Writing response, Running tools, Retrying model request and Waiting for your approval derive from HAL events. Timers are locally observed time, not authoritative execution durations.
- `model.finished` finishes one model request, not the whole task. Only terminal turn outcomes stop the activity indicator.
- No thinking transcript is synthesized. The inspected HAL event schema does not expose a public reasoning-summary channel.
- Tool details support keyboard disclosure. Truncated output is labeled. Permissions remain explicit in both views.

## Reference comparison and remaining gaps

Source inspection: Jcode `09114e218`, HAL `efe1358`, Philium baseline `011a389`. This is not a claim of runtime parity.

| Area | Philium baseline | Remaining direction |
| --- | --- | --- |
| Readable/copyable output | Plain assistant text | Markdown and copy actions implemented in source |
| Tool activity | Expandable raw output and observed timers | Semantic summaries, result navigation, full-output access |
| Follow-ups | Stop exists, sending while busy is disabled | Explicit per-session queue, then deliberate steering support |
| Plans | Board exists, no inline plan renderer | Link live plan progress into conversation |
| Usage | Aggregate Stats, model usage recorded | Purpose-separated per-turn usage, no invented cost/context numbers |
| Recovery | Agent history retry exists, limited turn visibility | Visible provider retry and reconnect/error handling |
| History | Chat list and agent history pagination | Search and bookmark navigation |
| Attachments | Image paste/drop/previews already exist | Clear rejected-file feedback and large-paste ergonomics |

Claude Code's interaction loop and interruptible/queued input inform the conversational direction. OpenShell informs the separation between user interaction and enforced runtime policy. Philium must describe its actual enforcement, not imply equivalent guarantees from visual similarity.

## HAL boundary

Existing HAL events support the current view toggle and activity presentation without harness changes. Mid-turn steering needs runtime admission/ordering semantics. A local follow-up queue must never automatically resend a message after an uncertain POST result. Cancellation acknowledgement means cancellation requested, not verified process cleanup.

The inspected Anthropic adapter buffers a full response before emitting answer text. True progressive Anthropic output needs a separate SSE transport change with cancellation, byte-limit and protected-output regressions. A typing animation would hide, not fix, that limitation. Provider-visible reasoning summaries require an explicit supported channel, not extraction of private reasoning.

## Acceptance scope

Required checks: both view buttons and keyboard operation, same mounted composer/session, retained draft while busy, persistence after reload, Markdown/copy in both modes, live permission/retry/tool/failure transitions, stale-history handling, unsafe content, narrow/wide layout and existing messaging regressions.

Unit tests and controlled browser DOM tests are supporting evidence only. Installed macOS app, native clipboard, live-provider behavior and preference persistence across native restart remain unverified until a coordinated single-copy update/restart is authorized. Do not launch a build-folder app alongside the installed copy.
