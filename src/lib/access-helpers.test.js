import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  allowedScopesFor,
  isScopeAllowed,
  sanitizeScopes,
  defaultScopesForRequest,
  defaultScopesForGrant,
  requestAskedForWrite,
  availableLifetimes,
  defaultLifetime,
  lifetimeLabel,
  inferTargetLabel,
  targetDisplay,
  canApproveAccess,
  buildDecisionPayload,
  buildGrantPayload,
  isGrantActive,
  pendingAccessByAgent,
  agentNeedsUser,
  parseScopes,
  normalizeTargetKind,
} from "./access-helpers.js";

test("allowedScopesFor enforces target kind and strict security rules", () => {
  assert.deepEqual(allowedScopesFor("repo", false), ["read", "code", "context", "communicate"]);
  assert.deepEqual(allowedScopesFor("repo", true), ["read", "code"]);
  assert.deepEqual(allowedScopesFor("folder", false), ["read", "write"]);
  assert.deepEqual(allowedScopesFor("folder", true), ["read", "write"]);
  assert.deepEqual(allowedScopesFor("file", false), ["read", "write"]);
  assert.deepEqual(allowedScopesFor("file", true), ["read", "write"]);

  // write is NEVER allowed on a repo
  assert.equal(isScopeAllowed("write", "repo", false), false);
  assert.equal(isScopeAllowed("write", "repo", true), false);
  // context & communicate are disabled for Strict agents
  assert.equal(isScopeAllowed("context", "repo", true), false);
  assert.equal(isScopeAllowed("communicate", "repo", true), false);
  // code is repo-only
  assert.equal(isScopeAllowed("code", "folder", false), false);
  assert.equal(isScopeAllowed("code", "file", false), false);
});

test("defaultScopesForRequest keeps write OFF by default and strips disallowed scopes", () => {
  assert.deepEqual(defaultScopesForRequest(["read", "write"], "folder", false), ["read"]);
  assert.deepEqual(defaultScopesForRequest(["write"], "folder", true), ["read"]);
  assert.deepEqual(defaultScopesForRequest(["read", "code", "context", "communicate", "write"], "repo", false), [
    "read",
    "code",
    "context",
    "communicate",
  ]);
  assert.deepEqual(defaultScopesForRequest(["read", "code", "context", "communicate"], "repo", true), [
    "read",
    "code",
  ]);
  assert.deepEqual(defaultScopesForRequest([], "file", false), ["read"]);
  assert.equal(requestAskedForWrite({ requested_scopes: ["read", "write"] }), true);
  assert.equal(requestAskedForWrite({ requested_scopes: ["read"] }), false);
});

test("defaultScopesForGrant keeps write OFF and respects strict mode", () => {
  assert.deepEqual(defaultScopesForGrant("repo", false), ["read", "code"]);
  assert.deepEqual(defaultScopesForGrant("repo", true), ["read", "code"]);
  assert.deepEqual(defaultScopesForGrant("folder", false), ["read"]);
  assert.deepEqual(defaultScopesForGrant("file", true), ["read"]);
});

test("sanitizeScopes and parseScopes handle arrays, CSV, JSON, and canonical ordering", () => {
  assert.deepEqual(parseScopes(["write", "read"]), ["write", "read"]);
  assert.deepEqual(parseScopes('["code","read"]'), ["code", "read"]);
  assert.deepEqual(parseScopes("communicate, read"), ["communicate", "read"]);
  assert.deepEqual(sanitizeScopes(["communicate", "write", "read", "code"], "repo", false), [
    "read",
    "code",
    "communicate",
  ]);
  assert.deepEqual(sanitizeScopes(["communicate", "context", "code", "read"], "repo", true), [
    "read",
    "code",
  ]);
  assert.equal(normalizeTargetKind("REPO"), "repo");
  assert.equal(normalizeTargetKind("unknown"), "folder");
});

test("availableLifetimes hides persistent when no repo_id is present", () => {
  assert.deepEqual(
    availableLifetimes({ hasRepo: false }).map((o) => o.value),
    ["once", "session"],
  );
  assert.deepEqual(
    availableLifetimes({ hasRepo: true }).map((o) => o.value),
    ["once", "session", "persistent"],
  );
  assert.equal(defaultLifetime(), "session");
  assert.equal(defaultLifetime({ forRepoScreen: true }), "persistent");
  assert.equal(lifetimeLabel("once"), "Once");
  assert.equal(lifetimeLabel("session"), "This agent");
  assert.equal(lifetimeLabel("persistent"), "Always for repo");
});

test("canApproveAccess requires a resolved target and at least one allowed scope", () => {
  assert.equal(
    canApproveAccess({ targetKind: "folder", targetPath: "", scopes: ["read"], strict: false }),
    false,
  );
  assert.equal(
    canApproveAccess({ targetKind: "folder", targetPath: "/tmp/docs", scopes: [], strict: false }),
    false,
  );
  assert.equal(
    canApproveAccess({ targetKind: "repo", targetRepoId: "r1", scopes: ["write"], strict: false }),
    false,
  );
  assert.equal(
    canApproveAccess({ targetKind: "repo", targetRepoId: "r1", scopes: ["context"], strict: true }),
    false,
  );
  assert.equal(
    canApproveAccess({ targetKind: "repo", targetRepoId: "r1", scopes: ["read", "code"], strict: true }),
    true,
  );
  assert.equal(
    canApproveAccess({ targetKind: "folder", targetPath: "/tmp/docs", scopes: ["read", "write"], strict: true }),
    true,
  );
});

