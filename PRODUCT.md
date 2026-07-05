# Product

## Register

product

## Users

One person: the author — technical (wrote the original `reel` shell CLI), on
Arch + Hyprland, comfortable in a terminal. Not a team, not a customer.

Context of use: home at a desk, often just back from a trip, tired, with one or
more camera cards (DJI / GoPro / iPhone) full of footage to deal with. The job
is to get that footage safely off the cards, sorted into the right trip, looked
through, the good bits marked and cut, and pushed to the shared pool — then
reclaim the card. Sessions are occasional (per trip), not daily, so the tool
must re-explain its own state every time rather than assume the user remembers
where they left off.

## Product Purpose

reel ingests trip footage into per-trip workspaces, helps review and mark it,
cuts the keepers, and pushes raw masters to a shared pool. It succeeds
the `reel` shell script, which grew to ~15 verbs and confused even its author —
and which let the same clips land in multiple trips, or one card's footage get
split across trips, with no clear picture of what was already imported or pushed
to the pool.

Success looks like: sit down with a full card, and without second-guessing,
know exactly what's on it, which sessions belong to which trip, what's already
imported and in the pool — then process and wipe with zero fear of losing or
duplicating a single clip. The tool should make a chore that's easy to get
wrong feel calm and obvious.

## Brand Personality

Warm, clear, trustworthy. The voice is plain and friendly — it talks like a
person who's done this before, not like enterprise software and not like a
chirpy consumer app. These are someone's travel memories, so the tool should
feel like a good place to spend time with them, not a file-management duty.

Crucially: warm is **not** cute. Warmth lives in palette, typography, and copy —
never in oversimplification. It treats the user as capable: it shows the real
decisions (what's a duplicate, what's in the pool, what's about to be deleted)
plainly rather than hiding them behind a friendly veneer.

Emotional goal: the quiet relief of knowing your footage is safe and in order.

## Anti-references

- **Heavyweight NLE (Premiere / DaVinci):** no panel clutter, no dockable-
  everything, no timeline-first cockpit, no wall of controls. reel is a focused
  pipeline, not an editor — the actual editing hands off to Kdenlive.
- **Consumer photo app (iCloud / Google Photos):** no rounded-bubble cuteness,
  no emoji, no oversimplification that hides what's happening. It must never
  make an irreversible decision (dedup, wipe) *for* the user behind a cheerful
  facade.
- (Adjacent, also avoid:) the generic SaaS dashboard — KPI tiles, charts, and
  identical card grids. reel shows footage and state, not metrics.

## Design Principles

1. **Clarity is the contract.** At every moment the user can see what's where:
   which trip, which session, on-card vs imported vs in-the-pool. State is shown,
   never assumed or hidden. This is the priority that wins every trade-off —
   it's the exact thing the old CLI failed at.
2. **Warm, not cute.** Personality comes through color, type, and voice — never
   through hiding the real decisions or talking down to the user. Respect their
   competence while making the screen feel good to be in.
3. **Earn the right to destroy.** Anything irreversible (wipe, overwrite) is
   gated on a verified pool copy + dedup, and says plainly what it's about to do.
   Trust is built before anything is deleted.
4. **Focused, not a cockpit.** One clear task per screen; resist NLE panel-
   sprawl. Depth and detail are reachable but are never the default surface.
5. **The footage is the point.** This is about memories, not files — keep the
   actual clips present and visible, and make moving through them feel good.

## Accessibility & Inclusion

Personal single-user tool, so no external WCAG mandate — but the baseline still
holds: body text ≥ 4.5:1 contrast, large text ≥ 3:1; never encode state by color
alone (always pair with text or icon, as the state badges already do); honor
`prefers-reduced-motion` with crossfade/instant fallbacks. No known assistive-
technology requirements. Long sessions in a dim room are likely, so favor a
comfortable dark surface and avoid harsh pure-white fields.
