---
job: teardown
checks: [ledger_empty]
---
# Tear down an agent
1. Stop accepting work and release task, browser, path, and port leases.
2. Terminate owned processes and remove owned containers, compose projects, and kind clusters.
3. Clean temporary directories and release ports. Never remove another agent's resources.
4. Keep worktrees and branches with unmerged work and surface them to the user.
5. Revoke credentials and verify the agent resource ledger is empty.
