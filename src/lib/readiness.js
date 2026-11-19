// The release decision as a table: one row per question, each row green, red or unknown.
// The verdict itself is computed by the backend. These rows only lay out what it was computed
// from, and they lean towards "unknown": a row is green only when its evidence is all there.

const short = (oid) => (oid ? String(oid).slice(0, 10) : "none");
const count = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const REGRESSION = ["composed_head_regression", "release_range_regression", "failure_unattributed"];

export const HEADLINE = {
  not_ready: "Do not ship",
  superseded: "Head or target changed. Verify again.",
  ready_for_reviewed_scope: "Ready for the reviewed scope",
  incomplete: "Release verification incomplete",
};
export const tone = (decision) =>
  ({ ready_for_reviewed_scope: "ok", not_ready: "bad", superseded: "old", incomplete: "unknown" })[decision] || "unknown";

/**
 * d: a release decision with its receipt. m: origin membership for the same generation, or null.
 * cov: which landed pull requests a pinned check reads, or null. Rows marked `informs` do not
 * change the verdict; they say what the verdict does not cover.
 */
export function readiness(d, m, cov) {
  const r = d?.receipt || {};
  const rows = [];
  const add = (id, label, state, detail, informs = false) => rows.push({ id, label, state, detail, informs });

  // 1. Is this still the revision the origin has?
  const moved = d.decision === "superseded" || d.origin_current === false;
  add("candidate", "This exact revision", moved ? "bad" : d.origin_current === true ? "ok" : "unknown",
    `#${d.pr} at ${short(d.head)} onto ${short(d.base)}. ${moved ? "The head or target moved; this result is for an earlier revision." : d.origin_current === true ? "Rechecked with the origin after the checks: unchanged." : "Could not be reconfirmed with the origin."}`);

  // 2. Do we know everything that is in it?
  if (!m) add("contents", "What is in it", "unknown", "Which pull requests landed in this release is not established.");
  else {
    const gaps = (m.missing || 0) + (m.ambiguous || 0);
    add("contents", "What is in it", gaps ? "unknown" : "ok",
      `${count(m.landed?.length || 0, "pull request")} landed, ${m.edges?.length || 0} nested.${m.missing ? ` GitHub did not say which pull request ${count(m.missing, "commit")} came from.` : ""}${m.ambiguous ? ` ${count(m.ambiguous, "commit")} could belong to more than one.` : ""}${m.unmapped ? ` ${count(m.unmapped, "commit")} not tied to a pull request.` : ""}`);
  }

  // 3. Does it merge onto the target as the target is now?
  const merge = d.mergeable?.state;
  add("merge", "Merges onto the target", merge === "clean" ? "ok" : merge === "conflicts" ? "bad" : "unknown",
    merge === "clean" ? "No conflicts with the target as it is now."
      : merge === "conflicts" ? `Conflicts in ${count(d.mergeable.count ?? d.mergeable.files?.length ?? 0, "file")}: ${(d.mergeable.files || []).slice(0, 6).join(", ")}${(d.mergeable.count || 0) > 6 ? ", ..." : ""}. The head has to change first.`
        : "Not established locally.");

  // 4. The pinned checks on this commit.
  const cases = r.cases || [];
  const pass = cases.filter((c) => c.state === "candidate_pass").length;
  const broke = cases.filter((c) => REGRESSION.includes(c.state));
  // A landed pull request that fails the same way on its own is the first place to look.
  const suspects = [...new Set(broke.flatMap((c) => (c.landed_evidence || []).filter((e) => e.result === "same_failure_as_candidate").map((e) => e.pr)))];
  const old = cases.filter((c) => c.state === "same_failure_on_target").length;
  const blind = cases.filter((c) => c.state === "incomplete").length;
  add("checks", "Checks on this commit", !cases.length ? "unknown" : broke.length ? "bad" : pass === cases.length ? "ok" : "unknown",
    !cases.length ? "No checks were run: none are set up for this checkout."
      : `${pass} of ${cases.length} pass.${broke.length ? ` Failing here: ${broke.map((c) => c.id).join(", ")}.` : ""}${suspects.length ? ` Fails the same way on its own: ${suspects.map((n) => `#${n}`).join(", ")}.` : ""}${old ? ` ${old} already failing on the target, not introduced by this release.` : ""}${blind ? ` ${blind} could not be observed.` : ""}`);

  // How much of the release the pinned checks look at. Informing only.
  if (cov?.prs?.length) {
    const none = cov.untouched || [];
    add("coverage", "Every pull request has a check", none.length ? "unknown" : "ok",
      `${cov.prs.length - none.length} of ${count(cov.prs.length, "landed pull request")} change a file that a pinned check reads.${none.length ? ` No check reads anything changed by ${none.slice(0, 12).map((n) => `#${n}`).join(", ")}${none.length > 12 ? ", ..." : ""}.` : ""}`, true);
  }

  // 5. Is the running product this build? Before and after the checks, on every place observed.
  const seen = [];
  if (r.observer_configured || r.deployment_identity) seen.push(["local observer", r.deployment_identity?.state, r.deployment_after?.state]);
  for (const t of r.targets || []) seen.push([t.name, t.identity_before?.state, t.identity_after?.state]);
  const states = seen.flatMap(([, before, after]) => [before, after]);
  const wrong = seen.filter(([, b, a]) => [b, a].some((s) => ["mismatch", "mixed", "not_ready"].includes(s)));
  const running = !seen.length ? "unknown" : wrong.length ? "bad" : states.every((s) => s === "match") ? "ok" : "unknown";
  add("running", "The deployed app is this build", running,
    !seen.length ? "Philium cannot see which build is deployed, so checks on the deployed app cannot count."
      : wrong.length ? `A different or mixed build is running on: ${wrong.map(([n]) => n).join(", ")}.`
        : running === "ok" ? `Same revision and artifact before and after the checks on: ${seen.map(([n]) => n).join(", ")}.`
          : `Not established on: ${seen.filter(([, b, a]) => b !== "match" || a !== "match").map(([n]) => n).join(", ")}.`);

  // 6. Checks against the running product. They count only while the row above is green.
  const live = r.live_cases || [];
  const liveBad = live.filter((c) => c.state === "mismatch");
  const liveOk = live.filter((c) => c.state === "pass").length;
  const smoke = !live.length ? "unknown" : liveBad.length ? "bad" : liveOk === live.length ? (running === "ok" ? "ok" : "unknown") : "unknown";
  add("smoke", "Checks on the deployed app", smoke,
    !live.length ? "None are set up."
      : `${liveOk} of ${live.length} match.${liveBad.length ? ` Differs: ${liveBad.map((c) => c.id).join(", ")}.` : ""}${liveOk === live.length && running !== "ok" ? " Not counted: the running build is not confirmed." : ""}`);

  // 7. Independent build record and review for this exact head.
  const evidence = Object.values(r.evidence_status || {});
  const tampered = evidence.filter((s) => s === "altered" || s === "invalid").length;
  const reviewed = r.review_attestation && typeof r.review_attestation === "object" && !r.review_attestation.error;
  const record = tampered ? "bad" : d.artifact && r.release_records === true && reviewed && evidence.every((s) => s === "verified") ? "ok" : "unknown";
  add("record", "Build record and review", record,
    tampered ? `${count(tampered, "piece")} of evidence was altered or is invalid.`
      : record === "ok" ? `Artifact ${short(d.artifact)} and a review are recorded for this head; ${count(evidence.length, "piece")} of evidence verified.`
        : !d.artifact ? "No build record for this exact head." : !reviewed ? "No recorded review for this exact head." : "Some evidence is missing or belongs to another head.");

  // 8. Did the owner read and confirm the checks that were run?
  add("confirmed", "Checks confirmed by you", r.checks_confirmed_ms ? "ok" : "unknown",
    r.checks_confirmed_ms ? `Confirmed ${new Date(r.checks_confirmed_ms).toISOString().slice(0, 16).replace("T", " ")} UTC.` : "You have not confirmed the checks that ran.");
  return rows;
}

