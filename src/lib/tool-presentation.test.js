import test from "node:test";
import assert from "node:assert/strict";
import { summarizeTool, cleanShellCommand, toolOutput, cleanTerminalText, shellStatus } from "./tool-presentation.js";

test("shell_run strips cd/export boilerplate, collapses newlines, and truncates long commands", () => {
  const s1 = summarizeTool("mcp_philium_shell_run", {
    command: "cd /Users/dev/project && export RUST_LOG=debug && cargo test --lib",
  }, { exit_code: 0, output: "ok\n" });
  assert.equal(s1.label, "Shell");
  assert.equal(s1.summary, "$ cargo test --lib");
  assert.equal(s1.outcome, "ok");
  assert.equal(s1.tone, "ok");

  const s2 = summarizeTool("shell_run", {
    command: 'cd "/Users/dev/my repo" && \\\n  npm run test \\\n  -- --watch=false',
  }, { exit_code: 0 });
  assert.equal(s2.summary, "$ npm run test -- --watch=false");

  const s3 = summarizeTool("shell_run", {
    command: "cd /tmp\nexport NODE_ENV=test\nnode --test src/lib/foo.test.js",
  }, { exit_code: 0 });
  assert.equal(s3.summary, "$ node --test src/lib/foo.test.js");

  // Standalone cd is preserved when not followed by another command
  assert.equal(cleanShellCommand("cd /Users/dev/project"), "cd /Users/dev/project");

  // Long command truncation (~84 chars with ellipsis)
  const longCmd = "cargo test --workspace --all-targets -- --nocapture very_long_test_filter_name_exceeding_limit_12345";
  const s4 = summarizeTool("shell_run", { command: longCmd }, undefined);
  assert.ok(s4.summary.length <= 84, `expected <= 84 chars, got ${s4.summary.length}`);
  assert.ok(s4.summary.endsWith("…"));
  assert.equal(s4.outcome, "Running…");
  assert.equal(s4.tone, "busy");

  // Explicit description or title takes precedence
  const s5 = summarizeTool("shell_run", { command: "echo hi", description: "Verify build output" }, { exit_code: 0 });
  assert.equal(s5.summary, "Verify build output");
  const s6 = summarizeTool("shell_run", { command: "echo hi", title: "Start dev server" }, { running: true });
  assert.equal(s6.summary, "Start dev server");
  assert.equal(s6.outcome, "still running");
  assert.equal(s6.tone, "neutral");
});

test("shell_run error exit code includes truncated last non-empty error line", () => {
  const mcpResult = {
    content: [{
      type: "text",
      text: JSON.stringify({
        exit_code: 1,
        output: "compiling foo\n\x1b[31merror[E0425]: cannot find value `bar` in this scope\x1b[0m\n\n",
      }),
    }],
  };
  const s = summarizeTool("mcp_philium_shell_run", { command: "cargo check" }, mcpResult);
  assert.equal(s.label, "Shell");
  assert.equal(s.tone, "bad");
  assert.match(s.outcome, /^exit 1 · error\[E0425\]: cannot find value `bar` in this scope$/);

  const veryLongErr = {
    exit_code: 2,
    output: "FAIL " + "x".repeat(120) + "\n",
  };
  const sLong = summarizeTool("shell_run", { command: "npm test" }, veryLongErr);
  assert.equal(sLong.tone, "bad");
  assert.ok(sLong.errorHint.length <= 60);
  assert.ok(sLong.errorHint.endsWith("…"));
  assert.equal(sLong.outcome, `exit 2 · ${sLong.errorHint}`);
});

test("surface_exec and shell_read summarize concisely", () => {
  const appCmd = summarizeTool("mcp_philium_surface_exec", {
    id: "surf_1",
    command: "cd /app && pytest -q",
  }, { exit_code: 0 });
  assert.equal(appCmd.label, "App cmd");
  assert.equal(appCmd.summary, "$ pytest -q");
  assert.equal(appCmd.outcome, "ok");

  const readRunning = summarizeTool("mcp_philium_shell_read", { id: "term_1" }, {
    running: true,
    output: "step 1\nlistening on http://localhost:3000\n",
  });
  assert.equal(readRunning.label, "Shell output");
  assert.equal(readRunning.summary, "listening on http://localhost:3000");
  assert.equal(readRunning.outcome, "still running");
});

