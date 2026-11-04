---
name: threejs-3d
description: Model, light, animate and export 3D objects for the web with three.js (for example a bicycle built from primitives), optionally with Blender in background mode when it is installed, keeping pages fast and accessible.
---

# 3D for the web with three.js

## Decide what the 3D is for

A product viewer (rotate and inspect), a hero scene (motion behind a headline)
or an asset to export (a `.glb` file). Write which in the plan. The page must
still make sense without WebGL: provide a poster image and text.

## Setup

- With a build step (operator level `some` or `expert`): Vite and the `three`
  package pinned to an exact version.
- Without one (level `none`): ship a pinned copy of three.js next to the page
  and load it as an ES module with an import map; do not depend on a CDN the
  operator has not chosen.
- One `WebGLRenderer` with `antialias: true`, pixel ratio capped at 2,
  `outputColorSpace = SRGBColorSpace`, tone mapping `ACESFilmicToneMapping`.
- Resize with the container, not the window, and stop rendering when the
  canvas is off screen.

## Modeling from primitives (a bicycle)

- Wheels: `TorusGeometry` for tires, thin `CylinderGeometry` spokes, a small
  cylinder hub. Keep real proportions (wheel about 0.7 m across).
- Frame: tubes along `CatmullRomCurve3` paths with `TubeGeometry`, joined at
  the head tube, bottom bracket and dropouts.
- Group parts in `Group`s (front wheel, rear wheel, frame, handlebar) so they
  can rotate and be named in the exported file.
- Materials: `MeshStandardMaterial` or `MeshPhysicalMaterial` with sensible
  roughness and metalness; one environment map gives reflections more cheaply
  than many lights. Add a key light and a soft fill; cast shadows only where
  they read.

## Interaction and motion

- `OrbitControls` with damping, limited zoom and polar angle so the object
  never flips; a reset view button.
- Animate with `requestAnimationFrame` and a clock; spin wheels by distance.
- Honor `prefers-reduced-motion`: no autoplay rotation.

## Loading and exporting

- Load provided models with `GLTFLoader` (and `DRACOLoader` if compressed).
- Export with `GLTFExporter` to `.glb` when the operator wants the asset.
- Dispose of geometries, materials and textures you replace.

## Optional: Blender in background mode

Only if `blender` is installed and runnable in the sandbox (the operator may
need to allow its directory in `policy.read_roots`):

```bash
blender --background --python build_bike.py -- --out bike.glb --preview bike.png
```

The script builds the geometry with `bpy`, assigns materials, exports glTF
binary with `bpy.ops.export_scene.gltf(filepath=..., export_format='GLB')` and
can render a preview with Eevee. Keep the script in the project so the model
can be rebuilt.

## Performance

- Under about 100,000 triangles for a product viewer; reuse geometries and
  materials; textures at 2048 px at most, compressed.
- Measure frame time on a modest laptop; drop shadows or pixel ratio first.

## Check it

- No console errors and a visible first frame within a few seconds.
- The fallback image and text appear when WebGL is unavailable.
- Take `browser_screenshot` of the viewer when the agent's browser can reach
  the page; otherwise say the visual check is pending.
