# Website

> **Status: implemented.** The public site, published to GitHub Pages by
> [`.github/workflows/pages.yml`](../../.github/workflows/pages.yml) on every push to `main`
> that touches this directory.

Two pages: the overview, and the download page (which also covers building from source).

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
changes, and every effect here is DOM-level. Each pinned scene (the hero's tilting app window,
the four-act pipeline, the graph that draws itself) reduces to one number, how far through it the reader is, and draws itself
from that. One scroll listener, coalesced to a frame, feeds all of them. The bundle is ~10 kB
gzipped.

## Not only scroll

`src/effects.ts` holds the animations that run on their own or answer the pointer: the hero
window plays a live recording on a timer (the clock ticks, lines type in, extracted items
appear), specks drift up behind the hero on a canvas, pulses flow along the graph's edges, the
manifesto's highlights shimmer, section labels decode the first time they appear, feature cards
glow under the pointer with a different icon move each, and pressing the download button sends
out a ripple and a burst. Each one pauses when it is off screen.

## Motion is optional

An inline script adds `.motion` to `<html>` before first paint unless the visitor asked for
reduced motion, and every pinned or scrubbed effect is gated on that class. Without it (reduced
motion, or no JavaScript) nothing is pinned and every section is simply laid out in its finished
state, including no `scroll-behavior: smooth`, which is the part that actually makes people ill.

## The download button

macOS gets a real download in three formats, `.dmg`, `.zip` and `.app` (as `.app.tar.gz`), all
the same signed app, built and verified by `release.yml`. Windows and Ubuntu say "coming soon".

The files carry the version in their names, so no fixed URL points at the current build. The page
asks the GitHub API for the newest release with a Mac build (final first, then release candidate)
and reads the real file names and sizes (`src/release.ts`). That gives three honest states:

| GitHub says | The button |
|---|---|
| A release with Mac files | Downloads the chosen format directly, with its size |
| No release yet | Becomes "Build it from source" and says the first build is not out |
| Could not ask (offline, rate-limited) | Opens the Releases page, which lists the same files |

The answer is cached for ten minutes per tab, since the unauthenticated API allows sixty requests
an hour per address.

## Keeping it true

Claims here are checkable against the repository, and several are checked *by* it: the test
count in the hero, the platform statuses, and the "what does not work yet" section all mirror
what the app's own About screen reports. If one of those changes, this page changes with it:
a landing page describing a different version of the product is worse than none.
