---
job: open_pr
checks: [tests_pass, lint_pass, no_secrets, no_coauthor, linked_task, template_complete]
---
# Open a PR
1. Rebase on the default branch and resolve conflicts.
2. Run the repository test and lint commands. Stop on failure.
3. Use an imperative title under 70 characters.
4. Write What, Why, How tested, and Risk headings and link the task ID.
5. Remove secrets, co-author trailers, and agent signatures.