test("buildDecisionPayload sets strict_allowed for strict persistent approvals and omits extra fields on deny", () => {
  assert.deepEqual(buildDecisionPayload({ requestId: "req-1", decision: "deny" }), {
    request_id: "req-1",
    decision: "deny",
  });

  const strictPersist = buildDecisionPayload({
    requestId: "req-2",
    decision: "approve",
    targetKind: "repo",
    targetRepoId: "repo-b",
    targetPath: "/code/repo-b",
    label: "repo-b",
    scopes: ["read", "code", "context", "write"],
    lifetime: "persistent",
    strict: true,
  });
  assert.deepEqual(strictPersist, {
    request_id: "req-2",
    decision: "approve",
    target_kind: "repo",
    target_repo_id: "repo-b",
    target_path: "/code/repo-b",
    label: "repo-b",
    scopes: ["read", "code"],
    lifetime: "persistent",
    strict_allowed: true,
  });

  const standardSession = buildDecisionPayload({
    requestId: "req-3",
    decision: "approve",
    targetKind: "folder",
    targetPath: "/Users/me/Design",
    label: "Design",
    scopes: ["read", "write"],
    lifetime: "session",
    strict: false,
  });
  assert.equal("strict_allowed" in standardSession, false);
  assert.deepEqual(standardSession.scopes, ["read", "write"]);
});

test("buildGrantPayload enforces scope filtering and strict_allowed semantics", () => {
  const payload = buildGrantPayload({
    sourceRepoId: "repo-a",
    targetKind: "repo",
    targetRepoId: "repo-b",
    targetPath: "/code/repo-b",
    label: "repo-b",
    scopes: ["read", "code", "write"],
    lifetime: "persistent",
    strictAllowed: true,
  });
  assert.deepEqual(payload, {
    source_repo_id: "repo-a",
    target_kind: "repo",
    target_repo_id: "repo-b",
    target_path: "/code/repo-b",
    label: "repo-b",
    scopes: ["read", "code"],
    lifetime: "persistent",
    strict_allowed: true,
  });
});

test("pendingAccessByAgent, agentNeedsUser, isGrantActive, and label helpers behave cleanly", () => {
  const counts = pendingAccessByAgent([
    { id: "1", agent_id: "a1", status: "pending" },
    { id: "2", agent_id: "a1", status: "pending" },
    { id: "3", agent_id: "a2", status: "approved" },
    { id: "4", agent_id: "a3" },
  ]);
  assert.equal(counts.get("a1"), 2);
  assert.equal(counts.get("a2"), undefined);
  assert.equal(counts.get("a3"), 1);

  assert.equal(agentNeedsUser({ id: "a1", state: "running" }, counts), true);
  assert.equal(agentNeedsUser({ id: "a2", state: "waiting" }, counts), true);
  assert.equal(agentNeedsUser({ id: "a2", state: "running" }, counts), false);

  assert.equal(isGrantActive({ id: "g1", revoked_at: null, uses_remaining: null }), true);
  assert.equal(isGrantActive({ id: "g2", revoked_at: "2026-01-01" }), false);
  assert.equal(isGrantActive({ id: "g3", revoked_at: null, uses_remaining: 0 }), false);
  assert.equal(isGrantActive({ id: "g4", revoked_at: null, uses_remaining: 1 }), true);

  assert.equal(inferTargetLabel("folder", "", "/Users/filip/Docs/"), "Docs");
  assert.equal(inferTargetLabel("repo", "philium", "/Users/filip/philium"), "philium");
  assert.equal(targetDisplay({ resolved_repo_name: "api", target_hint: "api repo" }), "api");
  assert.equal(targetDisplay({ target_hint: "design folder" }), "design folder");
});

test("UI components wire the exact backend access RPC and event contracts", () => {
  const requestsSrc = readFileSync(new URL("../ui/access-requests.jsx", import.meta.url), "utf8");
  const agentsSrc = readFileSync(new URL("../screens/agents.jsx", import.meta.url), "utf8");
  const contextSrc = readFileSync(new URL("../ui/agent-context.jsx", import.meta.url), "utf8");
  const reposSrc = readFileSync(new URL("../screens/repos.jsx", import.meta.url), "utf8");
  const storeSrc = readFileSync(new URL("./store.js", import.meta.url), "utf8");

  for (const method of ["access.pending", "access.decide", "access.grant_create", "access.catalog", "app.pick_folder"]) {
    assert.ok(requestsSrc.includes(`"${method}"`), `access-requests.jsx should call ${method}`);
  }
  for (const topic of ["access:requested", "access:changed"]) {
    assert.ok(requestsSrc.includes(`"${topic}"`), `access-requests.jsx should subscribe to ${topic}`);
    assert.ok(storeSrc.includes(`"${topic}"`), `the shared store should subscribe to ${topic}`);
  }
  assert.ok(agentsSrc.includes("<AccessApprovalBar"));
  assert.ok(contextSrc.includes("Shared Access"));
  assert.ok(contextSrc.includes('"access.grants"'));
  assert.ok(contextSrc.includes('"access.revoke"'));
  assert.ok(reposSrc.includes("Linked repos"));
  assert.ok(reposSrc.includes('"access.grants"'));
  assert.ok(reposSrc.includes('"access.revoke"'));
  assert.ok(reposSrc.includes('"sandbox.grants_get"'), "legacy sandbox grants must remain intact");
});
