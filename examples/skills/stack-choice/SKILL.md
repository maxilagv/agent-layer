---
name: stack-choice
description: Choose the simplest stack that meets a project's requirements and fits the operator's technical level, hosting and maintenance capacity, and explain the trade-offs.
---

# Stack choice

The best stack is the simplest one that meets the requirements and that the
operator can run, publish and change after you leave. Start from the operator
profile in your instructions.

## By technical level

| Operator | Default deliverable | Avoid |
| --- | --- | --- |
| `none` | One `index.html` with inline or adjacent CSS and JavaScript, or a static folder they can open by double-click or drag to a static host | Build steps, servers, databases, frameworks they must install |
| `some` | A static site; Vite with plain JavaScript or React when the interface has real state; a `README` with copy-paste commands | Custom servers, container setups, many services |
| `expert` | Whatever the requirements call for, following the project's existing conventions | Novelty without a reason |

Unknown level: ask once. Until then prefer the `none` column; a simple
deliverable can grow, a complex one rarely shrinks.

## Questions that decide the stack

1. **Does it need a server?** Only for secrets, accounts, payments processed by
   you, shared data or heavy computation. Payment links, form services and
   embedded widgets often remove the need for one.
2. **Does content change often?** Rarely: static files. Often and by a
   non-technical person: a hosted CMS or a spreadsheet-backed page, chosen with
   them.
3. **How interactive is it?** A few effects: plain JavaScript. Many views with
   shared state: a component framework. 3D: three.js (see `threejs-3d`).
4. **Where is it published?** Static hosting accepts a folder of files. A VPS
   accepts anything but must be maintained. Match the profile's `publish`.
5. **Who maintains it?** Everything you add is something they must update.

## Write it down

In `stack.md` give the choice, why it fits these answers, a simpler
alternative and a more capable one with what each gains and costs, and the
exact steps the operator must do themselves (accounts, keys, publishing). Keep
dependencies few and pinned; prefer ones with a long track record.

## Checks

- The deliverable opens or runs with the documented steps on a clean machine.
- No secret is needed in the browser; anything secret stays on a server or
  with a provider the operator controls.
- The operator's own words in `brief.md` are all covered by the stack.
