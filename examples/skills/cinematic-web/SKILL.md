---
name: cinematic-web
description: Build a cinematic, story-driven web page (product launch, brand landing, storefront hero) with strong typography, scroll-driven motion and media, while staying fast, accessible and honest about assets.
---

# Cinematic web pages

"Cinematic" means the page tells a story in scenes: each viewport-sized
section has one idea, one strong image or motion, and one line of copy that
moves the visitor to the next. It does not mean heavy.

## Structure the story first

Write the scenes before the code, in `docs/plan/` when planning:

1. **Opening**: the product in its best light and a promise in a few words.
2. **Feeling**: what using it is like (speed, freedom, craft), in images.
3. **Proof**: details, materials, numbers, a comparison or testimonials.
4. **Choice**: models, sizes or colors, with prices if there are any.
5. **Action**: one clear call to action, repeated in the footer.

For a bicycle store: opening on a rider at speed; feeling with motion through
a landscape; proof with the frame, groupset and weight; choice of models;
action to buy or book a test ride.

## Visual language

- Two typefaces at most: a display face for headlines, a readable text face.
  Large type with tight leading for headlines; body at 16 to 18 px.
- A restrained palette: one dark or light base, one accent for actions.
- Full-bleed media, generous spacing, aligned edges. Contrast first.

## Motion

- Prefer CSS: `transition`, `@keyframes`, and scroll-driven animations
  (`animation-timeline: view()`) where supported, with a static fallback.
- Use `IntersectionObserver` to start effects when a scene enters the view.
- Animate `transform` and `opacity` only; avoid animating layout.
- Respect `prefers-reduced-motion: reduce`: no parallax, no autoplaying
  motion, instant transitions.
- A video or canvas hero needs a poster image and must never block reading.
  For 3D, load the `threejs-3d` skill and keep the page usable without it.

## Assets, honestly

- Use only images, video and fonts the operator owns, generated procedurally
  (SVG, CSS gradients, canvas), or under a license that allows the use, and
  record the source and license in `docs/plan/` or a credits file.
- When real photos are missing, use clearly labeled placeholders and say what
  should replace them. Never imply they are the operator's products.

## Performance budget

- Largest image under about 300 KB, served as AVIF or WebP with a fallback,
  with `width`, `height` and `loading="lazy"` below the fold.
- Fonts: at most two families, `font-display: swap`, preloaded if critical.
- Little or no JavaScript for content; the page must read without it.

## Accessibility

- Text contrast of at least 4.5:1, visible focus, every control reachable by
  keyboard, meaningful `alt` text, headings in order, `lang` set.

## Check it

- Validate that every asset path resolves and the page has no console errors
  (`browser_console` when the agent's browser can reach the page).
- Check narrow and wide layouts and reduced motion. Take `browser_screenshot`
  of each scene as evidence when possible.
- If the browser cannot reach the page (local addresses are blocked unless the
  operator allows them), check the files instead and say that the visual check
  is still pending; do not claim it looks right.
