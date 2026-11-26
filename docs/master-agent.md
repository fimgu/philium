# Master agent (definition, not built)

Status: proposal for approval. Nothing here exists yet.

## What it is

One agent that works across all your workspaces. Every other Philium agent belongs to one workspace and only sees that workspace. The master sees everything Philium manages and acts for you across it.

Think of it as the person at the top of the org chart: it does not write code itself, it finds out, decides, and hands work to the agents that belong to each workspace.

## What it is for

- **"What's going on?"** One question, one answer across every workspace: which agents are working, which wait for you, which reviews need you, what is stuck or failing.
- **Cross-workspace tasks.** "The API changed in backend, update the frontend." It reads the backend, then starts or messages an agent in frontend with a precise brief.
- **Housekeeping.** "Clean up everything finished." "Mark the reviews for merged PRs done." It uses the same Done and Remove all actions you have, and shows you what it removed.
- **Routing work.** "Review PR 5651 and fix what it finds." It starts the reviewer, waits, then hands the findings to a worker in that workspace.

## What it is not

- Not a coder. It has no worktree and does not edit files. Code changes are always done by an agent in the workspace that owns the code.
- Not a memory merger. Each workspace keeps its own memory. The master can read a workspace's memory to answer you, but it never copies one workspace's memory into another, and it never builds a global memory.
- Not autonomous in the background. It works only when you talk to it. No schedules, no auto-runs.

## Rules

1. **One at a time.** Only you can start it. There is never a second one, and no agent can spawn it.
2. **Off by default.** Turned on in Settings. Off means it does not exist.
3. **Read across, write through the owner.** It can read every workspace's board, agents, reviews and memory. To change anything in a workspace it starts or messages an agent there. That agent's normal rules apply (its model, strict mode, privacy, approvals).
4. **Privacy follows the workspace.** It runs on a model you pick for it. Reading a workspace sends that workspace's content to that model, so it can only read workspaces that allow that provider. The default is: same provider as each workspace's own agents, or it skips the workspace and says so.
5. **Strict workspaces are off limits.** It sees only that they exist and whether something needs you.
6. **No GitHub writes of its own.** It cannot post reviews, approve, merge or push. It can ask the reviewer to run, which keeps the reviewer's rules (observe by default, never on your own PR except as a comment, never Approve without your permission).
7. **Destructive actions ask first.** Remove agents, mark done, stop a run: it shows the list and waits for your yes in the chat, unless you said "go ahead" for that kind of action in the same conversation.
8. **Everything it does is logged** in Activity, tagged as the master, so you can see what it read and what it started.

## Tools it gets

Read:
- every workspace's agents, their state and last message
- every board and review (checklist, verdict, needs-you items)
- a workspace's memory search (rule 4 applies)
- usage per provider

Act:
- start an agent in a workspace, with a brief
- message an agent in any workspace
- start a PR review, mark a review done
- stop or remove agents (rule 7)

It does not get: file edits, shell, browser, GitHub posting, access to secrets.

## How it looks

- **Sidebar:** a single "Master" entry at the top, above Home, only when it is on. Opening it shows one chat, the same chat view agents use.
- **Above the chat:** a slim strip with what it can see right now: "Sees 4 workspaces, skips 1 (provider not allowed), 1 strict". Clicking it opens the settings.
- **In the chat:** when it starts or messages another agent, a card shows which workspace and which agent, with a link to jump there. When it wants to remove or stop something, a card lists exactly what, with Yes and No.
- **Elsewhere:** agents it started show "started by Master" on their row, so you can tell them apart.

## Settings

Settings, Master agent:
- **On / Off.** Default off.
- **Model.** Picked from the live model list, like any agent. No hardcoded default.
- **Workspaces it may read.** A list of your workspaces with a checkbox each. All ticked by default except where the provider is not allowed or the workspace is strict, which are shown greyed out with the reason.
- **Ask before:** removing agents, marking reviews done, starting agents. All on by default.

## Open questions for you

1. Should it be able to start agents in a workspace without asking each time, or always ask?
2. Should it keep its own memory (things like "you prefer short briefs"), or start fresh each conversation? A small memory of its own, separate from every workspace, is my suggestion.
3. Do you want it to report on its own when something needs you (for example a review is blocked), or only when you ask? Rule "not autonomous" says only when you ask.

## Build size, if approved

Backend: a master role in agents (single instance, no worktree, no repo), a cross-workspace tool set in MCP built on the existing access and mail code, Settings keys, Activity tagging. Frontend: sidebar entry, chat view reuse, the strip, action cards, Settings section. Tests for each rule above, especially 1, 3, 4, 5 and 6.
