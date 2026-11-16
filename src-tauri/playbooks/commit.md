---
job: commit
checks: [no_coauthor, no_secrets]
---
# Commit
1. Inspect the diff and remove secrets.
2. Run focused tests and stage only the intended files.
3. Write a small conventional commit with a clear imperative subject.
4. Keep the user as author. Never add co-author trailers or agent signatures.