test("code navigation tools include query/symbol/path and optional [repo] prefix", () => {
  const find = summarizeTool("mcp_philium_code_find", { query: "summarizeTool", repo: "philium" }, { hits: [] });
  assert.equal(find.label, "Code find");
  assert.equal(find.summary, "[philium] summarizeTool");

  const sym = summarizeTool("code_symbol", { symbol: "Tool", path: "src/ui/conversation.jsx" }, {});
  assert.equal(sym.label, "Symbol");
  assert.equal(sym.summary, "Tool · src/ui/conversation.jsx");

  const outline = summarizeTool("code_outline", { path: "src/main.jsx" }, {});
  assert.equal(outline.label, "Outline");
  assert.equal(outline.summary, "src/main.jsx");

  const refs = summarizeTool("code_refs", { symbol: "fold", repo: "core" }, {});
  assert.equal(refs.label, "Refs");
  assert.equal(refs.summary, "[core] fold");

  const map = summarizeTool("code_map", { repo: "core" }, {});
  assert.equal(map.label, "Code map");
  assert.equal(map.summary, "[core] repo root");

  const ctx = summarizeTool("repo_context", { branch: "feat/scroll", repo: "philium" }, {});
  assert.equal(ctx.label, "Repo");
  assert.equal(ctx.summary, "[philium] branch feat/scroll");
});

test("file tools format path and line range cleanly", () => {
  const r = summarizeTool("fs_read", { grant_id: "docs", path: "src/ui/conversation.jsx", start_line: 95, end_line: 156 }, "ok");
  assert.equal(r.label, "Read");
  assert.equal(r.summary, "src/ui/conversation.jsx:95-156");

  const e = summarizeTool("edit", { file_path: "src/styles/agents.css" }, "ok");
  assert.equal(e.label, "Edit");
  assert.equal(e.summary, "src/styles/agents.css");

  const w = summarizeTool("fs_write", { grant_id: "notes", path: "todo.md", content: "hi" }, "ok");
  assert.equal(w.label, "Write");
  assert.equal(w.summary, "todo.md");

  const l = summarizeTool("fs_list", { grant_id: "docs", path: "guides", glob: "*.md" }, "ok");
  assert.equal(l.label, "List");
  assert.equal(l.summary, "guides *.md");

  const s = summarizeTool("fs_search", { grant_id: "docs", query: "migration", glob: "*.sql" }, "ok");
  assert.equal(s.label, "Search");
  assert.equal(s.summary, "migration in *.sql");
});

test("memory, working note, and access tools produce 1-line intent summaries", () => {
  const slate = summarizeTool("memory_slate", { q: "scroll", repo: "philium" }, {});
  assert.equal(slate.label, "Memory");
  assert.equal(slate.summary, "[philium] slate: scroll");

  const search = summarizeTool("memory_search", { q: "auto scroll" }, {});
  assert.equal(search.label, "Memory");
  assert.equal(search.summary, "auto scroll");

  const save = summarizeTool("memory_save", { category: "decision", fact_key: "scroll.mode", content: "Use ResizeObserver for timeline growth" }, {});
  assert.equal(save.label, "Remember");
  assert.equal(save.summary, "[decision] scroll.mode: Use ResizeObserver for timeline growth");

  const wg = summarizeTool("working_note_get", { repo: "philium" }, {});
  assert.equal(wg.label, "Handoff");
  assert.equal(wg.summary, "[philium] read working note");

  const ws = summarizeTool("working_note_set", { decisions: "x", next_steps: "y" }, {});
  assert.equal(ws.label, "Handoff");
  assert.equal(ws.summary, "update decisions, next_steps");

  const acc = summarizeTool("access_request", { target: "docs-repo", scopes: ["read", "code"], reason: "Inspect API schema" }, {});
  assert.equal(acc.label, "Access");
  assert.equal(acc.summary, "docs-repo · (read, code) · Inspect API schema");
});

test("failed tool result with error field or meta.ok=false sets bad tone", () => {
  const s1 = summarizeTool("fs_read", { path: "secret.txt" }, { error: "Permission denied" }, "", { running: false, ok: false });
  assert.equal(s1.tone, "bad");
  assert.equal(s1.outcome, "failed · Permission denied");

  const s2 = summarizeTool("custom_tool", { title: "Check health" }, undefined, "", { running: true, active: false });
  assert.equal(s2.label, "custom_tool");
  assert.equal(s2.summary, "Check health");
  assert.equal(s2.outcome, "unconfirmed");
  assert.equal(s2.tone, "neutral");

  // Existing exports still behave identically
  assert.equal(toolOutput({ stdout: "a", stderr: "b" }), "a\nb");
  assert.equal(cleanTerminalText("\x1b[32mok\x1b[0m\r\n"), "ok\n");
  assert.equal(shellStatus({ exit_code: 0 }), "Exit 0");
});
