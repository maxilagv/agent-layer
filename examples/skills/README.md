# Starter skills

Guides the agent can load when a task calls for them. They have no fixtures,
so each one waits in quarantine until you have read and accepted it:

```bash
layeragent skills import examples/skills
layeragent skills quarantine          # what is waiting
layeragent skills review cinematic-web
layeragent skills accept cinematic-web
layeragent skills reject cinematic-web
```

| Skill | For |
| --- | --- |
| `project-kickoff` | Brief, stack, plan, acceptance criteria and cost before building |
| `stack-choice` | The simplest stack for the requirements and the operator's level |
| `cost-control` | Staying within budget and spending less on the same result |
| `cinematic-web` | Story-driven landing and product pages that stay fast and accessible |
| `threejs-3d` | 3D on the web with three.js, optionally Blender in background mode |
| `ecommerce-storefront` | Catalog, product pages, cart and a checkout handoff |

The same commands import other catalogs that use the `SKILL.md` format (a
directory, or an `https` Git repository). Imported scripts are listed by
`review`; nothing an imported skill contains runs until the agent chooses to
run it inside the sandbox. Accept only skills you have read.
