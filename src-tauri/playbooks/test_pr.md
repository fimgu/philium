---
job: test_pr
checks: [tests_pass, workflow_evidence]
---
# Test a PR
1. Map changed areas and affected callers with the code graph.
2. Run the repository tests against the current PR head.
3. Exercise the real user workflow for each changed area.
4. Attach commands, observed outcomes, and evidence for every area. Report blockers honestly.
