# Gap grounding: worker brief

You are grounding ONE gap from `docs/GAPS.md`. Your gap ID is in your task.
This is a read-and-write-docs task. Do not change product code.

## Do
1. Read the shared context: `AGENTS.md`, `docs/LANES.md` (ground rules),
   `docs/GAPS.md` (your item, "Where fixes can live", brief format), and
   skim `docs/SPEC.md` headings only.
2. Verify the claim against real code in Philium (`~/Developer/philium`) and
   HAL (`~/Developer/hal`, read-only). Search first (`agentgrep`, `rg`), then
   read only the regions you need. Record exact file:line evidence.
   Treat the audit as a hypothesis: it may be wrong, partly right or stale.
   Also look for partial implementations already present.
3. You may run cheap, read-only commands (grep, `git log -S`, `cargo tree`,
   listing files). Do not run builds, the app, or anything that writes
   outside `docs/gaps/`.
4. Write `docs/gaps/<ID>.md` in the "Grounding brief format" from GAPS.md
   (Verdict, Current state, Where the fix lives, Proposed design, Acceptance
   checks, Dependencies and risks, Size). Keep it under ~150 lines. Cite
   file:line for every factual claim. Acceptance checks must include a real
   user workflow through the running app, not only unit tests.
5. Respect AGENTS.md: no hardcoded versions, model IDs or prices outside
   `src-tauri/src/pins.rs`. Designs must resolve them at runtime.
6. Commit only your file: `git add docs/gaps/<ID>.md && git commit -m "docs(gaps): ground <ID>"`.
   Never `git add -A`, never touch other files, never rewrite history.
   No `Co-authored-by` trailers or agent signatures. No em dashes.
7. Report back in 3 to 5 lines: verdict, where the fix lives, size, and any
   finding that changes the item's ranking or affects another gap ID.
