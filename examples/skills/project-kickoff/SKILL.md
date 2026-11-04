---
name: project-kickoff
description: Turn a request to build something new (a site, an app, a 3D piece, a tool) into a written brief, stack choice, plan, acceptance criteria and cost estimate before any code is written.
---

# Project kickoff

Use this before building anything new. The goal is that the operator and you
agree on what will be built, how it will be checked and roughly what it will
cost, while changing course is still cheap. In planning mode the files below go
under `docs/plan/`.

## 1. Understand the request

Read what the operator wrote twice. Write down, in their words:

- what they want to exist when you are done;
- who will use it and what those people are trying to do;
- what "good" looks like to the operator (examples, references, adjectives
  such as "cinematic", "minimal", "fast");
- constraints: deadline, budget, hosting, brand, language, devices.

Check the operator profile in your instructions: technical level, language,
where they publish. If the technical level is unknown and it changes what you
would build, ask once.

## 2. Ask only what blocks you

Decide what you reasonably can and record the assumption. Ask only when a
wrong guess would waste most of the work, for example: the product being sold,
whether real payments are needed, or whether existing content must be reused.
Put open questions at the end of `brief.md`, each with your default answer.

## 3. Write the plan files

- `brief.md`: goal, audience, success, scope and out of scope, assumptions,
  open questions with defaults.
- `stack.md`: the proposed stack and why, one simpler and one more capable
  alternative with their trade-offs, and what the operator must install, pay
  for or maintain (load the `stack-choice` skill).
- `plan.md`: ordered steps; each has a deliverable and how it will be checked.
  Keep steps small enough that a failure costs little.
- `acceptance.md`: numbered acceptance criteria, each with the check that
  proves it (a command, a page to open, a screenshot to compare). Anything
  people will see needs a visual check, not only a passing build.
- `costs.md`: estimated input and output tokens and cost per step at the
  configured prices (`budget_status` lists them), the model you would use for
  each step, and where a cheaper model is enough (load `cost-control`).

## 4. Ask for approval

End with a short summary in the operator's language: what you will build, the
stack in one line, the number of steps, the estimated cost and the questions
that still matter. Do not build until they approve or change the plan.

## 5. While building

Follow `plan.md` in order and tick acceptance criteria as their checks pass.
When reality forces a change, update the plan file and say so in your report.
At the end, report what was built, how each criterion was checked, what was
not checked and why, and what it cost (`budget_status`).
