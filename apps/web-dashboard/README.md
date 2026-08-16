# Web dashboard

> **Status: implemented, local-only.** A read-only overview of one workspace, served from a
> running local engine. The hosted team/admin/billing dashboard this directory was originally
> reserved for is still Phase 3 — see [below](#what-this-is-not).

Answers the questions the desktop app cannot, because the desktop app is built around one
meeting at a time:

- How much time went into meetings, and whether the rate is rising
- Who is carrying open work, and who is late
- How many decisions the workspace has actually recorded

## Running it

```sh
notewise serve                 # the engine, on 127.0.0.1:47821
npm install && npm run dev     # this app, on :1430
```

The dev server proxies `/v1` and `/health` to the engine, so everything is same-origin and
there is no CORS to configure on a server that deliberately has none. A production build is
static files; the engine can serve them with `notewise serve --ui apps/web-dashboard/dist`.

## Read-only, structurally

`src/lib/api.ts` contains no `POST`, `PUT`, `PATCH` or `DELETE` — not disabled, not behind a
flag: absent. The way a read-only surface stays read-only is by not containing the code that
would write.

## What this is not

This directory's original brief was the **hosted** dashboard: team workspace, admin,
onboarding and billing, talking to [`cloud/`](../../cloud/) and never to a user's local
engine. That app is still Phase 3 and cannot be built yet — every service under `cloud/` is a
scaffold, so there is nothing for it to talk to. Building its shell against a mocked API would
produce exactly the half-working product the roadmap's phase gating exists to prevent.

So this is a different, smaller app that could be built honestly today, in the directory
reserved for the larger one. Two consequences worth stating plainly:

- **It is MIT, not BSL.** It depends on nothing under `cloud/`. If the hosted dashboard is
  built here later, that boundary has to be revisited — see the licence table in
  [`CLAUDE.md`](../../CLAUDE.md).
- **It is Vite, not Next.js.** The original note specified Next.js for a server-rendered
  hosted app. This one has no server: it is static files against a loopback API, which is what
  the engine already knows how to serve, and it shares the desktop app's toolchain and theme
  rather than introducing a second one.

## Scope

Read-only, single workspace, no auth. The engine is loopback-only by design — see
`Server::bind`, which refuses a non-loopback address — so this cannot be opened from another
device, and nothing here changes that.
