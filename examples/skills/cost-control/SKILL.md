---
name: cost-control
description: Keep a task within its budget and spend less on the same result by checking budget_status, delegating bulk work to cheaper models, keeping prompts cache-friendly and stopping at the right time.
---

# Cost control

Most of an agent's cost is input tokens resent on every round. Spend them on
work that needs the expensive model.

## Know where you are

- Call `budget_status` at the start of a long task, before an expensive step
  and after any runtime budget notice. It shows the turn's tokens and cached
  share, the task's spend against its budget, and the models you may delegate
  to, cheapest first, with prices and notes.
- Estimate before you act: reading a 2,000-line file costs roughly 25,000
  input tokens on every later round of the turn.

## Spend less on the same result

1. **Search before you read.** `search_files` (with `regex` and `glob`) finds
   the lines that matter; read files with `offset` and `limit` instead of
   whole.
2. **Delegate bulk work.** Reading many files, summarizing logs, scanning
   documentation or checking a long list fits a cheaper model:
   `spawn_agent` with `model` set to one listed by `budget_status`. Ask for a
   short, structured result so the summary is cheap to read back.
3. **Keep the prefix stable.** Providers cache the beginning of a request.
   Do not rewrite early files or repeat large outputs; refer to what you
   already read.
4. **Do not repeat long outputs.** Quote the line that changed, not the file.
5. **Stop at done.** When the acceptance criteria pass, report; do not keep
   polishing unless asked.

## Budget notices

At 50% of the budget, check that the remaining plan fits. At 80%, finish the
most valuable remaining step and drop optional ones. At 95%, write your
report: what is done, what is not, and what the rest would cost.

## Report

End a costly task with the cost from `budget_status`, the share that came from
the cache, and one change that would make a similar task cheaper next time.
The operator can see the breakdown with `layeragent cost TASK_ID`.
