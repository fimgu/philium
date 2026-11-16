# Agent execution and latency

## Long commands

Use `shell_run` with `background: true` for test suites, builds, servers and watchers. Poll the returned terminal ID using `shell_read`. Foreground commands wait at most 30 seconds (or a shorter requested `timeout_secs`) and then return without terminating the command. This response budget is deliberately below the MCP transport timeout.

A response with `running: true` is not a failure. Do not launch the command again. `shell_read` returns an object containing `terminal`, `output`, `state`, `running`, `finished` and `exit_code`. This replaces its former plain-text response. For persistent shells, status tracks a pending command rather than the lifetime of bash. `finished: null` denotes an idle shell without a pending command result. `finished: false` with `running: false` can mean interruption without a known exit code, not success.

Check `finished: true` and `exit_code: 0` before reporting a successful check. A nonzero exit code is a completed failure. Pending persistent-shell commands remain tracked when a caller disconnects, and another command is rejected while that pending command is unfinished. This is not global deduplication across separate shells or background terminals.

Keep output visible rather than piping a long command into `tail`. A quiet job is not automatically stuck. Inspect its existing terminal before retrying or interrupting it. Never terminate another agent's process.

## Effort and speed

Fast / Normal / Deep request low / default / high reasoning effort. These are requests, not guarantees of speed, cost or quality. Provider/model support varies. A successfully saved setting does not prove that an upstream service honors it. Lower effort does not remove provider queueing or local test execution time.

Reduce avoidable round trips by batching independent inspections and reading focused source ranges. Use focused tests before a broad suite. Do not interpret an idle agent or successful HTTP response as proof that its task is complete.

## Validation boundary

Focused real-PTY and MCP dispatcher tests cover command continuation, polling, exit status, follow-up commands, disconnected callers and peer read denial. Frontend fixture checks and builds are supporting regression evidence. Installed-app and live-provider acceptance must be performed separately before claiming the changes work end to end. Existing agents need the rebuilt app, and updated operating instructions apply when their runtime configuration is rebuilt.
