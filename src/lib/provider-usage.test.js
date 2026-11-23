import test from "node:test";
import assert from "node:assert/strict";
import { providerUsageRows } from "./provider-usage.js";

test("joins by ID and includes idle, disabled, and removed providers", () => {
  const workProvider = { id: "work", name: "ChatGPT" };
  const idle = { id: "personal", name: "ChatGPT", enabled: false };
  const workUsage = { provider_id: "work", requests: 3 };
  const historical = { provider_id: "removed", requests: 2 };
  assert.deepEqual(providerUsageRows([workProvider, idle], [historical, workUsage]), [
    { id: "work", provider: workProvider, stats: workUsage },
    { id: "personal", provider: idle, stats: undefined },
    { id: "removed", stats: historical },
  ]);
});

test("lists configured providers even when no usage is recorded", () => {
  const provider = { id: "claude" };
  assert.deepEqual(providerUsageRows([provider], []), [
    { id: "claude", provider, stats: undefined },
  ]);
});
