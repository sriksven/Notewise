# Website

> **Status: implemented.** The public site, published to GitHub Pages by
> [`.github/workflows/pages.yml`](../../.github/workflows/pages.yml) on every push to `main`
> that touches this directory.

Two pages: the overview, and how to install.

```sh
npm install
npm run dev      # http://localhost:1440/Notewise/
npm run build    # typechecks, then emits dist/
```

The dev server serves under `/Notewise/` because that is where GitHub Pages serves this
repository. Setting a custom domain later means changing `base` in `vite.config.ts` back
to `/`.

## No framework

A static document with scroll effects. React would ship ~140 kB to render markup that never
changes, and every effect here — reveals, the sticky pipeline, parallax, counters — is
IntersectionObserver and one scroll listener. The whole bundle is about 7 kB.

## Motion is optional

Every animated path checks `prefers-reduced-motion`, including `scroll-behavior: smooth`,
which is the part that actually makes people ill. With it set, the content is simply there.

## Why there are no download buttons

There is no release channel: no signed builds, no installers, no auto-update. Three buttons
pointing at binaries that do not exist would be the most dishonest thing on the site, and the
first click would prove it.

So `/download/` states each platform's real status — macOS supported, Windows and Linux
engine-only — and gives commands that work today, with a copy button on each. Every button on
the site does something.

When a release pipeline exists, this page grows real artifacts and the honest status badges
stay.

## Keeping it true

Claims here are checkable against the repository, and several are checked *by* it: the test
count in the hero, the platform statuses, and the "what does not work yet" section all mirror
what the app's own About screen reports. If one of those changes, this page changes with it —
a landing page describing a different version of the product is worse than none.
