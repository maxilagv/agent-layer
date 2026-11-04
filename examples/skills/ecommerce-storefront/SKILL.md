---
name: ecommerce-storefront
description: Build a small online store front (catalog, product pages, cart, checkout handoff) that fits the operator's level, never handles card data itself, and is findable and accessible.
---

# Storefront

## Choose the checkout first

The checkout decides the stack. Never collect or store card numbers yourself.

| Operator | Checkout |
| --- | --- |
| `none` | Payment links or buy buttons from a payment provider the operator already uses; the site stays static |
| `some` | The provider's hosted checkout started from a small serverless function, or links as above |
| `expert` | The provider's API on the operator's server, with webhooks, tested in the provider's test mode |

The operator creates the provider account and keys. Keys never go into the
browser, the repository or your messages; reference them by environment
variable on the server.

## Catalog

- Products as data (`products.json` or the provider's catalog): id, name,
  price with currency, images with `alt`, short and long description,
  variants (size, color) and stock if known.
- Prices formatted for the operator's locale; taxes and shipping stated where
  they apply.

## Pages

- Home or landing (see `cinematic-web` for a story-driven one).
- Catalog grid with filters only if there are many products.
- Product page: gallery, price, variants, a clear add or buy button, delivery
  and returns information.
- Cart (saved in `localStorage` for a static site) with quantities and a
  total, then the checkout handoff.
- Contact, shipping, returns, privacy and terms pages as placeholders the
  operator must complete; do not invent legal text as if it were final.

## Findability

- A unique `<title>` and meta description per page, Open Graph tags with an
  image, a `sitemap.xml` and a `robots.txt`.
- `Product` structured data (JSON-LD) with name, image, price, currency and
  availability on product pages.

## Accessibility and trust

- Keyboard-reachable controls, visible focus, labels on every input, error
  messages next to the field, contrast of at least 4.5:1.
- Show who sells, how to reach them and the return policy before payment.

## Check it

- Add to cart, change quantity, remove, and reach the checkout handoff on a
  narrow and a wide layout.
- With the provider's test mode, complete a test purchase only when the
  operator has set it up; otherwise list it as a pending check.
- No console errors; every product page validates its structured data.