/** A summary to paste where the team reads it. Plain Markdown, no approval implied. */
/** The agents' review in words for a pasted summary. */
export const AGENT_SUMMARY = { none: "plan not written yet", do_not_ship: "do not ship", not_ready: "not ready yet", passed: "passed, waiting for sign-off" };
export const ORIGIN_WORDS = { introduced: "introduced", pre_existing: "pre-existing", unverified: "origin unverified" };

function agentSection(review, cell) {
  const items = review?.items || [];
  if (!items.length) return [];
  const result = (it) => ({ pass: "PASS", fail: "FAIL", blocked: "BLOCKED" })[it.result] || (it.status === "claimed" ? "IN PROGRESS" : "TO DO");
  const weight = (it) => it.result === "fail" ? (it.severity ?? 1) : it.result === "blocked" ? 10 : !it.result ? 12 : 20;
  const short = (s) => { const t = cell(s || ""); return t.length > 180 ? `${t.slice(0, 177).replace(/\s+\S*$/, "")}…` : t; };
  return [
    `### Agents' review: ${AGENT_SUMMARY[review.headline] || review.headline} (${items.filter((i) => i.result).length} of ${items.length} items have a result)`,
    "",
    "| Result | Severity | Check | Origin | What was run |",
    "|---|---|---|---|---|",
    ...[...items].sort((a, b) => weight(a) - weight(b)).map((it) => `| ${result(it)} | ${it.result === "fail" ? `P${it.severity ?? "?"}` : ""} | ${cell(String(it.title).replace(/^P\d\s+/, ""))} | ${it.result === "fail" ? ORIGIN_WORDS[it.origin] || "origin unverified" : ""} | ${short(it.evidence)} |`),
    "",
    "P0 and P1 block the release; P2 to P5 should be looked at, in decreasing importance. Introduced: fails here, passes on the target. Pre-existing: fails the same way on the target.",
    "",
  ];
}

export function summaryMarkdown(d, rows, review) {
  const mark = { ok: "PASS", bad: "FAIL", unknown: "UNKNOWN" };
  const cell = (s) => String(s).replaceAll("|", "\\|").replace(/\s+/g, " ").trim();
  return [
    `## Release check for #${d.pr}: ${HEADLINE[d.decision] || d.decision}`,
    "",
    `Head \`${d.head}\` onto \`${d.base}\`. Artifact: ${d.artifact ? `\`${d.artifact}\`` : "none recorded"}. Checked ${new Date(d.checked_ms).toISOString().slice(0, 16).replace("T", " ")} UTC.`,
    "",
    "| Result | Question | Detail |",
    "|---|---|---|",
    ...rows.map((r) => `| ${mark[r.state]}${r.informs ? " (informs)" : ""} | ${cell(r.label)} | ${cell(r.detail)} |`),
    "",
    ...(d.why?.length ? ["Why it is not ready:", ...d.why.map((w) => `- ${cell(w)}`), ""] : []),
    ...(review?.pr === d.pr ? agentSection(review, cell) : []),
    "Unknown is not a pass. Computed locally by Philium for this exact revision. This is not a GitHub approval.",
  ].join("\n");
}
