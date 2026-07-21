"use strict";

// Tauri v2 with withGlobalTauri exposes invoke here.
const invoke = (cmd, args) =>
  window.__TAURI__?.core?.invoke
    ? window.__TAURI__.core.invoke(cmd, args)
    : Promise.reject(new Error("not running inside the reel window"));

const $ = (sel) => document.querySelector(sel);

// ---- logging ----
// Everything here lands in the same file the engine writes (`log.rs`), in order,
// so a JS failure and the Rust fault behind it read as one story. Never awaited
// and never allowed to throw: a logger that can break the caller is worse than
// no logger. `console` is kept too — it's there in the inspector during a debug
// build, where the file is the awkward way to read it.
function logf(level, msg, ctx) {
  try {
    (console[level] || console.log).call(console, msg, ctx ?? "");
    invoke("log_event", { level, msg: String(msg), ctx: ctx ?? null }).catch(() => {});
  } catch {}
}
const logError = (msg, ctx) => logf("error", msg, ctx);

// Nothing else catches these, and unhandled rejections are how a failed `invoke`
// disappears — the single most common way a bug here left no trace at all.
window.addEventListener("error", (e) => {
  logError(`uncaught: ${e.message}`, {
    src: e.filename ? `${e.filename}:${e.lineno}:${e.colno}` : null,
    stack: e.error?.stack ?? null,
  });
});
window.addEventListener("unhandledrejection", (e) => {
  const r = e.reason;
  logError(`unhandled rejection: ${r?.message ?? r}`, { stack: r?.stack ?? null });
});

// ---- formatting ----
function fmtBytes(b) {
  const gib = b / 1073741824;
  if (gib >= 1) return `${gib.toFixed(1)} GiB`;
  const mib = b / 1048576;
  return `${mib.toFixed(0)} MiB`;
}

// Transfer rate, precise enough that a slow upload doesn't read as "0 MiB/s".
function fmtRate(bps) {
  if (!bps) return "";
  const mib = bps / 1048576;
  if (mib >= 10) return `${mib.toFixed(0)} MiB/s`;
  if (mib >= 1) return `${mib.toFixed(1)} MiB/s`;
  return `${Math.max(1, Math.round(bps / 1024))} KiB/s`;
}

// A rough "time left" from seconds: 45s / 12m / 3h 5m.
function fmtEta(sec) {
  sec = Math.round(sec);
  if (sec < 60) return `${sec}s`;
  const m = Math.round(sec / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

const MONTH = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const pad = (n) => String(n).padStart(2, "0");
const fmtClock = (d) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;
const fmtDay = (d) => `${MONTH[d.getMonth()]} ${d.getDate()}`;
// epoch seconds → "2026-06-18", the default name for a new trip
const isoDay = (sec) => {
  const d = new Date(sec * 1000);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
};

// A session's span, compact: same day shows one date + a clock range.
function fmtRange(startSec, endSec) {
  const s = new Date(startSec * 1000);
  const e = new Date(endSec * 1000);
  const sameDay = s.toDateString() === e.toDateString();
  if (sameDay) return `${fmtDay(s)} · ${fmtClock(s)}–${fmtClock(e)}`;
  return `${fmtDay(s)} → ${fmtDay(e)}`;
}

// A trip's date range, from the footage timestamps. Year shown only when it
// isn't the current one; a same-day trip collapses to one date.
function fmtTripRange(start, end) {
  if (!start && !end) return null;
  const s = new Date((start ?? end) * 1000);
  const e = new Date((end ?? start) * 1000);
  const yr = e.getFullYear();
  const tail = yr !== new Date().getFullYear() ? ` ${yr}` : "";
  if (s.toDateString() === e.toDateString()) return `${fmtDay(s)}${tail}`;
  const days = Math.floor((e - s) / 86400000) + 1;
  return `${fmtDay(s)} – ${fmtDay(e)}${tail} · ${days} days`;
}

function ago(sec) {
  const days = Math.floor((Date.now() / 1000 - sec) / 86400);
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return `${days}d ago`;
  return `${Math.floor(days / 30)}mo ago`;
}

function plural(n, w) {
  return `${n} ${w}${n === 1 ? "" : "s"}`;
}

// Each trip gets a stable colour from its name. Hues are hand-picked to be
// distinct from each other AND from the fixed sage(~158)/amber(~82) signals, so
// a trip's identity never reads as a safety state.
const TRIP_HUES = [352, 18, 42, 200, 222, 248, 274, 298, 322, 188];
function tripColor(name) {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (Math.imul(h, 31) + name.charCodeAt(i)) >>> 0;
  return `oklch(0.72 0.135 ${TRIP_HUES[h % TRIP_HUES.length]})`;
}

const HTML_ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
// Escape a value for interpolation into an HTML template string. Needed wherever
// a name reaches `innerHTML` — see `elHTML`.
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => HTML_ESCAPES[c]);
}

// Build an element. The third argument is **text**: it goes through
// `textContent`, so a filename, trip name, or a contributor's folder name out of
// the shared cloud can never inject markup. Names arrive from other people (a cloud
// `person/` folder is whatever they called it), so text is the only safe default.
function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
}

// `el`'s raw-HTML twin, for the handful of places that need child markup. ONLY
// pass markup this file controls — run any interpolated name through `esc()`.
function elHTML(tag, cls, html) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (html != null) n.innerHTML = html;
  return n;
}

// latest trips + inserted card, kept so the queue can re-render (`reflow`) from
// cache without a rescan, and the import dialog can offer trips as destinations
let lastTrips = [];
let lastCard = null;

// Trips with a push (share) in flight — a session-level Share pushes the WHOLE
// trip, so several sessions of one trip must not each kick off a duplicate upload
// of the same footage (which just fights itself and crawls). Keyed by trip name.
const pushingTrips = new Set();
// Latest push progress per trip (the `share_trip` channel payload), kept in module
// state — NOT a captured DOM element — so a Rescan/re-render can't orphan it. Every
// card or session owned by an uploading trip renders a bar tagged `data-share-trip`
// that `paintShareProgress` fills from here, so the upload stays visible throughout.
const shareProgress = new Map();
// The share queue: trip names waiting their turn to upload (the in-flight trip is
// in `pushingTrips`, not here). Uploads are bandwidth-bound, so the queue drains at
// concurrency 1 — every share action (a card's Share, a session's Share, "Share
// all") feeds it, so you can queue a batch and walk away. Module state, so a Rescan
// mid-batch re-renders without interrupting the drain.
const shareQueue = [];
let shareDraining = false;

let toastTimer;
function toast(msg) {
  // Every toast is something the user saw. Logging them all gives the log the
  // one thing a bug report usually can't supply: what was on screen, and when.
  logf("info", `toast: ${msg}`);
  const t = $("#toast");
  t.textContent = msg;
  t.hidden = false;
  requestAnimationFrame(() => t.classList.add("show"));
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), 2600);
}

// ---- thumbnails: a small queue so we don't spawn dozens of ffmpegs at once ----
let thumbQueue = [];
let thumbActive = 0;
const THUMB_MAX = 3;

function loadThumb(img, ref) {
  // A fileid alone is enough: posters are cached under it, so an archived trip —
  // whose footage is gone and whose cover therefore has no local path — still
  // resolves from cache. Only a ref with neither has nothing to look up.
  if (!ref || (!ref.path && !ref.fileid)) return; // leave the placeholder
  thumbQueue.push([img, ref]);
  pumpThumbs();
}
function pumpThumbs() {
  while (thumbActive < THUMB_MAX && thumbQueue.length) {
    const [img, ref] = thumbQueue.shift();
    thumbActive++;
    invoke("thumb", { path: ref.path, fileid: ref.fileid })
      .then((uri) => {
        if (uri) {
          img.src = uri;
          img.classList.add("loaded");
        }
      })
      .catch(() => {})
      .finally(() => {
        thumbActive--;
        pumpThumbs();
      });
  }
}
function frameImg(ref) {
  const img = el("img", "frame");
  img.alt = "";
  loadThumb(img, ref);
  return img;
}

// ---- share vocabulary (the shared cloud of everyone's clips — not a personal
// backup; your footage is "shared" once it's pushed up to the remote) ----
// The UI says "cloud" for what the engine calls the *cloud*: same thing, one word
// each side. The engine keeps `cloud` in its identifiers, so the fields arriving
// over IPC (`lastCloudCheck`, `cloudSynced`, `keptCloud`, `inCloud`, `cloudOk`) still
// read "cloud" — don't rename those chasing consistency, they're the wire format.
//
// The trip's live sync state as one chip, worst drift first. Driven by the
// network-free SyncBrief on the trip (tier-1 "to share" always; cloud-side counts
// from the last refresh's cache), so it rides along with list_trips.
function syncChip(t) {
  const s = t.sync || {};
  if (s.conflicts) return { cls: "warn", text: `⚠ ${plural(s.conflicts, "conflict")}`, act: true };
  if (s.pending) return { cls: "warn", text: `⟳ ${s.pending} queued`, act: true };
  if (s.deletedLocal) return { cls: "warn", text: `↑ ${s.deletedLocal} to clean`, act: true };
  if (s.toPush) return { cls: "new", text: `↑ ${s.toPush} to share`, act: true };
  // These two both live in the cloud and not here, so they can't both say "in
  // cloud": toPull is footage that's new to you, cloudOnly is footage you had and
  // freed. The wording matches the Sync panel's groups.
  if (s.toPull) return { cls: "new", text: `↓ ${s.toPull} new`, act: true };
  if (s.cloudOnly) return { cls: "unknown", text: `☁ ${s.cloudOnly} freed`, act: true };
  return { cls: "safe", text: "✓ In sync" };
}

// The chip names the drift; the Sync panel is where you resolve it — so the chip is
// the way in, rather than making you find the Sync… button (which isn't even on
// every card). Mirrors the share chip's affordance. "In sync" stays inert: nothing
// to act on.
function syncChipEl(t) {
  const spec = syncChip(t);
  const c = chip(spec);
  if (!spec.act) return c;
  c.classList.add("chip-action");
  c.title = "Open Sync to resolve this";
  c.setAttribute("role", "button");
  c.tabIndex = 0;
  c.onclick = (e) => {
    e.stopPropagation();
    openSync(t);
  };
  c.onkeydown = (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      openSync(t);
    }
  };
  return c;
}
function chip(spec) {
  return el("span", `chip ${spec.cls}`, spec.text);
}

// A trip's sharing as a clickable chip — the dashboard's at-a-glance signal and
// the one-click entry to the Sharing panel (no ⋯-menu digging). Driven by the
// network-free `sharedWith` on the trip, so it rides along with list_trips:
//   null   → not in the cloud / not checked yet → no chip
//   0      → in the cloud, shared with nobody   → quiet "🔗 Share…" call-to-action
//   n > 0  → shared with n people              → "🔗 Shared · N"
// So an unshared-but-shareable trip gets a fast share button right on the card.
function sharedChip(t) {
  if (t.sharedWith == null) return null;
  const shared = t.sharedWith > 0;
  const c = el(
    "span",
    `chip share-chip${shared ? "" : " share-cta"}`,
    shared ? `🔗 Shared · ${t.sharedWith}` : "🔗 Share…"
  );
  c.title = shared
    ? `Shared with ${peopleCount(t.sharedWith)} — click to manage`
    : "Not shared yet — click to add friends";
  c.setAttribute("role", "button");
  c.tabIndex = 0;
  c.onclick = (e) => {
    e.stopPropagation();
    openShare(t);
  };
  c.onkeydown = (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      openShare(t);
    }
  };
  return c;
}

// Patch a trip card's share chip in place after a fetch/add/remove, so the count
// stays live without a full re-render (e.g. while the panel is open over the grid).
function updateShareChip(t, n) {
  t.sharedWith = n; // keep the in-memory model in step for later re-renders
  const card = [...document.querySelectorAll(".trip")].find((c) => c.dataset.trip === t.name);
  const chips = card && card.querySelector(".trip-chips");
  if (!chips) return;
  const old = chips.querySelector(".share-chip");
  if (old) old.remove();
  const c = sharedChip(t);
  if (c) chips.append(c);
}

// A session's capture counts. Videos and photos are one unit for the
// import/clear logic — a photo (a picture, or a stitched panorama) reads the same
// as a clip: new → import, imported → clear/share. The photo subset is carried
// only so the meta line can show a "· N photos" breakdown.
function sessionCounts(s) {
  const total = s.captures;
  return {
    total,
    newN: s.newCaptures,
    disc: s.discarded,
    imp: total - s.newCaptures - s.discarded, // imported (owned)
    photos: s.photos,
    videos: total - s.photos,
  };
}

// A session's status chips + its primary action, by import/share state. `act`
// drives which handler the button runs: import, clear (reclaim), share, or trash.
function sessionStatus(s) {
  const c = sessionCounts(s);
  const where = s.owners.length ? `in ${s.owners.join(", ")}` : "imported";
  const disc = c.disc ? [{ cls: "warn", text: `${c.disc} discarded` }] : [];

  // everything here is trash you already deleted → just clear it off the card
  if (c.disc === c.total) {
    return { chips: disc, action: "Clear trash →", act: "cleartrash" };
  }
  // nothing imported yet → import it all
  if (c.imp === 0) {
    return {
      chips: [{ cls: "new", text: `● ${plural(c.newN, "clip")} new` }],
      action: "Import →",
      act: "import",
    };
  }
  // everything imported → clear (if inCloud) or share
  if (c.newN === 0) {
    if (s.safe)
      return {
        chips: [{ cls: "safe", text: "✓ Safe to clear" }, { cls: "owned", text: where }, ...disc],
        action: "Clear →",
        act: "clear",
      };
    return {
      chips: [{ cls: "owned", text: where }, ...disc, { cls: "warn", text: "⚠ Share to clear" }],
      action: "Share →",
      act: "share",
    };
  }
  // mixed: some imported, some still new
  return {
    chips: [
      { cls: "new", text: `● ${plural(c.newN, "clip")} new` },
      { cls: "owned", text: `${plural(c.imp, "clip")} ${where}` },
      ...disc,
    ],
    action: "Import new →",
    act: "import",
  };
}

function sessionAction(act, s) {
  if (act === "import") return openImport(s);
  if (act === "clear")
    return openWipe({ window: [s.start, s.end], label: `the ${fmtRange(s.start, s.end)} session` });
  if (act === "cleartrash") return clearTrash(s);
  // "share" is handled in renderCard (it needs the row's action element for the
  // inline progress bar); nothing else reaches here.
}

// ---- render: inserted card ----
function renderCard(card) {
  const panel = $("#card-panel");
  panel.innerHTML = "";

  if (!card) {
    const empty = el("div", "card-empty");
    empty.append(
      elHTML("div", null, "<strong>No card inserted</strong>"),
      el("div", "hint", "Insert a DJI, GoPro, or iPhone card to import a session.")
    );
    panel.append(empty);
    return;
  }

  const box = el("div", "cardbox");
  const head = el("div", "cardbox-head");
  const id = el("div", "card-id");
  id.append(el("span", "card-h", "Card inserted"), el("span", "card-path", card.roots[0] ?? ""));
  const vids = card.captures - card.photos;
  const totals = [];
  if (vids) totals.push(plural(vids, "clip"));
  if (card.photos) totals.push(plural(card.photos, "photo"));
  totals.push(fmtBytes(card.bytes), plural(card.sessions.length, "session"));
  head.append(id, el("span", "card-totals tnum", totals.join(" · ")));
  box.append(head);

  const newCount = card.sessions.filter((s) => s.newCaptures > 0).length;
  const safeCount = card.sessions.filter((s) => s.safe).length;
  const safety = el("div", "card-safety");
  safety.append(el("span", "dot"));
  if (newCount === 0 && safeCount === card.sessions.length && card.sessions.length) {
    safety.classList.add("all-safe");
    safety.append(el("span", null, "Every clip is shared — safe to reclaim this card."));
    const reclaim = el("button", "btn small ghost reclaim", "Reclaim card →");
    reclaim.type = "button";
    reclaim.onclick = () => openWipe({ window: null, label: "every session on this card" });
    safety.append(reclaim);
  } else {
    const parts = [];
    if (newCount) parts.push(`${plural(newCount, "session")} to import`);
    if (safeCount) parts.push(`${safeCount} safe to clear`);
    const stuck = card.sessions.filter((s) => {
      const c = sessionCounts(s);
      return c.total > 0 && c.newN === 0 && c.disc < c.total && !s.safe;
    }).length;
    if (stuck) parts.push(`${stuck} imported, not shared`);
    const disc = card.sessions.reduce((a, s) => a + sessionCounts(s).disc, 0);
    if (disc) parts.push(`${disc} discarded`);
    if (newCount) safety.classList.add("has-new");
    safety.append(el("span", null, parts.join(" · ") || "Nothing to do here."));
    // Escape hatch: clear imported footage off the card without a cloud check, for
    // when there's no internet. Deliberately subdued — the inCloud path is safer.
    if (stuck) {
      const off = el("button", "btn small subtle", "Clear offline…");
      off.type = "button";
      off.title = "Delete imported clips from the card without a cloud check (for when you're offline)";
      off.onclick = () => openWipe({ window: null, label: "every imported clip on this card", offline: true });
      safety.append(off);
    }
  }
  box.append(safety);

  const list = el("div", "sessions");
  for (const s of card.sessions) {
    const row = el("div", "session");
    const c = sessionCounts(s);

    // The contact strip: a few frames spread across the session's captures (videos
    // and photos alike). Click it to watch the session before importing (read-only).
    const strip = el("div", "strip");
    for (const ref of s.strip) strip.append(frameImg(ref));
    if (s.captures > s.strip.length)
      strip.append(el("div", "more tnum", `+${s.captures - s.strip.length}`));
    strip.append(elHTML("div", "strip-play", ICON_PLAY));
    strip.setAttribute("role", "button");
    strip.tabIndex = 0;
    strip.title = "Preview this session";
    strip.setAttribute("aria-label", `Preview the ${fmtRange(s.start, s.end)} session`);
    const preview = () => openCardPreview([s.start, s.end], `Card · ${fmtRange(s.start, s.end)}`);
    strip.onclick = preview;
    strip.onkeydown = (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        preview();
      }
    };

    const when = el("div", "when");
    when.append(el("div", "range tnum", fmtRange(s.start, s.end)), el("div", "ago", ago(s.end)));

    const st = sessionStatus(s);
    const status = el("div", "status");
    for (const ch of st.chips) status.append(chip(ch));

    const actions = el("div", "actions");
    // An owner uploading now → show its live bar in place of the Share button, so
    // the push is visible here and can't be double-started. Survives a Rescan. An
    // owner still queued → the muted "Queued to share…" placeholder.
    const uploadingOwner = s.owners.find((o) => pushingTrips.has(o));
    const queuedOwner = s.owners.find((o) => isQueued(o));
    if (uploadingOwner) {
      actions.append(shareProgressBox(uploadingOwner));
    } else if (queuedOwner) {
      actions.append(shareQueuedBox(queuedOwner));
    } else {
      const primary = st.act === "import" || st.act === "share";
      const btn = el("button", `btn small ${primary ? "primary" : "ghost"}`, st.action);
      btn.type = "button";
      btn.onclick = st.act === "share" ? () => shareSession(s) : () => sessionAction(st.act, s);
      actions.append(btn);
    }

    // Meta: how many videos, how many photos, and the total size.
    const metaParts = [];
    if (c.videos) metaParts.push(plural(c.videos, "clip"));
    if (c.photos) metaParts.push(plural(c.photos, "photo"));
    metaParts.push(fmtBytes(s.bytes));

    row.append(strip, when, el("div", "s-meta tnum", metaParts.join("  ·  ")), status, actions);
    list.append(row);
  }
  box.append(list);
  panel.append(box);
  paintAllShares(); // fill any freshly-rendered upload bars from live progress
}

// ---- render: trips ----
const NEXT_LABEL = { review: "Review →", cut: "Cut →", edit: "Edit →", import: "Import →" };

// "12 yours · 3 pulled from alice" — shown only when a trip mixes your footage
// with clips pulled from the shared cloud; an all-yours trip needs no caption.
function provRow(t) {
  if (!t.pulled) return null;
  const names = t.contributors || [];
  const who =
    names.length > 2 ? `${names.slice(0, 2).join(", ")} +${names.length - 2}` : names.join(", ");
  const row = el("div", "prov tnum");
  const pulled = elHTML("span", null, `<span class="n">${t.pulled}</span> pulled`);
  // Contributor names are cloud folder names — other people choose them, so they
  // go in as text, never markup.
  if (who) pulled.append(" ", el("span", "prov-from", `from ${who}`));
  row.append(
    elHTML("span", null, `<span class="n">${t.mine}</span> yours`),
    el("span", "prov-sep", "·"),
    pulled
  );
  return row;
}

// The trip card's footer row: size on the left, actions on the right. A "Share"
// action appears whenever you have footage here that isn't in the cloud yet — it's
// the step that flips the trip to ✓ Shared and its card sessions to safe-to-clear.
function footActions(t, card) {
  // Uploading now → the whole action row becomes the live bar (fed by module state,
  // so a Rescan re-creates and refills it). No Share button to double-click.
  if (pushingTrips.has(t.name)) {
    return [el("div", "trip-size tnum", fmtBytes(t.bytes)), shareProgressBox(t.name)];
  }
  // Waiting its turn in the share queue → a muted "Queued to share…" with a ✕.
  if (isQueued(t.name)) {
    return [el("div", "trip-size tnum", fmtBytes(t.bytes)), shareQueuedBox(t.name)];
  }
  const actions = el("div", "trip-actions");
  const s = t.sync || {};
  // An archived trip has no raw here, so every action on it is a lie except one:
  // bring the footage back. Editing especially — the cut clips are still local, but
  // opening them offers no way to change a single edge, and `Edit →` sitting there
  // reads as "this trip is ready to work on" when it's the opposite.
  if (t.state === "archived") {
    const back = elHTML("button", "btn small primary", '<span class="btn-ico" aria-hidden="true">↓</span> Restore');
    back.type = "button";
    back.title = `Bring ${t.name}'s footage back from the cloud`;
    back.onclick = () => openSync(t, "restoreCloud");
    actions.append(back);
    return [el("div", "trip-size tnum", fmtBytes(t.bytes)), actions];
  }
  // Share is the quick path when you have footage that isn't up yet. Hidden while
  // ops are queued (a rename/move must replay first) — Sync handles those.
  if (owesShare(t)) {
    const share = elHTML("button", "btn small ghost", '<span class="btn-ico" aria-hidden="true">↑</span> Share');
    share.type = "button";
    share.onclick = () => shareTripNow(t);
    actions.append(share);
  }
  // Sync surfaces the rest of the drift — pull, cleanup, conflicts, owed ops, or a
  // trip whose cloud state hasn't been checked yet.
  const otherDrift = s.toPull || s.deletedLocal || s.conflicts || s.pending || s.lastCloudCheck == null;
  if (t.masters > 0 && otherDrift) {
    const sync = elHTML("button", "btn small ghost", '<span class="btn-ico" aria-hidden="true">⇅</span> Sync…');
    sync.type = "button";
    sync.onclick = () => openSync(t);
    actions.append(sync);
  }
  // once everything's provably in the cloud, the local raw can be freed — kept
  // re-pullable, with clips/marks staying put
  if (t.masters > 0 && s.toPush === 0) {
    const arch = el("button", "btn small ghost", "Archive →");
    arch.type = "button";
    arch.onclick = () => openArchive(t);
    actions.append(arch);
  }
  // Once a trip has marks, Cut and Edit are both available and their order is
  // **fixed**: Cut then Edit, always. They used to be placed by pipeline position —
  // whichever was the "next step" went last, as the primary — so the same two
  // buttons swapped sides depending on whether the trip had been cut yet, and two
  // cards side by side disagreed about where Edit lived.
  //
  // The pipeline framing is what stopped being true: with the timeline export you
  // no longer have to cut before you can edit. Cut is an optional deliverable
  // (standalone files to hand someone), Edit is where the trip is going. So Edit is
  // the primary and the rightmost from the moment there are marks, and Cut sits to
  // its left in the same place whether it says "cut these" or "cut the new ones".
  const marked = t.marks > 0 && (t.masters > 0 || t.clips > 0);
  if (marked) {
    const cut = elHTML("button", "btn small ghost", '<span class="btn-ico" aria-hidden="true">✂</span> Cut');
    cut.type = "button";
    cut.title = t.clips > 0
      ? `Cut ${t.name}'s new marks into clips (existing ones are left alone)`
      : `Cut ${t.name}'s ${plural(t.marks, "mark")} into standalone clips`;
    cut.onclick = () => startCut(t, card);
    actions.append(cut);
  }
  const nextKey = marked && t.masters > 0 ? "edit" : t.next;
  const next = el("button", "btn small primary", NEXT_LABEL[nextKey] ?? nextKey);
  next.type = "button";
  next.onclick =
    nextKey === "review"
      ? () => openReview(t)
      : nextKey === "cut"
        ? () => startCut(t, card)
        : nextKey === "edit"
          ? () => openInEditor(t)
          : () => toast(`"${nextKey}" for ${t.name} is coming in a later build.`);
  actions.append(next);
  return [el("div", "trip-size tnum", fmtBytes(t.bytes)), actions];
}

function renderTrips(trips) {
  const wrap = $("#trips");
  const shelf = $("#archived-trips");
  wrap.innerHTML = "";
  shelf.innerHTML = "";
  renderShareAll(); // the "Share all" / "Sharing… N left" control in the panel head

  // Archived trips are done with — their raw is in the cloud and there's nothing
  // here to act on — so they come off the dashboard into a shelf you can open,
  // rather than padding the grid you actually work in. `state` is written down by
  // `archive`, not inferred, so nothing lands here by accident.
  const live = trips.filter((t) => t.state !== "archived");
  const archived = trips.filter((t) => t.state === "archived");

  $("#trips-sub").textContent = live.length ? plural(live.length, "trip") : "";
  $("#archived-panel").hidden = archived.length === 0;
  $("#archived-sub").textContent = archived.length ? plural(archived.length, "trip") : "";

  if (!trips.length) {
    wrap.append(
      el("div", "empty-note", "No trips yet. Insert a card and import a session, or pull one from the cloud.")
    );
    return;
  }
  if (!live.length) {
    wrap.append(
      el("div", "empty-note", "Everything's archived — your footage is in the cloud. Import a card to start a new trip.")
    );
  }

  for (const t of live) wrap.append(tripCard(t));
  for (const t of archived) shelf.append(tripCard(t));
  paintAllShares(); // fill any freshly-rendered upload bars from live progress
}

function tripCard(t) {
  const card = el("div", "trip" + (t.state === "archived" ? " is-archived" : ""));
  card.dataset.trip = t.name; // lets warmShareChips/updateShareChip patch this card
  card.style.setProperty("--tc", tripColor(t.name));

  const cover = el("div", "cover-wrap");
  if (t.cover) {
    const img = el("img", "cover");
    img.alt = "";
    cover.append(img);
    loadThumb(img, t.cover);
  }
  // An archived trip keeps its cover (the poster cache outlives the footage) but
  // has nothing local to play — say so rather than leaving a dead tile.
  if (t.masters === 0 && t.state === "archived") {
    cover.title = `${t.name} is archived — bring its footage back to review it`;
  }
  // any trip with footage can be skimmed — the cover is the way in
  if (t.masters > 0) {
    cover.classList.add("playable");
    cover.append(elHTML("div", "cover-play", '<span aria-hidden="true">▶</span>'));
    cover.setAttribute("role", "button");
    cover.tabIndex = 0;
    cover.title = `Review ${t.name}`;
    cover.onclick = () => openReview(t);
    cover.onkeydown = (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        openReview(t);
      }
    };
  }
  card.append(cover);

  const body = el("div", "trip-body");

  const top = el("div", "trip-top");
  const nameRow = el("div", "trip-name-row");
  nameRow.append(el("div", "trip-name", t.name));
  const menuBtn = el("button", "trip-menu", "⋯");
  menuBtn.type = "button";
  menuBtn.setAttribute("aria-label", `More actions for ${t.name}`);
  menuBtn.title = "Organize · rename · move · pull · delete";
  menuBtn.onclick = (e) => {
    e.stopPropagation();
    openTripMenu(menuBtn, t);
  };
  nameRow.append(menuBtn);
  top.append(nameRow);
  const range = fmtTripRange(t.start, t.end) || (t.from || t.to ? `${t.from ?? "…"} → ${t.to ?? "…"}` : null);
  if (range) top.append(el("div", "trip-window", range));

  const chips = el("div", "trip-chips");
  const badge = elHTML("span", "badge", `<span class="dot"></span>${esc(t.state)}`);
  badge.dataset.state = t.state;
  chips.append(badge);
  // live sync state — also shown on archived trips (no local masters) so their
  // cloud-only footage stays visible
  if (t.masters > 0 || t.sync.cloudOnly > 0) chips.append(syncChipEl(t));
  // who this trip's cloud folder is shared with (network-free, from the cache)
  const sc = sharedChip(t);
  if (sc) chips.append(sc);

  const stats = el("div", "stats tnum");
  // An archived trip has no local masters, so the plain count read "0 clips" — the
  // one thing that isn't true. Its footage is in the cloud; count it there.
  const held =
    t.state === "archived"
      ? `<span class="n">${t.sync.cloudOnly}</span> in cloud`
      : `<span class="n">${t.masters}</span> clips`;
  stats.append(
    elHTML("div", null, held),
    elHTML("div", null, `<span class="n">${t.marks}</span> marks`),
    elHTML("div", null, `<span class="n">${t.clips}</span> cut`)
  );

  const foot = el("div", "trip-foot");
  foot.append(...footActions(t, card));

  body.append(top, chips);
  const prov = provRow(t);
  if (prov) body.append(prov);
  body.append(stats, foot);
  card.append(body);
  return card;
}

// ---- share: push a trip's footage to the cloud, with progress inline on the card ----
function shareSummary(r) {
  if (r.uploaded > 0)
    return `Shared ${plural(r.files, "clip")} · ${fmtBytes(r.uploaded)} up to the cloud`;
  return `${r.trip} already shared — ${plural(r.files, "clip")} verified in the cloud`;
}

// A push-progress bar bound to a trip by a `data-share-trip` marker (not a captured
// element reference). A re-render re-creates it; `paintShareProgress` refills it from
// module state — so the readout survives a Rescan while the upload keeps running.
function shareProgressBox(trip) {
  const box = el("div", "share-prog");
  box.dataset.shareTrip = trip;
  box.append(el("div", "share-row"), el("div", "bar"));
  box.firstChild.append(el("span", "share-stage", "Preparing…"), el("span", "share-pct tnum", ""));
  box.lastChild.append(el("div", "bar-fill"));
  return box;
}

// Fill every on-screen bar for `trip` from the stored progress (upload line shows
// real figures — bytes done / total · rate · time left — so a slow transfer reads
// as slow, not stuck). Matches by dataset, so no CSS-selector escaping of names.
function paintShareProgress(trip) {
  const p = shareProgress.get(trip);
  for (const box of document.querySelectorAll("[data-share-trip]")) {
    if (box.dataset.shareTrip !== trip) continue;
    const stage = box.querySelector(".share-stage");
    const pct = box.querySelector(".share-pct");
    const fill = box.querySelector(".bar-fill");
    if (!stage || !pct || !fill) continue;
    const setFill = (f) => (fill.style.transform = `scaleX(${Math.max(0, Math.min(1, f))})`);
    if (!p) {
      stage.textContent = "Preparing…";
      pct.textContent = "";
      setFill(0);
    } else if (p.phase === "verify") {
      // upload's done — hold the bar full and let the file count tick up
      stage.textContent = p.total ? `Verifying ${p.done}/${p.total}…` : "Verifying…";
      pct.textContent = "";
      setFill(1);
    } else {
      const f = p.total ? p.done / p.total : 0;
      const bits = [`${fmtBytes(p.done)} / ${fmtBytes(p.total)}`];
      if (p.speed) bits.push(fmtRate(p.speed));
      if (p.eta > 0) bits.push(`${fmtEta(p.eta)} left`);
      stage.textContent = bits.join(" · ");
      pct.textContent = `${Math.round(f * 100)}%`;
      setFill(f);
    }
  }
}

// Repaint every in-flight trip's bars — called after a render so freshly-created
// boxes pick up the current progress.
function paintAllShares() {
  for (const trip of pushingTrips) paintShareProgress(trip);
}

// Wire a `share_trip` Channel to module state: each message updates the trip's
// stored progress and repaints its bars wherever they are on screen.
function bindShareProgress(channel, trip) {
  if (!channel) return;
  channel.onmessage = (p) => {
    shareProgress.set(trip, p);
    paintShareProgress(trip);
  };
}

// Re-render the card + trips from cached state (no rescan) — reflects a share-queue
// change instantly on every card and session row.
function reflow() {
  renderCard(lastCard);
  renderTrips(lastTrips);
}

function isQueued(name) {
  return shareQueue.includes(name);
}

// A trip owes a share when you hold masters here that aren't in the cloud yet and no
// owed op is blocking — the same gate as the card's Share button. Drives "Share all".
function owesShare(t) {
  const s = t.sync || {};
  return t.mine > 0 && s.toPush > 0 && !s.pending;
}
function owedShareTrips() {
  return lastTrips.filter(owesShare);
}

// Add trips to the share queue and start draining if idle. Skips any already
// uploading or queued (so a second click never fights the first push), and
// re-renders so the cards show "Queued…" / the live bar at once. Returns the
// number newly enqueued.
function enqueueShares(names) {
  let added = 0;
  for (const name of new Set(names)) {
    if (!name || pushingTrips.has(name) || isQueued(name)) continue;
    shareQueue.push(name);
    added++;
  }
  if (added) {
    reflow();
    drainShareQueue();
  }
  return added;
}

// Drop a trip that's still waiting its turn (the in-flight upload can't be pulled).
function dequeueShare(name) {
  const i = shareQueue.indexOf(name);
  if (i < 0) return;
  shareQueue.splice(i, 1);
  reflow();
}

// Drain the queue one trip at a time until it's empty. Guarded so only one drain
// loop runs; module state means a Rescan mid-batch re-renders but never interrupts
// it. Each trip's progress paints via `shareProgress` wherever its bars are.
async function drainShareQueue() {
  if (shareDraining) return;
  shareDraining = true;
  try {
    while (shareQueue.length) {
      const name = shareQueue.shift();
      const t = lastTrips.find((x) => x.name === name);
      if (!t) continue; // renamed/deleted before its turn — skip
      await pushTrip(t);
    }
  } finally {
    shareDraining = false;
    reflow(); // reset the "Share all" button out of its "Sharing…" state
  }
}

// Upload one trip's owed footage and verify it. Marks the trip in `pushingTrips` so
// its live bar shows on every card and session row it owns (rescan-proof), then
// reloads so its chip flips to ✓ In sync. The queue calls this; never call it
// directly (go through `enqueueShares` so it's serialized).
async function pushTrip(t) {
  if (pushingTrips.has(t.name)) return; // belt and braces
  pushingTrips.add(t.name);
  shareProgress.delete(t.name);

  const Ch = window.__TAURI__?.core?.Channel;
  const channel = Ch ? new Ch() : null;
  bindShareProgress(channel, t.name);
  reflow(); // swap this trip's Share/Queued box for its live bar now
  paintShareProgress(t.name);

  try {
    const res = await invoke("share_trip", { channel, trip: t.name });
    toast(shareSummary(res));
  } catch (e) {
    toast(String(e));
  } finally {
    pushingTrips.delete(t.name);
    shareProgress.delete(t.name);
    await load(); // re-render: chip flips to ✓ In sync, the Share button drops off
    // The trip is now in the cloud → shareable. Refresh its share cache so the
    // "🔗 Share…" quick-action chip appears without waiting for the next launch
    // (the once-per-session warm-up has already run). Best-effort.
    invoke("trip_shares", { trip: t.name })
      .then((s) => updateShareChip(t, s.length))
      .catch(() => {});
  }
}

// A trip card's Share button: queue this one trip. A second click while it's
// uploading/queued just says so (it's already in the queue) instead of fighting it.
function shareTripNow(t) {
  if (!enqueueShares([t.name])) toast(`${t.name} is already uploading or queued…`);
}

// A card session that's imported but "Share to clear": queue its owning trip(s) so
// the footage is safe to wipe off the card. Same push as a trip card's Share, from
// the session row; on completion the re-scan flips the session to "✓ Safe to clear".
function shareSession(s) {
  const owners = [...new Set(s.owners)].filter((name) => lastTrips.some((t) => t.name === name));
  if (!owners.length) return toast("Can't find the trip these clips are in.");
  if (!enqueueShares(owners)) toast(`${owners.join(", ")} already uploading or queued…`);
}

// A muted "Queued to share…" placeholder shown on a card/session while the trip
// waits its turn, with a ✕ to drop it from the queue. Tagged by trip so a Rescan
// re-creates it (never a captured reference). Static text only — no name in innerHTML.
function shareQueuedBox(trip) {
  const box = el("div", "share-queued");
  box.dataset.queuedTrip = trip;
  box.append(el("span", "queued-label", "Queued to share…"));
  const x = el("button", "queued-cancel", "✕");
  x.type = "button";
  x.title = "Remove from the share queue";
  x.setAttribute("aria-label", `Remove ${trip} from the share queue`);
  x.onclick = (e) => {
    e.stopPropagation();
    dequeueShare(trip);
  };
  box.append(x);
  return box;
}

// The Trips-panel "Share all" control: queue every trip that owes a share and walk
// away. While a batch drains it reports how many are left (and disables, since the
// queue is already running); with nothing owed it shows nothing.
function renderShareAll() {
  const slot = $("#share-all-slot");
  if (!slot) return;
  slot.innerHTML = "";
  const remaining = shareQueue.length + pushingTrips.size;
  if (remaining > 0) {
    const b = el("button", "btn small ghost", `↑ Sharing… ${remaining} left`);
    b.type = "button";
    b.disabled = true;
    slot.append(b);
    return;
  }
  const owed = owedShareTrips();
  if (!owed.length) return;
  const b = el("button", "btn small primary", `↑ Share all · ${owed.length}`);
  b.type = "button";
  b.title = `Queue ${plural(owed.length, "trip")} and upload them one after another — you can walk away`;
  b.onclick = () => {
    const added = enqueueShares(owed.map((t) => t.name));
    if (added) toast(`Queued ${plural(added, "trip")} to share — you can leave it running`);
  };
  slot.append(b);
}

// ---- cut: write a trip's marked ranges into clips/, progress inline on the card ----
function cutSummary(r) {
  const bits = [`Cut ${plural(r.made, "clip")}`];
  if (r.skipped) bits.push(`${r.skipped} already there`);
  if (r.failed) bits.push(`${r.failed} failed`);
  return bits.join(" · ");
}

async function startCut(t, card) {
  const foot = card.querySelector(".trip-foot");
  if (!foot || card.dataset.cutting) return;
  card.dataset.cutting = "1";

  // Reuse the share progress readout (bar + row), tinted the trip's own colour.
  foot.innerHTML = "";
  const stage = el("span", "share-stage", "Cutting…");
  const pct = el("span", "share-pct tnum", "");
  const fill = el("div", "bar-fill");
  const prog = el("div", "share-prog");
  prog.append(el("div", "share-row"), el("div", "bar"));
  prog.firstChild.append(stage, pct);
  prog.lastChild.append(fill);
  foot.append(prog);
  const setFill = (f) => (fill.style.transform = `scaleX(${Math.max(0, Math.min(1, f))})`);
  setFill(0);

  const Ch = window.__TAURI__?.core?.Channel;
  const channel = Ch ? new Ch() : null;
  if (channel)
    channel.onmessage = (p) => {
      stage.textContent = p.file ? `Cutting ${p.file}` : "Cutting…";
      pct.textContent = p.count ? `${p.index}/${p.count}` : "";
      setFill(p.count ? p.index / p.count : 0);
    };

  try {
    const res = await invoke("cut_trip", { channel, trip: t.name });
    card.dataset.cutting = "";
    toast(cutSummary(res));
    await load(); // re-render: the "cut" stat ticks up, state advances toward edit
  } catch (e) {
    card.dataset.cutting = "";
    foot.innerHTML = "";
    foot.append(el("div", "share-error", String(e)), ...footActions(t, card));
  }
}

// Hand the trip off to Kdenlive — the pipeline's last step. The editor is
// launched detached engine-side, so this returns as soon as it's handed over.
// Normally that hand-off is a built timeline (every mark, in order, against its
// master); a trip with no marks still opens its loose files the old way.
async function openInEditor(t) {
  try {
    const r = await invoke("open_in_editor", { trip: t.name });
    const tl = r.timeline;
    if (!tl) return toast(`Opening ${plural(r.files, "clip")} in Kdenlive…`);
    // Name what was left out rather than quietly building a shorter timeline —
    // a mark whose raw is archived can't be placed, and a silent drop would read
    // as reel losing it.
    const bits = [`${plural(tl.segments, "mark")} as a timeline`, fmtTime(tl.duration)];
    if (tl.skipped) bits.push(`${tl.skipped} skipped (raw missing)`);
    // Kdenlive's project profile has to be one of its presets. When the footage
    // matches none — portrait drone video at 23.98 is the real case — it opens at
    // Kdenlive's *default* instead, quietly rendering the picture at the wrong size
    // and rate. Nothing downstream would tell you, so it's said here.
    // Kdenlive won't take a fractional rate on non-standard geometry (portrait drone
    // footage), so the project is built at the nearest whole rate instead. Small —
    // well under a frame across a trip — but it's a change to the footage's own
    // timing, so it's named rather than done quietly.
    if (tl.conformedFrom) bits.push(`${tl.profile} project (footage is ${tl.conformedFrom})`);
    else if (!tl.profileId) bits.push(`set the project profile to ${tl.profile} in Kdenlive`);
    toast(`Opening ${bits.join(" · ")}`);
  } catch (e) {
    toast(String(e));
  }
}

// ---- import dialog ----
const dlg = $("#import-dialog");
const impName = $("#import-trip");
const impPicks = $("#import-picks");
const impMeta = $("#import-meta");
const impHint = $("#import-hint");
const impErr = $("#import-error");
const impProg = $("#import-progress");
const impFile = $("#imp-file");
const impPct = $("#imp-pct");
const impBar = $("#imp-bar");
const impGo = $("#import-go");
const impCancel = $("#import-cancel");

let impSession = null;
let importing = false;

// Recolour the dialog to the destination trip: drives the input focus ring and
// the progress bar, and lights the matching quick-pick.
function paintTc(name) {
  dlg.style.setProperty("--tc", tripColor(name || "trip"));
  for (const b of impPicks.children) {
    b.setAttribute("aria-pressed", b.dataset.name === name ? "true" : "false");
  }
}

function setBar(frac) {
  impBar.style.transform = `scaleX(${Math.max(0, Math.min(1, frac))})`;
}

function openImport(s) {
  impSession = s;
  const c = sessionCounts(s);
  // What will actually copy in: the new captures (or everything, if none imported
  // yet). Photos and videos are one unit and counted together.
  const freshLabel = plural(c.newN || c.total, "clip");
  impMeta.textContent = `${fmtRange(s.start, s.end)} · ${freshLabel} · ${fmtBytes(s.bytes)}`;
  const alreadyImp = c.imp;
  impHint.textContent =
    alreadyImp > 0
      ? `${alreadyImp} already imported — only the new ${plural(c.newN, "clip")} copy in.`
      : "A new name creates a trip; pick one below to add to it.";

  // existing trips as quick picks, each dotted in its own colour
  impPicks.innerHTML = "";
  for (const t of lastTrips) {
    const b = elHTML("button", "trip-pick", '<span class="pdot"></span>');
    b.append(t.name); // trip name as text, never markup
    b.type = "button";
    b.dataset.name = t.name;
    b.style.setProperty("--pc", tripColor(t.name));
    b.onclick = () => {
      impName.value = t.name;
      paintTc(t.name);
    };
    impPicks.append(b);
  }
  impPicks.hidden = lastTrips.length === 0;

  // default: the sole owning trip if there is one, else the capture date
  impName.value = s.owners && s.owners.length === 1 ? s.owners[0] : isoDay(s.start);

  impErr.hidden = true;
  impProg.hidden = true;
  setBar(0);
  impName.disabled = false;
  impGo.disabled = false;
  impGo.textContent = "Import";
  paintTc(impName.value);

  dlg.showModal();
  impName.focus();
  impName.select();
}

function importSummary(r) {
  const bits = [];
  const vids = r.copied - r.photos;
  if (vids) bits.push(plural(vids, "clip"));
  if (r.photos) bits.push(plural(r.photos, "photo"));
  if (bits.length) {
    const extra = r.skippedOther ? ` · ${r.skippedOther} left for other trips` : "";
    return `Imported ${bits.join(" + ")} · ${fmtBytes(r.bytes)} into ${r.trip}${extra}`;
  }
  if (r.skippedHere) return `Already in ${r.trip} — nothing new to import.`;
  if (r.skippedOther) return "Those clips belong to other trips — nothing imported.";
  return "Nothing to import.";
}

function onImportProgress(p) {
  impProg.hidden = false;
  impFile.textContent = p.file;
  const frac = p.totalBytes ? p.copiedBytes / p.totalBytes : 0;
  impPct.textContent = `${Math.round(frac * 100)}%  ·  ${p.fileIndex}/${p.fileCount}`;
  setBar(frac);
}

function failImport(msg) {
  impErr.textContent = msg;
  impErr.hidden = false;
  impProg.hidden = true;
  impName.disabled = false;
  impGo.disabled = false;
  impGo.textContent = "Import";
}

async function runImport() {
  const name = impName.value.trim();
  if (!name) return failImport("Name the trip first.");
  if (name.includes("/") || name.includes("\\") || name === "." || name === "..")
    return failImport("That trip name isn't valid.");

  importing = true;
  impErr.hidden = true;
  impName.disabled = true;
  impGo.disabled = true;
  impGo.textContent = "Importing…";
  impProg.hidden = false;
  impFile.textContent = "preparing…";
  impPct.textContent = "0%";
  setBar(0);

  const Ch = window.__TAURI__?.core?.Channel;
  const channel = Ch ? new Ch() : null;
  if (channel) channel.onmessage = onImportProgress;

  try {
    const res = await invoke("import_session", {
      channel,
      trip: name,
      start: impSession.start,
      end: impSession.end,
    });
    importing = false;
    dlg.close();
    toast(importSummary(res));
    await load();
  } catch (e) {
    importing = false;
    failImport(String(e));
  }
}

impName.addEventListener("input", () => paintTc(impName.value.trim()));
impName.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    runImport();
  }
});
impGo.addEventListener("click", runImport);
impCancel.addEventListener("click", () => dlg.close());
// don't let Escape / backdrop close a copy in flight
dlg.addEventListener("cancel", (e) => {
  if (importing) e.preventDefault();
});
dlg.addEventListener("click", (e) => {
  if (e.target === dlg && !importing) dlg.close();
});

// ---- destructive-confirm dialog: stream a cloud check, show the plan, then on
// confirm stream the commit. Shared by card reclaim and trip archive. ----
const cdlg = $("#confirm-dialog");
const cTitle = $("#confirm-title");
const cMeta = $("#confirm-meta");
const cCheck = $("#confirm-check");
const cStage = $("#confirm-stage");
const cCount = $("#confirm-count");
const cBar = $("#confirm-bar");
const cSummary = $("#confirm-summary");
const cErr = $("#confirm-error");
const cGo = $("#confirm-go");
const cAlt = $("#confirm-alt");
const cCancel = $("#confirm-cancel");

let confirmBusy = false; // true while committing — block dismiss mid-delete

function setCBar(frac) {
  cBar.style.transform = `scaleX(${Math.max(0, Math.min(1, frac))})`;
}

// Progress shape is shared: {done,total} always; reclaim also sends {phase,label}.
function onCheckProgress(p) {
  cCount.textContent = p.total ? `${p.done}/${p.total}` : "";
  cStage.textContent =
    p.phase === "match"
      ? "Matching card files…"
      : p.label
      ? `Verifying ${p.label} in the cloud…`
      : "Verifying in the cloud…";
  setCBar(p.total ? p.done / p.total : 0);
}

function mkChannel(handler) {
  const Ch = window.__TAURI__?.core?.Channel;
  if (!Ch) return null;
  const ch = new Ch();
  ch.onmessage = handler;
  return ch;
}

// Wait for the user's choice: "go" (danger confirm), "alt" (the subdued
// secondary action, e.g. clear-offline), or "cancel" (any dismissal). `offerAlt`
// shows/hides the secondary button for this wait.
function awaitChoice(offerAlt) {
  cAlt.hidden = !offerAlt;
  return new Promise((resolve) => {
    const settle = (v) => {
      cGo.removeEventListener("click", onGo);
      cAlt.removeEventListener("click", onAlt);
      cCancel.removeEventListener("click", onNo);
      cdlg.removeEventListener("close", onNo);
      cAlt.hidden = true;
      resolve(v);
    };
    const onGo = () => settle("go");
    const onAlt = () => settle("alt");
    const onNo = () => settle("cancel");
    cGo.addEventListener("click", onGo);
    cAlt.addEventListener("click", onAlt);
    cCancel.addEventListener("click", onNo);
    cdlg.addEventListener("close", onNo);
  });
}

function showConfirmError(e) {
  cCheck.hidden = true;
  cSummary.hidden = true;
  cErr.textContent = String(e);
  cErr.hidden = false;
  cGo.disabled = true;
  cCancel.disabled = false;
}

// Run a verify → confirm → commit flow. `plan(channel)` / `commit(channel)` are
// thunks invoking the matching Tauri commands; `summarize` turns the plan into
// the confirm copy, `done` turns the result into a toast line.
async function runDestructive({ title, meta, confirmLabel, plan, summarize, commit, done, alt }) {
  cTitle.textContent = title;
  cMeta.textContent = meta;
  cGo.textContent = confirmLabel;
  cAlt.textContent = alt ? alt.label : "";
  cdlg.showModal();

  // The active plan can switch from the primary flow to the `alt` (offline)
  // flow if the user picks the secondary button (e.g. the cloud is unreachable).
  let curPlan = plan,
    curSummarize = summarize,
    offerAlt = !!alt;

  while (true) {
    // (re)enter the check state
    cCheck.hidden = false;
    cStage.textContent = "Checking…";
    cCount.textContent = "";
    setCBar(0);
    cSummary.hidden = true;
    cErr.hidden = true;
    cAlt.hidden = true;
    cGo.disabled = true;
    cCancel.disabled = false;
    confirmBusy = false;

    // 1) plan — the live cloud check (skipped by the offline plan)
    let planned;
    try {
      planned = await curPlan(mkChannel(onCheckProgress));
    } catch (e) {
      if (!cdlg.open) return; // dismissed mid-check
      showConfirmError(e); // leaves cGo disabled
      // Offer the offline fallback as the way forward, else just wait to dismiss.
      const choice = await awaitChoice(offerAlt);
      if (choice === "alt") {
        ({ plan: curPlan, summarize: curSummarize } = adoptAlt(alt));
        offerAlt = false;
        continue;
      }
      return;
    }
    if (!cdlg.open) return;

    // 2) show the plan; wait for confirm / offline-fallback / dismiss
    cCheck.hidden = true;
    cSummary.innerHTML = curSummarize(planned);
    cSummary.hidden = false;
    cGo.disabled = false;
    const choice = await awaitChoice(offerAlt);
    if (choice === "alt") {
      ({ plan: curPlan, summarize: curSummarize } = adoptAlt(alt));
      offerAlt = false;
      continue;
    }
    if (choice !== "go") return;

    // 3) commit
    confirmBusy = true;
    cGo.disabled = true;
    cAlt.hidden = true;
    cCancel.disabled = true;
    cGo.textContent = "Working…";
    cSummary.hidden = true;
    cErr.hidden = true;
    cCheck.hidden = false;
    cStage.textContent = "Working…";
    cCount.textContent = "";
    setCBar(0);
    try {
      const res = await commit(mkChannel(onCheckProgress));
      confirmBusy = false;
      cdlg.close();
      toast(done(res));
      await load();
    } catch (e) {
      confirmBusy = false;
      cCancel.disabled = false;
      showConfirmError(e);
    }
    return;
  }
}

// Switch the dialog over to an alt (offline) flow: retitle, relabel the confirm,
// and hand back its plan/summarize for the loop to re-run.
function adoptAlt(alt) {
  if (alt.title) cTitle.textContent = alt.title;
  cGo.textContent = alt.confirmLabel;
  return { plan: alt.plan, summarize: alt.summarize };
}

cCancel.addEventListener("click", () => {
  if (!confirmBusy) cdlg.close();
});
cdlg.addEventListener("cancel", (e) => {
  if (confirmBusy) e.preventDefault();
});
cdlg.addEventListener("click", (e) => {
  if (e.target === cdlg && !confirmBusy) cdlg.close();
});

// ---- card reclaim (wipe) ----
function reclaimSummary(plan) {
  const where = plan.trips.length ? ` from ${esc(plan.trips.join(", "))}` : "";
  const left = [];
  if (plan.notImported) left.push(`${plan.notImported} not imported`);
  if (plan.notVerified) left.push(`${plan.notVerified} not verified`);
  const tail = left.length ? ` <span class="left">${left.join(", ")} — left on the card.</span>` : "";
  return `<span class="free">${plural(plan.files.length, "clip")} · ${fmtBytes(
    plan.bytes
  )}</span> verified in the cloud${where} and safe to delete.${tail}`;
}

// Offline reclaim: no cloud check, so the summary is a warning — the footage will
// rest on a single local copy in the library, with no cloud backup.
function reclaimOfflineSummary(plan) {
  const where = plan.trips.length ? ` in ${esc(plan.trips.join(", "))}` : "";
  const left = [];
  if (plan.notImported) left.push(`${plan.notImported} not imported`);
  if (plan.notVerified) left.push(`${plan.notVerified} missing locally`);
  const tail = left.length ? ` <span class="left">${left.join(", ")} — left on the card.</span>` : "";
  return `<span class="free">${plural(plan.files.length, "clip")} · ${fmtBytes(plan.bytes)}</span> imported${where} — <strong>not verified in the cloud.</strong> Deleting leaves a single copy in your library, no cloud backup.${tail}`;
}

function openWipe({ window: win, label, offline = false }) {
  let files = [];
  const capture = (p) => ((files = p.files), p);
  const planWith = (off) => (channel) =>
    invoke("plan_reclaim", {
      channel,
      start: win ? win[0] : null,
      end: win ? win[1] : null,
      offline: off,
    }).then(capture);

  return runDestructive({
    title: offline ? "Clear card offline" : "Reclaim card",
    meta: offline
      ? `Clearing ${label} without a cloud check — for when you're offline.`
      : `Clearing ${label}. Confirming each clip is in the cloud before anything is deleted.`,
    confirmLabel: offline ? "Delete from card (offline)" : "Delete from card",
    plan: planWith(offline),
    summarize: offline ? reclaimOfflineSummary : reclaimSummary,
    commit: () => invoke("commit_reclaim", { files }),
    done: (res) => `Reclaimed ${plural(res.deleted, "clip")} · ${fmtBytes(res.bytes)} from the card`,
    // Starting online, offer a fallback to clear without the cloud check — the
    // escape hatch when the cloud is unreachable (its own confirm + warning).
    alt: offline
      ? null
      : {
          label: "Clear without cloud check",
          title: "Clear card offline",
          confirmLabel: "Delete from card (offline)",
          plan: planWith(true),
          summarize: reclaimOfflineSummary,
        },
  });
}

// ---- trip archive (free local raw, keep clips) ----
function archiveSummary(plan) {
  return `<span class="free">${plural(plan.masters, "clip")} · ${fmtBytes(
    plan.bytes
  )}</span> of raw is safe in the cloud. <span class="left">Freeing keeps your cut clips and marks — re-pull the raw anytime.</span>`;
}

function openArchive(t) {
  return runDestructive({
    title: `Archive ${t.name}`,
    meta: `Freeing ${t.name}'s local raw. Confirming all of it is in the cloud first.`,
    confirmLabel: "Free local raw",
    plan: (channel) => invoke("plan_archive", { channel, trip: t.name }),
    summarize: archiveSummary,
    commit: (channel) => invoke("commit_archive", { channel, trip: t.name }),
    done: (res) => `Archived ${res.trip} — freed ${fmtBytes(res.freed)} (clips kept)`,
  });
}

// ---- load ----
// Placeholder cards while the (now off-thread) scan runs, so the first paint
// isn't a blank page. Only shown when nothing's rendered yet — a rescan keeps its
// existing cards (the Rescan button is its own indicator).
function renderTripsLoading() {
  const wrap = $("#trips");
  wrap.innerHTML = "";
  const n = Math.min(Math.max(lastTrips.length || 4, 3), 8);
  for (let i = 0; i < n; i++) {
    const card = el("div", "trip skeleton");
    const body = el("div", "skel-body");
    body.append(el("div", "skel-line w70"), el("div", "skel-line w40"), el("div", "skel-line w85"));
    card.append(el("div", "cover-wrap"), body);
    wrap.append(card);
  }
}

async function load() {
  thumbQueue = []; // drop any pending thumbs from a previous scan
  if (!$("#trips").querySelector(".trip")) {
    renderTripsLoading();
    $("#summary").textContent = "Loading…";
  }
  try {
    const [card, trips] = await Promise.all([invoke("scan_card"), invoke("list_trips")]);
    lastTrips = trips;
    lastCard = card;
    renderCard(card);
    renderTrips(trips);
    // if the organize board is open (e.g. a lane's ⋯ menu just renamed/merged/
    // deleted a trip), rebuild its lanes off the fresh trip list too
    if (ORG.open) loadLanes();
    const bits = [plural(trips.length, "trip")];
    if (card) bits.push(`card: ${plural(card.sessions.length, "session")}`);
    $("#summary").textContent = bits.join("  ·  ");
    warmShareChips(trips); // background: light up "🔗 Shared · N" chips (once/session)
  } catch (e) {
    $("#card-panel").innerHTML = "";
    $("#trips").innerHTML = "";
    $("#trips").append(el("div", "empty-note", `Couldn't load: ${e}`));
  }
}

// Background: warm the network-free share cache so "🔗 Shared · N" chips appear
// on cards without opening each trip's panel first. Runs at most once per session
// and only when per-trip sharing is configured; if Nextcloud can't be reached it
// bails without marking done, so launching offline just retries on the next load.
let sharesWarmed = false;
let warmingShares = false;
async function warmShareChips(trips) {
  if (sharesWarmed || warmingShares) return;
  warmingShares = true;
  try {
    try {
      await invoke("sharing_status"); // config-only (no network); gates the sweep
    } catch {
      sharesWarmed = true; // sharing isn't available here at all — stop trying
      return;
    }
    let reached = false;
    for (const t of trips) {
      // skip trips with nothing in (or bound for) the cloud — they can't be shared
      if (!(t.masters > 0 || (t.sync && t.sync.cloudOnly > 0))) continue;
      let shares;
      try {
        shares = await invoke("trip_shares", { trip: t.name });
        reached = true;
      } catch (e) {
        // transport failure before any success → offline; retry next load
        if (!reached && String(e).includes("couldn't reach")) return;
        continue; // this trip just isn't pushed/shared — no chip
      }
      updateShareChip(t, shares.length);
    }
    sharesWarmed = true;
  } finally {
    warmingShares = false;
  }
}

$("#refresh").addEventListener("click", () => {
  const b = $("#refresh");
  b.disabled = true;
  b.innerHTML = '<span class="btn-ico" aria-hidden="true">↻</span> Rescanning…';
  load().finally(() => {
    b.disabled = false;
    b.innerHTML = '<span class="btn-ico" aria-hidden="true">↻</span> Rescan';
  });
});

// ============================ review / player ============================
// A full-screen cutting room: skim a trip's proxies, set in/out ranges and
// highlights, label and delete them. Marks key on the MASTER (proxies share its
// timeline) and persist to marks.tsv exactly as `reel cut` expects.

// `h` is pressed *after* something good happens — the moment is behind the
// playhead, not ahead of it — so the window leans on what you just watched.
// (Was 2s back / 8s on, inherited from the script, which grabbed mostly footage
// you hadn't seen yet.)
const HL_PRE = 8,
  HL_POST = 2;
const ICON_PLAY = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5v14l11-7z"/></svg>';
const ICON_PAUSE =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1"/><rect x="14" y="5" width="4" height="14" rx="1"/></svg>';

const video = $("#video");
const photoEl = $("#photo");

// player state
const P = {
  open: false,
  preview: false, // read-only card preview: no marks/move/delete, proxies off-trip
  trip: null,
  clips: [],
  marks: [], // every mark in the trip, file order
  i: 0, // current clip index
  selected: null, // selected mark index, or null
  pendingIn: null, // a started mark awaiting its end
  scrubbing: false,
  seekAfterLoad: null, // seek target applied once the next clip's metadata loads
  srcFor: null, // master path whose source <video> currently holds (see videoReady)
  saveT: null,
  clipBase: null, // loopback server base URL, fetched once (see clipUrl)
  loading: false, // a clip is loading/buffering (drives the stage spinner)
  loadT: null, // delay timer before the loading spinner appears
  loadTimeout: null, // backstop timer: give up on a stuck load
  preparing: false, // a proxy is being built (the "Preparing…" overlay owns the stage)
  zoneStart: null, // Ctrl+Space plays this segment [start,end]; loop repeats it
  zoneEnd: null,
  zoneLoop: false,
  zoneMark: null, // index into P.marks, so a running loop can be retrimmed by i/o
  zoneFree: false, // wrap paused: seeked out of the loop, or parked on an edge
  zoneEdge: null, // 0/1 while parked on that edge of the mark, else null
};

function clipUrl(path) {
  // Loopback HTTP server (clipBase = http://127.0.0.1:PORT). WebKitGTK's media
  // backend can't play a custom URI scheme (WebKit bug 146351), so <video> must
  // point at real http; the Rust side decodes the path and serves byte ranges.
  return `${P.clipBase}/${encodeURIComponent(path)}`;
}
function fmtTime(sec) {
  sec = Math.max(0, Math.floor(sec || 0));
  const h = Math.floor(sec / 3600),
    m = Math.floor((sec % 3600) / 60),
    s = sec % 60;
  return h ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}
const curClip = () => P.clips[P.i];

// ---- open / close ----
// Drop keyboard focus before a full-screen view takes over.
//
// The dashboard stays in the DOM behind the player, so whatever you activated to
// get here — a trip cover, a card's session strip — is a `tabIndex = 0` element
// that still holds focus. Its own Enter/Space handler runs in the target phase,
// i.e. *before* the player's document-level one, so every Space press re-ran
// `openReview` and snapped playback back to the first clip. Clicking anywhere in
// the player moved focus off it and the symptom vanished, which is exactly what
// made it look random. Blur is synchronous, so it lands before the await below
// and a second press can't repeat the open either.
function parkFocus() {
  const a = document.activeElement;
  if (a && a !== document.body && typeof a.blur === "function") a.blur();
}

async function openReview(t) {
  if (P.open) return; // a stray Enter/Space on something still focused behind us
  parkFocus();
  let pl;
  try {
    pl = await invoke("review_playlist", { trip: t.name });
  } catch (e) {
    return toast(String(e));
  }
  if (!P.clipBase) {
    // Resolve the loopback server once; <video> can't stream without it.
    try {
      P.clipBase = await invoke("clip_base");
    } catch (e) {
      return toast("couldn't reach the clip server: " + String(e));
    }
  }
  Object.assign(P, {
    open: true,
    preview: false,
    trip: pl.trip,
    clips: pl.clips,
    marks: pl.marks || [],
    i: 0,
    selected: null,
    pendingIn: null,
    seekAfterLoad: null,
  });
  const player = $("#player");
  player.classList.remove("preview");
  player.style.setProperty("--tc", tripColor(t.name));
  $("#player-trip").textContent = t.name;
  player.hidden = false;
  document.body.classList.add("reviewing");
  setPlayIcon(false);
  renderMarks();
  renderFilmstrip();
  logf("info", `review opened: ${t.name}`, { clips: pl.clips.length, marks: P.marks.length });
  loadClip(firstPlayable(), null, 1);
}

// Open the player read-only on the inserted card, optionally scoped to one
// session's [start, end] window — a way to watch what's on a card before
// importing it. No marks, move, or delete; proxies build off-trip (into the
// cache), and clips stream straight off the card via the broadened clip scope.
async function openCardPreview(win, label) {
  if (P.open) return; // same focused-strip-behind-the-player trap as openReview
  parkFocus();
  let pl;
  try {
    pl = await invoke("card_playlist", { start: win ? win[0] : null, end: win ? win[1] : null });
  } catch (e) {
    return toast(String(e));
  }
  if (!pl.clips.length) return toast("Nothing to preview here.");
  if (!P.clipBase) {
    try {
      P.clipBase = await invoke("clip_base");
    } catch (e) {
      return toast("couldn't reach the clip server: " + String(e));
    }
  }
  Object.assign(P, {
    open: true,
    preview: true,
    trip: pl.trip, // "card" sentinel — never a real trip in preview
    clips: pl.clips,
    marks: [],
    i: 0,
    selected: null,
    pendingIn: null,
    seekAfterLoad: null,
  });
  const player = $("#player");
  player.classList.add("preview");
  player.style.setProperty("--tc", "#8a8f98"); // neutral — the card isn't a trip
  $("#player-trip").textContent = label || "Card preview";
  player.hidden = false;
  document.body.classList.add("reviewing");
  setPlayIcon(false);
  renderMarks();
  renderFilmstrip();
  loadClip(firstPlayable(), null, 1);
}

// Index of the next clip that can actually play from `from` in direction `dir`
// (+1/-1), skipping stubs (empty placeholder clips); -1 if there's none that way.
function nextPlayable(from, dir) {
  for (let i = from; i >= 0 && i < P.clips.length; i += dir) {
    if (!P.clips[i].stub) return i;
  }
  return -1;
}
function firstPlayable() {
  const i = nextPlayable(0, 1);
  return i < 0 ? 0 : i; // all stubs → land on 0 and show the empty note
}

function closeReview() {
  if (!P.open) return;
  if (P.saveT) {
    clearTimeout(P.saveT);
    saveMarks(); // flush any pending label edit
  }
  P.open = false;
  P.preview = false;
  clearTimeout(P.loadT);
  clearTimeout(P.loadTimeout);
  P.loading = false;
  P.preparing = false;
  stopShuttle();
  clearZone();
  resetZoom();
  video.pause();
  P.srcFor = null;
  video.removeAttribute("src");
  video.load();
  hidePhoto();
  const player = $("#player");
  player.hidden = true;
  player.classList.remove("preview", "photo");
  document.body.classList.remove("reviewing");
  load(); // dashboard mark counts may have changed
}

// ---- clip loading ----
// `dir` is the navigation direction (+1/-1) when this load came from stepping or
// auto-advance; if the clip turns out unplayable we keep going that way. 0 means
// an explicit pick (a filmstrip click), where we instead show why it won't play.
async function loadClip(i, seekTo = null, dir = 0) {
  if (i < 0 || i >= P.clips.length) return;
  P.i = i;
  resetZoom(); // every clip starts fit-to-stage
  P.pendingIn = null;
  hidePending();
  armPending(false);
  const c = curClip();
  // Disown the outgoing source *now*. `playSrc` won't attach this clip's until
  // after the health probe (and possibly a proxy build), and until then the
  // element still holds the previous clip — see videoReady.
  P.srcFor = null;
  video.pause(); // stop the outgoing clip while we probe/load the next
  stopShuttle();
  clearZone();
  updateHead();
  updateProxyTag();
  updateWho();
  updateFilmstripActive();
  renderScrubMarks();
  updateTime();

  // A still photo (a picture, or a stitched panorama): show it as an image, with
  // no proxy, scrubbing, marks, or health probe. `photo` class hides the video
  // transport/edit chrome (see style.css).
  $("#player").classList.toggle("photo", !!c.photo);
  if (c.photo) {
    showPhoto(c, i);
    return;
  }
  hidePhoto();
  video.poster = "";
  setPoster(c, i);
  hideStageOverlay();

  // Already known unplayable — a size stub, or flagged by an earlier probe.
  if (c.stub) {
    stopLoading();
    video.removeAttribute("src");
    video.load();
    showStageNote(c.skipReason || `Empty clip — “${c.name}” has no video.`);
    return;
  }

  // Probe once (fast ffprobe) to catch empty / too-brief / corrupt clips up
  // front, instead of sinking the load timeout into a clip that can't play.
  // Skipped in card preview: the probe would ffprobe a multi-GB master on slow
  // card media (the DJI clips are known-good and play via their tiny .LRF).
  if (!c.checked && !P.preview) {
    startLoading();
    const h = await invoke("clip_health", { path: c.master }).catch(() => null);
    if (curClip() !== c) return; // moved on during the probe
    c.checked = true;
    if (h && !h.ok) {
      c.stub = true;
      c.skipTag = h.tag;
      c.skipReason = h.reason;
      markSkipCell(i);
      // stepping/auto-advance skips past it; an explicit pick shows why
      if (dir) {
        const nxt = nextPlayable(i + dir, dir);
        if (nxt >= 0) return loadClip(nxt, null, dir);
      }
      showStageNote(h.reason);
      return;
    }
  }

  if (c.showMaster) {
    playSrc(c.master, seekTo); // you asked for the real picture on this clip
  } else if (c.proxied) {
    playSrc(c.play, seekTo); // a clean cached proxy — load it directly
  } else if (c.hasProxy) {
    prepareThenPlay(c, seekTo); // native LRF/LRV present → fast remux, then play
  } else {
    playSrc(c.play, seekTo); // no fast source: try the master; onerror builds one
  }
}

// Show a photo on the stage instead of the video. Detaches the video so its
// buffering/timers don't run behind the image, and lets the loopback server hand
// over the bytes (the same path <video> uses; WebKit shows the image inline).
function showPhoto(c, i) {
  stopLoading();
  clearTimeout(P.loadT);
  clearTimeout(P.loadTimeout);
  P.loading = false;
  P.preparing = false;
  video.pause();
  video.removeAttribute("src");
  video.load();
  hideStageOverlay();
  photoEl.onerror = () => {
    if (P.open && P.i === i) showStageError(`“${c.name}” won't load.`);
  };
  photoEl.src = clipUrl(c.play);
  photoEl.hidden = false;
}
function hidePhoto() {
  photoEl.hidden = true;
  photoEl.removeAttribute("src");
}

function playSrc(path, seekTo) {
  P.seekAfterLoad = seekTo;
  P.srcFor = curClip()?.master ?? null; // the element now holds what we're showing
  video.src = clipUrl(path);
  video.load();
  startLoading();
}

function onMeta() {
  renderScrubMarks();
  if (P.seekAfterLoad != null) {
    video.currentTime = Math.min(P.seekAfterLoad, video.duration || 0);
    P.seekAfterLoad = null;
  }
  updateTime();
  video.play().catch(() => {}); // autoplay for a continuous skim; harmless if blocked
}

async function setPoster(c, i) {
  try {
    // Pull the frame from the small proxy (LRF/built proxy) when we have one —
    // a card master is many GB on slow media. Cached by the master's fileid.
    const uri = await invoke("thumb", { path: c.poster || c.master, fileid: c.fileid });
    if (uri && P.open && P.i === i) video.poster = uri;
  } catch {}
}

// A master that won't decode in the webview (HEVC, or no fast source) → build a
// clean proxy on the fly, then resume from where it stalled.
function onVideoError() {
  if (!P.open || !video.getAttribute("src")) return;
  clearTimeout(P.loadT);
  clearTimeout(P.loadTimeout);
  P.loading = false;
  const c = curClip();
  if (!c) return;
  if (c.showMaster && c.proxied) {
    // The master won't decode in the webview (usually HEVC) — which is the whole
    // reason the proxy exists. Drop back to it instead of calling the clip broken.
    c.showMaster = false;
    updateProxyTag();
    toast("This master won't decode here — back to the proxy.");
    playSrc(c.play, video.currentTime || null);
    return;
  }
  if (c.proxied || c._tried) {
    showStageError("This clip won't play, even as a proxy.");
    return;
  }
  c._tried = true;
  prepareThenPlay(c, video.currentTime || null);
}

// Build (remux or transcode) a clean cached proxy, then load it. Used eagerly
// when a native proxy exists, and as the fallback when a master won't decode.
async function prepareThenPlay(c, seekTo) {
  P.preparing = true;
  clearTimeout(P.loadT);
  clearTimeout(P.loadTimeout);
  P.loading = false;
  showStageOverlay(`Preparing ${c.name}…`, true);
  try {
    // In card preview there's no trip: build into the cache, keyed by content id.
    const play = P.preview
      ? await invoke("make_card_proxy", { master: c.master })
      : await invoke("make_proxy", { trip: P.trip, master: c.master });
    c.play = play;
    c.proxied = true;
    P.preparing = false;
    if (curClip() === c && !c.stub) {
      updateProxyTag();
      playSrc(play, seekTo); // built — load it (its own loader takes over)
    }
  } catch (e) {
    P.preparing = false;
    if (curClip() === c) showStageError(`Couldn't prepare this clip: ${e}`);
  }
}

function updateHead() {
  const c = curClip();
  $("#player-clip").textContent = `${P.i + 1} / ${P.clips.length} · ${c ? c.name : ""}`;
}
function updateProxyTag() {
  const c = curClip();
  const tag = $("#player-proxy");
  if (!c || c.photo) return (tag.hidden = true); // a photo has no proxy/master toggle
  tag.hidden = false;
  // The pill names what you're watching, and clicking always moves you to the
  // other one. It used to go inert the moment a proxy was in play, which read as a
  // dead button and left no way back to the real picture.
  const onMaster = !c.proxied || c.showMaster;
  tag.textContent = onMaster ? "master" : "proxy";
  tag.classList.toggle("is-proxy", !onMaster);
  tag.disabled = false;
  tag.title = !c.proxied
    ? "Playing the master — click to build a fast proxy if it won't scrub"
    : onMaster
      ? "Watching the master at full quality — click for the fast proxy"
      : "Scrubbing a fast proxy — click to watch the master at full quality";
}

// ---- transport ----
// True when <video> is holding the clip the rest of the UI is showing. Picking a
// clip and attaching its source are separated by an await — the ffprobe health
// check, or a proxy build that can run for minutes — and until that resolves the
// element still holds the *previous* clip, which is usually `ended`, because
// auto-advance is what got us here. `play()` on an ended element seeks back to
// the start (that's in the spec, not a WebKit quirk), so an unguarded press in
// that window restarts the clip you just left instead of pausing the one on
// screen — sometimes invisibly, behind the "Preparing…" overlay. Every control
// that touches the element checks this first.
const videoReady = () => P.srcFor != null && P.srcFor === curClip()?.master;

function togglePlay() {
  if (!videoReady()) return;
  // A reverse shuttle (J) holds the element *paused* and walks currentTime back on
  // a timer, so the picture moves while `video.paused` reads true. Space on a
  // moving picture means stop — without this it fell through to the play branch
  // and ran forward instead, and if the scrub had already hit the head it played
  // from 0, which looks exactly like the clip restarting.
  const rewinding = revTimer !== null;
  stopShuttle(); // Space always drops back to normal-rate playback
  if (rewinding) return;
  if (video.paused) video.play().catch(() => {});
  else video.pause();
}
function setPlayIcon(playing) {
  $("#play").innerHTML = playing ? ICON_PAUSE : ICON_PLAY;
}
function nudge(dt) {
  if (!videoReady() || !video.duration) return;
  stopShuttle();
  zoneSeek();
  video.currentTime = Math.max(0, Math.min(video.duration, video.currentTime + dt));
}
// Jump to the current clip's start (0) or end. `which`: 0 = Home, 1 = End.
function goToEnds(which) {
  if (!videoReady() || !video.duration) return;
  stopShuttle();
  zoneSeek();
  video.currentTime = which ? Math.max(0, video.duration - 0.05) : 0;
}

// ---- shuttle (J/K/L): the editor-standard transport ----
// L ramps forward through these speeds, J ramps in reverse, K stops. A native
// <video> can't play a negative rate, so reverse is a stepped scrub on a timer
// (no audio — like a real shuttle held at speed), matching Kdenlive's J/K/L.
const SHUTTLE = [1, 1.5, 2, 3, 5.5, 10];
let revTimer = null,
  revRate = 0;
// The speed readout over the picture. Shown only when the transport isn't at
// normal forward playback: any reverse scrub (the element is *paused* while the
// picture moves, so the play icon can't say it), and forward from 1.5× up. Plain
// 1× forward is what the play icon already means, so a "1×" pill sitting there
// during ordinary playback would be noise.
function renderShuttle() {
  const tag = $("#shuttle-tag");
  const rev = revTimer !== null;
  const rate = rev ? revRate : video.playbackRate;
  if (!rev && rate <= 1) {
    tag.classList.remove("on");
    tag.hidden = true;
    return;
  }
  tag.hidden = false;
  tag.innerHTML = "";
  tag.append(el("span", "sh-dir", rev ? "◂◂" : "▸▸"), el("span", "sh-rate", `${rate}×`));
  requestAnimationFrame(() => tag.classList.add("on"));
}
function stopReverse() {
  if (revTimer) clearInterval(revTimer);
  revTimer = null;
  revRate = 0;
  renderShuttle();
}
function stopShuttle() {
  stopReverse();
  // Only write when it actually changed: togglePlay calls this on every press, and
  // on WebKitGTK's GStreamer backend a rate write is a pipeline operation, not a
  // field assignment. Skipping the no-op write costs nothing either way.
  if (video.playbackRate !== 1) video.playbackRate = 1;
  renderShuttle();
}
function pauseShuttle() {
  stopShuttle();
  video.pause();
}
function shuttle(dir) {
  const c = curClip();
  if (!c || c.stub || !videoReady() || !video.duration) return;
  clearZone();
  if (dir > 0) {
    // forward: ramp playbackRate up one step (from a stop, start at 1×)
    stopReverse();
    const i = video.paused ? -1 : SHUTTLE.indexOf(video.playbackRate);
    video.playbackRate = SHUTTLE[Math.min(i + 1, SHUTTLE.length - 1)];
    video.play().catch(() => {});
  } else {
    // reverse: step currentTime back on a timer, ramping the rate one step
    video.pause();
    revRate = SHUTTLE[Math.min(SHUTTLE.indexOf(revRate) + 1, SHUTTLE.length - 1)];
    if (!revTimer) {
      revTimer = setInterval(() => {
        const t = (video.currentTime || 0) - revRate * 0.06;
        if (t <= 0) {
          video.currentTime = 0;
          stopReverse();
        } else {
          video.currentTime = t;
        }
      }, 60);
    }
  }
  renderShuttle();
}

// ---- stills (s): keep the frame you're looking at ----
// A still is a capture, not an export — it lands beside its source clip inside the
// trip and is thereafter an ordinary photo: filmstrip, cloud, dedup, archive. So it
// joins the strip right here rather than waiting for the next open, at the position
// the engine will independently sort it to (its mtime is the moment in the footage).
async function grabStill() {
  const c = curClip();
  if (!c || c.photo || c.stub || !videoReady() || !video.duration) return;
  const t = video.currentTime || 0;
  let s;
  try {
    s = await invoke("grab_still", { master: c.master, t });
  } catch (e) {
    return toast(String(e));
  }
  const at = P.clips.findIndex((x) => x.master === c.master);
  if (at >= 0 && !P.clips.some((x) => x.master === s.path)) {
    P.clips.splice(at + 1, 0, {
      master: s.path,
      play: s.path,
      poster: s.path,
      name: s.name,
      fileid: s.fileid,
      bytes: s.bytes,
      captured: (c.captured || 0) + Math.floor(t),
      proxied: false,
      hasProxy: false,
      stub: false,
      photo: true,
      person: c.person,
    });
    renderFilmstrip();
  }
  toast(`Still saved — ${s.name}`);
}

// ---- zone (Ctrl+Space play / Ctrl+Shift+Space loop): review one segment ----
// The "zone" is the selected mark, else a mark under the playhead on this clip,
// else the clip's last mark — mirroring Kdenlive's Play/Loop Zone on the in/out.
function currentZone() {
  if (P.selected != null && P.marks[P.selected]) return P.marks[P.selected];
  const c = curClip();
  if (!c) return null;
  const t = video.currentTime || 0;
  const here = P.marks.filter((m) => m.master === c.master);
  return here.find((m) => t >= m.start && t <= m.end) || here[here.length - 1] || null;
}
// A zone on another clip has to wait for that clip to become playable, so it arms
// a one-shot `canplay`. If the clip never gets there — a stub, a photo, a proxy
// build that fails — the listener would sit attached and fire on some *later*
// clip's `canplay`, which also fires again after any re-buffer. That yanked the
// playhead to a zone belonging to a clip you'd long since left. Only one arm can
// exist, and anything that abandons the zone drops it.
let zoneArm = null;
function disarmZone() {
  if (!zoneArm) return;
  video.removeEventListener("canplay", zoneArm);
  zoneArm = null;
}
function clearZone() {
  disarmZone();
  P.zoneStart = P.zoneEnd = null;
  P.zoneLoop = false;
  P.zoneMark = null;
  P.zoneFree = false;
  P.zoneEdge = null;
  renderZone();
}

// Seeking while a loop runs isn't "abandon the loop" — it's how you reach a point
// *outside* the current range in order to grow it. So the zone survives the seek
// and only the wrap is suspended, until i/o sets the new edge. Without this the
// playhead could never leave [start, end], so retrimming could only ever shrink a
// mark — never extend one.
function zoneSeek() {
  if (P.zoneEnd == null || !P.zoneLoop) return clearZone();
  P.zoneFree = true;
  P.zoneEdge = null;
  renderZone();
}

// The segment readout. A loop with no indicator is just a clip that mysteriously
// won't end, so it says what's repeating — and, since the point of watching a
// segment on repeat is to fix its edges, that i/o will retrim it.
function renderZone() {
  const tag = $("#zone-tag");
  if (P.zoneEnd == null) {
    tag.classList.remove("on");
    tag.hidden = true;
    return;
  }
  tag.hidden = false;
  tag.innerHTML = "";
  const m = P.marks[P.zoneMark];
  const parked = P.zoneEdge != null;
  tag.append(
    el("span", "zn-ico", parked ? "⏸" : P.zoneLoop ? "⟳" : "▸"),
    el("span", "zn-span tnum", `${fmtTime(P.zoneStart)} – ${fmtTime(P.zoneEnd)}`)
  );
  if (m?.label) tag.append(el("span", "zn-name", m.label));
  if (P.zoneMark != null) {
    tag.append(
      el(
        "span",
        "zn-hint",
        parked
          ? `${P.zoneEdge === 0 ? "start" : "end"} · ctrl shift space to loop`
          : "shift ←→ start · ctrl ←→ end"
      )
    );
  }
  requestAnimationFrame(() => tag.classList.add("on"));
}

// How far one trim keypress moves an edge. Coarse enough that a few taps reshape a
// highlight, fine enough to land a cut where you want it; `i`/`o` remain the way to
// jump an edge straight to the playhead.
const TRIM_STEP = 0.5;

// Walk one edge of the looping mark. `edge`: 0 = start, 1 = end. Parks the picture
// *paused on that edge* so you see the exact frame it lands on — the whole reason
// to nudge rather than set-at-playhead. Ctrl+Shift+Space picks the loop back up.
function nudgeEdge(edge, dir) {
  const m = P.marks[P.zoneMark];
  if (!m || !video.duration) return;
  const step = TRIM_STEP * dir;
  if (edge === 0) m.start = Math.max(0, Math.min(m.end - 0.1, m.start + step));
  else m.end = Math.min(video.duration, Math.max(m.start + 0.1, m.end + step));

  P.zoneStart = m.start;
  P.zoneEnd = m.end;
  // Suspend the wrap: parking on the end would otherwise trip the loop tick and
  // fling the playhead back to the start, so you'd never see the frame you set.
  P.zoneFree = true;
  P.zoneEdge = edge;
  P.selected = P.zoneMark; // so Ctrl+Shift+Space resumes *this* mark, not another
  video.pause();
  video.currentTime = edge === 0 ? m.start : m.end;
  renderMarks();
  renderScrubMarks();
  updateFilmstripActive();
  renderZone();
  scheduleSave();
}

// While a loop is running, i/o move *that* mark's edges instead of starting a new
// one. Watching a segment repeat is how you find the right in and out, so the keys
// that set them should apply to what you're watching.
function trimLoop(edge) {
  const m = P.marks[P.zoneMark];
  if (!m) return false;
  const t = video.currentTime || 0;
  if (edge === 0) {
    if (t >= m.end - 0.05) {
      toast("The start has to land before the end of the loop.");
      return true;
    }
    m.start = t;
  } else {
    if (t <= m.start + 0.05) {
      toast("The end has to land after the start of the loop.");
      return true;
    }
    m.end = t;
  }
  // The loop tick reads these, so it picks up the new edges on the next wrap —
  // moving the out-point under the playhead restarts it immediately, which is the
  // confirmation you want.
  P.zoneStart = m.start;
  P.zoneEnd = m.end;
  P.zoneFree = false;
  P.zoneEdge = null;
  P.selected = P.zoneMark;
  renderMarks();
  renderScrubMarks();
  updateFilmstripActive();
  renderZone();
  scheduleSave();
  // Play the new range back from the top — that's the confirmation you trimmed for.
  video.currentTime = m.start;
  video.play().catch(() => {});
  return true;
}
function playZone(loop) {
  const z = currentZone();
  if (!z) return toast("No segment to play — mark one with h, or i / o, then try again.");
  const start = () => {
    // Armed off a `canplay`, which also re-fires after a re-buffer — so confirm the
    // element really is holding the clip this zone belongs to before seeking it.
    if (!videoReady() || curClip()?.master !== z.master) return;
    stopShuttle();
    video.currentTime = z.start;
    P.zoneStart = z.start;
    P.zoneEnd = z.end;
    P.zoneLoop = loop;
    const mi = P.marks.indexOf(z);
    P.zoneMark = mi >= 0 ? mi : null;
    P.zoneFree = false;
    P.zoneEdge = null;
    renderZone();
    video.play().catch(() => {});
  };
  const c = curClip();
  if (!c || z.master !== c.master) {
    // the mark lives on another clip — load it, then start once it's ready
    const ci = P.clips.findIndex((x) => x.master === z.master);
    if (ci < 0) return;
    loadClip(ci, z.start); // clears the zone (and any stale arm) before we re-arm
    zoneArm = () => {
      disarmZone();
      start();
    };
    video.addEventListener("canplay", zoneArm);
  } else {
    start(); // guards itself
  }
}

// ---- mark navigation (Ctrl+←/→): hop between marks across the whole trip ----
// Kdenlive's next/previous-guide; reel's range marks are the guide analog. Marks
// are ordered by (clip, start) and we step relative to the current playhead.
function gotoAdjacentMark(dir) {
  if (!P.marks.length) return;
  const ordered = P.marks
    .map((m, i) => ({ i, start: m.start, ci: P.clips.findIndex((c) => c.master === m.master) }))
    .filter((x) => x.ci >= 0)
    .sort((a, b) => a.ci - b.ci || a.start - b.start);
  const ci = P.i,
    t = video.currentTime || 0;
  const target =
    dir > 0
      ? ordered.find((x) => x.ci > ci || (x.ci === ci && x.start > t + 0.15))
      : [...ordered].reverse().find((x) => x.ci < ci || (x.ci === ci && x.start < t - 0.15));
  if (target) goToMark(target.i);
}
// What the zone tick owes at time `t`. Pure, and separate, because inline it was
// wrong twice: once by wrapping a playhead that had deliberately been seeked out
// past the end (so a mark could only ever be shrunk), and once by *destroying* a
// one-shot zone the moment a ctrl+→ parked the playhead on its end — after which
// the next ctrl+→ found no zone and hopped to a different mark instead.
//
// `free` is the whole point: it means the playhead is out here on purpose —
// parked on an edge you're trimming, or hunting for a new one — and neither the
// wrap nor the stop should touch it. That applies to a one-shot zone exactly as
// much as to a loop.
function zoneAction(t, z) {
  if (z.zoneEnd == null || t < z.zoneEnd) return "none";
  if (z.zoneFree) return "none";
  return z.zoneLoop ? "wrap" : "stop";
}

function updateTime() {
  const d = video.duration || 0,
    t = video.currentTime || 0;
  // zone play/loop: stop (or loop back) when the playhead reaches the zone's end
  switch (zoneAction(t, P)) {
    case "wrap":
      video.currentTime = P.zoneStart;
      break;
    case "stop":
      video.pause();
      clearZone();
      break;
  }
  $("#time").textContent = `${fmtTime(t)} / ${fmtTime(d)}`;
  const f = d ? t / d : 0;
  $("#scrub-played").style.transform = `scaleX(${f})`;
  $("#scrub-head").style.left = `${f * 100}%`;
  if (P.pendingIn != null && d) {
    const a = Math.min(P.pendingIn, t),
      b = Math.max(P.pendingIn, t);
    const band = $("#scrub-pending");
    band.hidden = false;
    band.style.left = `${(a / d) * 100}%`;
    band.style.width = `${((b - a) / d) * 100}%`;
  }
}
function seekFromEvent(e) {
  const r = $("#scrub").getBoundingClientRect();
  const f = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width));
  if (!videoReady() || !video.duration) return;
  stopShuttle();
  zoneSeek();
  video.currentTime = f * video.duration;
}

// ---- marking ----
// A pending start arms the "o end" key hint (in the trip's colour) so the next
// step reads off the strip; cleared when the mark closes or undoes.
function armPending(on) {
  $("#hint-out")?.classList.toggle("armed", on);
}
function markIn() {
  if (!video.duration) return;
  if (P.zoneLoop && trimLoop(0)) return;
  P.pendingIn = video.currentTime;
  armPending(true);
  updateTime();
}
function markOut() {
  const c = curClip();
  if (!c) return;
  if (P.zoneLoop && trimLoop(1)) return;
  if (P.pendingIn == null) return toast("Nothing started yet — press i first, or h to keep the last few seconds.");
  let s = P.pendingIn,
    e = video.currentTime;
  if (e < s) [s, e] = [e, s];
  P.pendingIn = null;
  hidePending();
  armPending(false);
  addMark(c.master, s, e, "");
}
function highlight() {
  const c = curClip();
  if (!c || !video.duration) return;
  const t = video.currentTime;
  addMark(c.master, Math.max(0, t - HL_PRE), Math.min(video.duration, t + HL_POST), "hl");
}
function addMark(master, start, end, label) {
  P.marks.push({ master, start, end, label });
  P.selected = P.marks.length - 1;
  renderMarks();
  renderScrubMarks();
  updateFilmstripActive();
  scheduleSave();
}
function deleteMark(idx) {
  P.marks.splice(idx, 1);
  if (P.selected === idx) P.selected = null;
  else if (P.selected != null && P.selected > idx) P.selected--;
  renderMarks();
  renderScrubMarks();
  updateFilmstripActive();
  scheduleSave();
}
function undo() {
  if (P.pendingIn != null) {
    P.pendingIn = null;
    hidePending();
    armPending(false);
    return;
  }
  if (!P.marks.length) return;
  P.marks.pop();
  P.selected = null;
  renderMarks();
  renderScrubMarks();
  updateFilmstripActive();
  scheduleSave();
}
function labelLast() {
  showMarksPanel(true);
  const idx = P.selected != null ? P.selected : P.marks.length - 1;
  if (idx < 0) return toast("No segment to label yet.");
  P.selected = idx;
  renderMarks();
  const inp = document.querySelector(`.mark-row[data-idx="${idx}"] .mark-label`);
  if (inp) {
    inp.focus();
    inp.select();
  }
}
function goToMark(idx) {
  const m = P.marks[idx];
  if (!m) return;
  P.selected = idx;
  const ci = P.clips.findIndex((c) => c.master === m.master);
  if (ci >= 0 && ci !== P.i) {
    loadClip(ci, m.start); // loadClip already stops any shuttle/zone
  } else if (videoReady() && video.duration) {
    stopShuttle();
    clearZone();
    video.currentTime = Math.min(m.start, video.duration);
  }
  renderMarks();
  renderScrubMarks();
}

// ---- persistence (debounced; flushed on close) ----
function scheduleSave() {
  clearTimeout(P.saveT);
  P.saveT = setTimeout(saveMarks, 350);
}
async function saveMarks() {
  P.saveT = null;
  try {
    await invoke("save_marks", {
      trip: P.trip,
      marks: P.marks.map((m) => ({ master: m.master, start: m.start, end: m.end, label: m.label })),
    });
  } catch (e) {
    toast(`Couldn't save marks: ${e}`);
  }
}

// ---- rendering ----
function hidePending() {
  $("#scrub-pending").hidden = true;
}
function renderScrubMarks() {
  const wrap = $("#scrub-marks");
  wrap.innerHTML = "";
  const d = video.duration || 0;
  const c = curClip();
  if (!d || !c) return;
  P.marks.forEach((m, idx) => {
    if (m.master !== c.master) return;
    const band = el("div", `scrub-band${m.label === "hl" ? " hl" : ""}${idx === P.selected ? " sel" : ""}`);
    band.style.left = `${(m.start / d) * 100}%`;
    band.style.width = `${(Math.max(0.0001, m.end - m.start) / d) * 100}%`;
    band.title = `${fmtTime(m.start)}–${fmtTime(m.end)}${m.label && m.label !== "hl" ? " · " + m.label : ""}`;
    band.onclick = (e) => {
      e.stopPropagation();
      goToMark(idx);
    };
    wrap.append(band);
  });
}
function renderMarks() {
  const list = $("#marks-list");
  list.innerHTML = "";
  $("#marks-empty").hidden = P.marks.length > 0;
  $("#marks-sub").textContent = P.marks.length ? plural(P.marks.length, "segment") : "";
  // The count rides on the button that opens them — one control, not a readout
  // sitting beside a button that says the same thing.
  $("#player-list").textContent = P.marks.length ? `Marks · ${P.marks.length}` : "Marks";
  P.marks.forEach((m, idx) => {
    const ci = P.clips.findIndex((c) => c.master === m.master);
    const row = el("div", `mark-row${idx === P.selected ? " sel" : ""}`);
    row.dataset.idx = idx;
    row.style.setProperty("--mc", `${(m.end - m.start).toFixed(1)}s`);

    const meta = el("div", "mark-meta");
    meta.append(
      el("span", "mark-clip tnum", ci >= 0 ? String(ci + 1) : "?"),
      el("span", "mark-range tnum", `${fmtTime(m.start)}–${fmtTime(m.end)}`),
      el("span", "mark-dur tnum", `${(m.end - m.start).toFixed(1)}s`)
    );
    if (m.label === "hl") meta.append(el("span", "mark-star", "★"));

    const label = el("input", "mark-label");
    label.type = "text";
    label.value = m.label === "hl" ? "" : m.label;
    label.placeholder = "label";
    label.addEventListener("click", (e) => e.stopPropagation());
    label.addEventListener("keydown", (e) => {
      e.stopPropagation(); // marking keys must not fire while typing a label
      if (e.key === "Enter" || e.key === "Escape") {
        e.preventDefault();
        label.blur();
      }
    });
    label.addEventListener("change", () => {
      P.marks[idx].label = label.value.trim();
      scheduleSave();
    });

    const del = el("button", "mark-del", "✕");
    del.type = "button";
    del.title = "Delete segment";
    del.onclick = (e) => {
      e.stopPropagation();
      deleteMark(idx);
    };

    row.append(meta, label, del);
    row.onclick = () => goToMark(idx);
    list.append(row);
  });
}
// Short filmstrip tag for an unplayable clip: size stubs and no-video → "empty",
// sub-second blips → "brief", corrupt → "error".
function skipLabel(c) {
  return c.skipTag === "brief" ? "brief" : c.skipTag === "unreadable" ? "error" : "empty";
}
// Mark one filmstrip cell skippable in place (a probe just flagged it) without
// re-rendering the whole strip and re-requesting every thumbnail.
function markSkipCell(idx) {
  const cell = $(`#filmstrip .film[data-idx="${idx}"]`);
  if (!cell) return;
  cell.classList.add("stub");
  if (!cell.querySelector(".film-empty")) {
    cell.append(el("span", "film-empty", skipLabel(P.clips[idx])));
  }
}
function renderFilmstrip() {
  const strip = $("#filmstrip");
  strip.innerHTML = "";
  P.clips.forEach((c, idx) => {
    const cell = el("button", "film" + (c.stub ? " stub" : ""));
    cell.type = "button";
    cell.dataset.idx = idx;
    cell.title = c.stub ? `${c.name} — ${skipLabel(c)}` : c.name;
    const img = el("img", "film-thumb");
    img.alt = "";
    if (!c.stub) loadThumb(img, { path: c.poster || c.master, fileid: c.fileid });
    const badge = el("span", "film-marks tnum");
    badge.hidden = true;
    cell.append(img, el("span", "film-n tnum", String(idx + 1)), badge);
    if (c.stub) cell.append(el("span", "film-empty", skipLabel(c)));
    cell.onclick = () => loadClip(idx);
    strip.append(cell);
  });
  updateFilmstripActive();
}
function updateFilmstripActive() {
  const strip = $("#filmstrip");
  for (const cell of strip.children) {
    const idx = +cell.dataset.idx;
    cell.classList.toggle("active", idx === P.i);
    const n = P.marks.filter((m) => m.master === P.clips[idx].master).length;
    const badge = cell.querySelector(".film-marks");
    badge.hidden = n === 0;
    badge.textContent = String(n);
  }
  const act = strip.querySelector(".film.active");
  if (act) act.scrollIntoView({ inline: "center", block: "nearest" });
}

// ---- stage overlay (proxy building / errors) ----
function showStageOverlay(text, spinner) {
  const o = $("#stage-over");
  o.hidden = false;
  o.className = "stage-over";
  o.innerHTML = "";
  if (spinner) o.append(el("div", "spinner"));
  o.append(el("div", "stage-msg", text));
}
function showStageError(text) {
  logError(`stage: ${text}`, { clip: curClip()?.master ?? null });
  const o = $("#stage-over");
  o.hidden = false;
  o.className = "stage-over err";
  o.innerHTML = "";
  o.append(el("div", "stage-msg", text));
  const c = curClip();
  if (c) {
    const b = el("button", "btn small", "Build a proxy");
    b.type = "button";
    b.onclick = () => prepareThenPlay(c, null);
    o.append(b);
  }
}
function hideStageOverlay() {
  $("#stage-over").hidden = true;
}

// Seconds a stuck load waits before giving up — a quiet backstop. The probe in
// loadClip flags the common unplayable clips up front, so this rarely fires.
const LOAD_TIMEOUT = 15;

// A clip is loading/buffering. After a short delay (so a fast cached-proxy load
// doesn't flash a spinner), show a plain spinner; give up after LOAD_TIMEOUT.
// Cleared the moment the clip can play.
// True when the element is actually advancing (enough data buffered to play),
// as opposed to fetching or stalled. Lets the loader tell a real wait apart from
// a brief `waiting` blip fired mid-playback — the latter must not dim the screen.
function isPlaying() {
  return !video.paused && !video.ended && video.readyState > 2;
}

function startLoading(label = "Loading", delay = 180) {
  P.loading = true;
  P.preparing = false; // a direct load, not a proxy build
  clearTimeout(P.loadT);
  clearTimeout(P.loadTimeout);
  P.loadT = setTimeout(() => {
    // don't cover a clip that's actually playing (a mid-playback buffer blip)
    if (P.loading && P.open && !isPlaying()) showStageOverlay(`${label}…`, true);
  }, delay);
  P.loadTimeout = setTimeout(() => {
    if (!P.loading || !P.open || isPlaying()) return;
    P.loading = false;
    showStageError(`Couldn't load “${curClip()?.name || "this clip"}” — it may be corrupt or unsupported.`);
  }, LOAD_TIMEOUT * 1000);
}
function stopLoading() {
  P.loading = false;
  clearTimeout(P.loadT);
  clearTimeout(P.loadTimeout);
  P.loadT = null;
  // leave an error, or the "Preparing…" build overlay, in place
  if (!P.preparing && !$("#stage-over").classList.contains("err")) hideStageOverlay();
}

// A neutral stage message — no spinner, no retry button (e.g. an empty clip).
// Mirrors the engine's stub threshold (`review.rs`): a real camera master is
// always multiple MB, so anything under this with no streams is an unfinished file.
const STUB_BYTES = 512 * 1024;

// A clip that can't be shown. Says why, and — unlike `showStageError` — offers the
// only action that helps: there are no streams here to remux, so "Build a proxy"
// would be a dead end. For the classic few-KB stub the size *is* the explanation,
// so lead with it.
function showStageNote(text) {
  clearTimeout(P.loadT);
  clearTimeout(P.loadTimeout);
  P.loading = false;
  P.preparing = false;
  const o = $("#stage-over");
  o.hidden = false;
  o.className = "stage-over";
  o.innerHTML = "";
  o.append(el("div", "stage-msg", text));
  const c = curClip();
  if (!c) return;
  if (c.bytes > 0 && c.bytes < STUB_BYTES) {
    o.append(
      el(
        "div",
        "stage-sub",
        `${fmtBytes(c.bytes)} — the camera never finished writing this one. There's nothing to recover.`
      )
    );
  }
  // Card preview is read-only; a trip clip can be thrown away from here.
  if (!P.preview) {
    const b = el("button", "btn small danger", "Delete it");
    b.type = "button";
    b.onclick = () => deleteCurrentClip();
    o.append(b);
  }
}

// ---- marks panel toggle ----
function showMarksPanel(show) {
  $("#marks-panel").hidden = !show;
  $("#player-list").classList.toggle("on", show);
}
function toggleMarksPanel() {
  showMarksPanel($("#marks-panel").hidden);
}

// ---- wiring ----
$("#player-back").addEventListener("click", closeReview);
$("#player-list").addEventListener("click", toggleMarksPanel);

// ---- the keys panel (?) ----
// Holds everything the hint bar no longer shows, plus what a mark actually is.
const keysDlg = $("#keys-dialog");
function openKeys() {
  if (!keysDlg.open) keysDlg.showModal();
}
$("#keys-close").addEventListener("click", () => keysDlg.close());
keysDlg.addEventListener("click", (e) => {
  if (e.target === keysDlg) keysDlg.close();
});
$("#player-proxy").addEventListener("click", () => {
  const c = curClip();
  if (!c) return;
  const at = video.currentTime || null;
  if (!c.proxied) return prepareThenPlay(c, at); // nothing to switch to yet — build it
  c.showMaster = !c.showMaster;
  updateProxyTag();
  playSrc(c.showMaster ? c.master : c.play, at);
});
$("#play").addEventListener("click", togglePlay);

video.addEventListener("click", togglePlay);
video.addEventListener("loadedmetadata", onMeta);
video.addEventListener("timeupdate", () => {
  updateTime();
  // A timeupdate means the playhead actually moved, so we're progressing — even
  // during a fast shuttle or reverse scrub, where readyState routinely dips and
  // `isPlaying()` would read false. That's the reliable "not stuck" signal, so
  // tear down any buffering overlay a `waiting` may have raised.
  if (P.loading && (!video.paused || revTimer)) stopLoading();
});
video.addEventListener("play", () => setPlayIcon(true));
video.addEventListener("pause", () => setPlayIcon(false));
video.addEventListener("ended", () => {
  // A forward shuttle that runs off the end of the *last* clip has no loadClip to
  // reset it, so the rate (and its readout) would stick around on a stopped picture.
  stopShuttle();
  const next = nextPlayable(P.i + 1, 1);
  if (next >= 0) loadClip(next, null, 1);
});
video.addEventListener("error", onVideoError);
video.addEventListener("playing", stopLoading);
video.addEventListener("canplay", stopLoading);
video.addEventListener("waiting", () => {
  // A deliberate fast shuttle (rate > 1×, or a reverse scrub) can't keep the
  // buffer full — that's expected, not a stall, so don't dim the scan. A genuine
  // 1× buffer stall still shows "Buffering…" after a beat, cleared on progress.
  if (video.playbackRate > 1 || revTimer) return;
  startLoading("Buffering", 700);
});

const scrubEl = $("#scrub");
scrubEl.addEventListener("pointerdown", (e) => {
  P.scrubbing = true;
  scrubEl.setPointerCapture(e.pointerId);
  seekFromEvent(e);
});
scrubEl.addEventListener("pointermove", (e) => {
  if (P.scrubbing) seekFromEvent(e);
});
scrubEl.addEventListener("pointerup", (e) => {
  P.scrubbing = false;
  try {
    scrubEl.releasePointerCapture(e.pointerId);
  } catch {}
});

// ============================ zoom the clip, not the app ============================
// A touchpad pinch reaches the webview as ctrl+wheel (and as `gesture*` on WebKit).
// Left alone the webview zooms the entire UI — never what you want mid-review, and
// awkward to undo. So we swallow those globally and scale the clip on the stage
// instead. Applies to video and photo alike; a photo is the case that really wants
// it (checking faces in a panorama).
const ZOOM_MIN = 1;
const ZOOM_MAX = 8;
const stageEl = $("#stage");
const Z = { scale: 1, x: 0, y: 0 };

function applyZoom() {
  const t = `translate(${Z.x}px, ${Z.y}px) scale(${Z.scale})`;
  video.style.transform = t;
  photoEl.style.transform = t;
  stageEl.classList.toggle("zoomed", Z.scale > 1.001);
}
function resetZoom() {
  Z.scale = 1;
  Z.x = 0;
  Z.y = 0;
  applyZoom();
}
// Keep the clip from being dragged off the stage: at scale s it can travel at most
// half the overflow in each axis.
function clampPan() {
  const r = stageEl.getBoundingClientRect();
  const maxX = Math.max(0, (r.width * (Z.scale - 1)) / 2);
  const maxY = Math.max(0, (r.height * (Z.scale - 1)) / 2);
  Z.x = Math.min(maxX, Math.max(-maxX, Z.x));
  Z.y = Math.min(maxY, Math.max(-maxY, Z.y));
}
// Zoom about the cursor, so the pixel under the pointer stays put.
function zoomAt(cx, cy, factor) {
  const prev = Z.scale;
  const next = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, prev * factor));
  if (Math.abs(next - prev) < 0.0005) return;
  const r = stageEl.getBoundingClientRect();
  const ox = cx - r.left - r.width / 2;
  const oy = cy - r.top - r.height / 2;
  Z.x = ox - (ox - Z.x) * (next / prev);
  Z.y = oy - (oy - Z.y) * (next / prev);
  Z.scale = next;
  if (Z.scale <= ZOOM_MIN + 0.001) return resetZoom(); // snap back to centred
  clampPan();
  applyZoom();
}

// Never let the page itself zoom, wherever the pointer is.
document.addEventListener(
  "wheel",
  (e) => {
    if (e.ctrlKey) e.preventDefault();
  },
  { passive: false }
);

stageEl.addEventListener(
  "wheel",
  (e) => {
    if (!P.open) return;
    if (e.ctrlKey) {
      e.preventDefault();
      zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY / 300));
    } else if (Z.scale > 1.001) {
      e.preventDefault(); // once zoomed in, a plain two-finger scroll pans
      Z.x -= e.deltaX;
      Z.y -= e.deltaY;
      clampPan();
      applyZoom();
    }
  },
  { passive: false }
);

// WebKit's pinch gesture events, for good measure — harmless where they never fire.
let gestureBase = 1;
document.addEventListener("gesturestart", (e) => {
  e.preventDefault();
  gestureBase = Z.scale;
});
document.addEventListener("gesturechange", (e) => {
  e.preventDefault();
  if (!P.open) return;
  const r = stageEl.getBoundingClientRect();
  zoomAt(r.left + r.width / 2, r.top + r.height / 2, (gestureBase * e.scale) / Z.scale);
});
document.addEventListener("gestureend", (e) => e.preventDefault());

// On WebKitGTK a touchpad pinch is a GTK gesture the webview eats itself — no DOM
// event is ever dispatched, so the handlers above never see it. The Rust side
// watches the webview's zoom instead, pins it at 1 so the UI can't scale, and
// forwards the factor here (see `pin_page_zoom` in main.rs). The gesture carries no
// coordinates, so we anchor on wherever the pointer last was over the stage.
let lastPointer = null;
window.__TAURI__?.event?.listen?.("pinch-zoom", (e) => {
  if (!P.open) return;
  const f = Number(e?.payload);
  if (!Number.isFinite(f) || f <= 0) return;
  const r = stageEl.getBoundingClientRect();
  const p = lastPointer || { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  zoomAt(p.x, p.y, f);
});
stageEl.addEventListener("pointerleave", () => (lastPointer = null));

// Drag to pan while zoomed.
let panFrom = null;
let panMoved = false;
stageEl.addEventListener("pointerdown", (e) => {
  panMoved = false; // cleared even when not zoomed, so a stale pan can't eat a click
  if (Z.scale <= 1.001 || e.button !== 0) return;
  panFrom = { x: e.clientX, y: e.clientY, ox: Z.x, oy: Z.y };
  stageEl.classList.add("panning");
  stageEl.setPointerCapture(e.pointerId);
});
stageEl.addEventListener("pointermove", (e) => {
  lastPointer = { x: e.clientX, y: e.clientY };
  if (!panFrom) return;
  const dx = e.clientX - panFrom.x;
  const dy = e.clientY - panFrom.y;
  if (Math.hypot(dx, dy) > 3) panMoved = true;
  Z.x = panFrom.ox + dx;
  Z.y = panFrom.oy + dy;
  clampPan();
  applyZoom();
});
// The <video> toggles play on click, so a pan that ends over it would pause the
// clip. Swallow that click in the capture phase before it gets there.
stageEl.addEventListener(
  "click",
  (e) => {
    if (!panMoved) return;
    panMoved = false;
    e.stopPropagation();
    e.preventDefault();
  },
  true
);
const endPan = (e) => {
  if (!panFrom) return;
  panFrom = null;
  stageEl.classList.remove("panning");
  try {
    stageEl.releasePointerCapture(e.pointerId);
  } catch {}
};
stageEl.addEventListener("pointerup", endPan);
stageEl.addEventListener("pointercancel", endPan);
// Double-click anywhere on the stage returns to fit.
stageEl.addEventListener("dblclick", () => {
  if (Z.scale > 1.001) resetZoom();
});

document.addEventListener("keydown", (e) => {
  if (!P.open) return;
  // A modal (Move / Delete confirm / pick a trip) sits over the player, but its
  // keydowns still bubble to us. Without this the edit keys keep running behind
  // it — `i`/`o` silently add and auto-save marks, bare Delete drops the selected
  // mark, and `m` re-enters showModal() on an already-open dialog (throws). The
  // Organize handler guards the same way.
  if (document.querySelector("dialog[open]")) return;
  const typing = e.target && (e.target.tagName === "INPUT" || e.target.isContentEditable);
  if (e.key === "Escape") {
    if (typing) e.target.blur();
    else closeReview();
    return;
  }
  if (typing) return; // let label inputs type freely
  // Ctrl/Cmd combos (play/loop zone, hop between marks). Swallow any other
  // modified key so a stray Ctrl+letter can't fire a bare-key action.
  if (e.ctrlKey || e.metaKey) {
    if (e.key === " ") {
      e.preventDefault();
      playZone(e.shiftKey); // Ctrl+Space play zone, +Shift loop
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      const d = e.key === "ArrowRight" ? 1 : -1;
      // A live zone claims ctrl+arrows for its *end*. Hopping to the next mark
      // isn't what you're doing while one is up on screen being trimmed.
      if (P.zoneMark != null) nudgeEdge(1, d);
      else gotoAdjacentMark(d);
    }
    return;
  }
  // Card preview is read-only: transport keys still work, but the mark/edit
  // keys (start/end/highlight/undo/name/marks/move/delete/still) do nothing — a
  // card isn't a trip, so there's nowhere for a still to belong yet. `M` is
  // shift+m (move), which `e.key` reports uppercase.
  if (P.preview && "iohuexmsM".includes(e.key)) return;
  if (P.preview && (e.key === "Delete" || e.key === "Backspace")) {
    e.preventDefault();
    return;
  }
  // A photo has no timeline: the mark/scrub-edit keys do nothing (but shift+m and
  // Delete still work — a photo can be relocated or discarded like any capture).
  // `s` joins them: there's no frame to grab from a picture that already is one.
  // `m` too, since the Marks button is hidden on a photo (see style.css).
  if (curClip()?.photo && "iohuexsm".includes(e.key)) return;
  switch (e.key) {
    case " ":
      e.preventDefault();
      togglePlay();
      break;
    case "ArrowLeft":
    case "ArrowRight": {
      e.preventDefault();
      const d = e.key === "ArrowRight" ? 1 : -1;
      // With a zone up, shift+arrows walk its *start* (ctrl walks the end); without
      // one they stay the fine 1s seek. Plain arrows always seek.
      if (e.shiftKey && P.zoneMark != null) nudgeEdge(0, d);
      else nudge(d * (e.shiftKey ? 1 : 5));
      break;
    }
    case "j": // shuttle reverse (ramps −1×…−10×)
      shuttle(-1);
      break;
    case "k": // stop
      pauseShuttle();
      break;
    case "l": // shuttle forward (ramps 1×…10×)
      shuttle(1);
      break;
    case "Home":
      e.preventDefault();
      goToEnds(0);
      break;
    case "End":
      e.preventDefault();
      goToEnds(1);
      break;
    case "i":
      markIn();
      break;
    case "o":
      markOut();
      break;
    case "h":
      highlight();
      break;
    case "s":
      grabStill();
      break;
    case "?": // help is never read-only — works in card preview and on photos too
      e.preventDefault();
      openKeys();
      break;
    case "u":
      undo();
      break;
    case "e":
      e.preventDefault();
      labelLast();
      break;
    case "x":
      // sits with ⌫/del: same action, whichever hand is free
      e.preventDefault();
      if (P.selected != null) deleteMark(P.selected);
      break;
    case "m":
      toggleMarksPanel();
      break;
    case "M":
      // shift+m. Without preventDefault the keypress lands an "M" in the trip-pick
      // input the dialog focuses (same reason `e` above preventDefaults).
      e.preventDefault();
      moveCurrentClip();
      break;
    case "[": {
      const p = nextPlayable(P.i - 1, -1);
      if (p >= 0) loadClip(p, null, -1);
      break;
    }
    case "]": {
      const n = nextPlayable(P.i + 1, 1);
      if (n >= 0) loadClip(n, null, 1);
      break;
    }
    case "Delete":
    case "Backspace":
      e.preventDefault();
      if (e.shiftKey) deleteCurrentClip();
      else if (P.selected != null) deleteMark(P.selected);
      break;
  }
});

// ============================ organize / delete / pull ============================
// Move footage between trips, delete it for good, pull others' footage down. Each
// goes through a reel-core command that keeps the ledger, marks, cut, and cloud in
// step — the UI just gathers intent and shows the result.

// ---- reusable promise-based dialogs ----

// Pick or type a destination trip. Resolves the trip name, or null if dismissed.
const pickDlg = $("#pick-dialog");
const pickInput = $("#pick-input");
const pickPicks = $("#pick-picks");
const pickErr = $("#pick-error");
function paintPickTc(name) {
  pickDlg.style.setProperty("--tc", tripColor(name || "trip"));
  for (const b of pickPicks.children)
    b.setAttribute("aria-pressed", b.dataset.name === name ? "true" : "false");
}
function pickTrip({ title = "Move to trip", meta = "", confirmLabel = "Move", exclude = null }) {
  $("#pick-title").textContent = title;
  const m = $("#pick-meta");
  m.textContent = meta;
  m.hidden = !meta;
  $("#pick-go").textContent = confirmLabel;
  pickErr.hidden = true;
  pickPicks.innerHTML = "";
  const others = lastTrips.filter((t) => t.name !== exclude);
  for (const t of others) {
    const b = elHTML("button", "trip-pick", '<span class="pdot"></span>');
    b.append(t.name); // trip name as text, never markup
    b.type = "button";
    b.dataset.name = t.name;
    b.style.setProperty("--pc", tripColor(t.name));
    b.onclick = () => {
      pickInput.value = t.name;
      paintPickTc(t.name);
    };
    pickPicks.append(b);
  }
  pickPicks.hidden = others.length === 0;
  pickInput.value = "";
  paintPickTc("");
  pickDlg.showModal();
  pickInput.focus();
  return new Promise((resolve) => {
    const cleanup = () => {
      $("#pick-go").removeEventListener("click", onGo);
      $("#pick-cancel").removeEventListener("click", onNo);
      pickDlg.removeEventListener("close", onNo);
      pickInput.removeEventListener("keydown", onKey);
    };
    const finish = (v) => {
      cleanup();
      if (pickDlg.open) pickDlg.close();
      resolve(v);
    };
    const submit = () => {
      const name = pickInput.value.trim();
      if (!name) return ((pickErr.textContent = "Name a trip first."), (pickErr.hidden = false));
      if (/[\\/]/.test(name) || name === "." || name === "..")
        return ((pickErr.textContent = "That trip name isn't valid."), (pickErr.hidden = false));
      finish(name);
    };
    const onGo = submit;
    const onNo = () => finish(null);
    const onKey = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        submit();
      }
    };
    $("#pick-go").addEventListener("click", onGo);
    $("#pick-cancel").addEventListener("click", onNo);
    pickDlg.addEventListener("close", onNo);
    pickInput.addEventListener("keydown", onKey);
  });
}
pickInput.addEventListener("input", () => paintPickTc(pickInput.value.trim()));
pickDlg.addEventListener("click", (e) => {
  if (e.target === pickDlg) pickDlg.close();
});

// Ask for a single name (rename). Resolves the string, or null if dismissed.
const nameDlg = $("#name-dialog");
const nameInput = $("#name-input");
const nameErr = $("#name-error");
function askName({ title = "Rename", label = "New name", value = "", confirmLabel = "Rename" }) {
  $("#name-title").textContent = title;
  $("#name-label").textContent = label;
  $("#name-go").textContent = confirmLabel;
  nameErr.hidden = true;
  nameInput.value = value;
  nameDlg.showModal();
  nameInput.focus();
  nameInput.select();
  return new Promise((resolve) => {
    const cleanup = () => {
      $("#name-go").removeEventListener("click", onGo);
      $("#name-cancel").removeEventListener("click", onNo);
      nameDlg.removeEventListener("close", onNo);
      nameInput.removeEventListener("keydown", onKey);
    };
    const finish = (v) => {
      cleanup();
      if (nameDlg.open) nameDlg.close();
      resolve(v);
    };
    const submit = () => {
      const name = nameInput.value.trim();
      if (!name) return ((nameErr.textContent = "Enter a name."), (nameErr.hidden = false));
      if (/[\\/]/.test(name) || name === "." || name === "..")
        return ((nameErr.textContent = "That name isn't valid."), (nameErr.hidden = false));
      finish(name);
    };
    const onGo = submit;
    const onNo = () => finish(null);
    const onKey = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        submit();
      }
    };
    $("#name-go").addEventListener("click", onGo);
    $("#name-cancel").addEventListener("click", onNo);
    nameDlg.addEventListener("close", onNo);
    nameInput.addEventListener("keydown", onKey);
  });
}
nameDlg.addEventListener("click", (e) => {
  if (e.target === nameDlg) nameDlg.close();
});

// Confirm an irreversible action. Resolves true only on the explicit confirm.
// `body` is HTML (callers want <strong> emphasis) — run any name through `esc()`.
const dangerDlg = $("#danger-dialog");
function confirmDanger({ title = "Delete", body = "", confirmLabel = "Delete forever" }) {
  $("#danger-title").textContent = title;
  $("#danger-body").innerHTML = body;
  $("#danger-error").hidden = true;
  $("#danger-go").textContent = confirmLabel;
  dangerDlg.showModal();
  $("#danger-cancel").focus();
  return new Promise((resolve) => {
    const cleanup = () => {
      $("#danger-go").removeEventListener("click", onGo);
      $("#danger-cancel").removeEventListener("click", onNo);
      dangerDlg.removeEventListener("close", onNo);
    };
    const finish = (v) => {
      cleanup();
      if (dangerDlg.open) dangerDlg.close();
      resolve(v);
    };
    const onGo = () => finish(true);
    const onNo = () => finish(false);
    $("#danger-go").addEventListener("click", onGo);
    $("#danger-cancel").addEventListener("click", onNo);
    dangerDlg.addEventListener("close", onNo);
  });
}
dangerDlg.addEventListener("click", (e) => {
  if (e.target === dangerDlg) dangerDlg.close();
});

function deleteSummary(r) {
  const bits = [`Deleted ${plural(r.deleted, "clip")}`];
  if (r.bytes) bits[0] += ` · ${fmtBytes(r.bytes)}`;
  if (r.inCloud) bits.push(`${r.inCloud} from the cloud`);
  if (r.keptCloud) bits.push(`${r.keptCloud} kept in the cloud`);
  if (!r.cloudOk) bits.push("cloud cleanup owed — retry online");
  return bits.join(" · ");
}

// ---- trip ⋯ menu ----
let openMenuEl = null;
function closeMenu() {
  if (!openMenuEl) return;
  openMenuEl.remove();
  openMenuEl = null;
  document.removeEventListener("keydown", menuEsc, true);
  document.removeEventListener("click", closeMenu);
}
function menuEsc(e) {
  if (e.key === "Escape") {
    e.stopPropagation();
    closeMenu();
  }
}
function openTripMenu(anchor, t, opts = {}) {
  const items = [{ label: "Rename…", run: () => renameTrip(t) }];
  if (t.masters > 0) {
    // already inside the board — no point offering to open it again
    if (!opts.inBoard) items.push({ label: "Organize clips…", run: () => openOrganize(t.name) });
    items.push({ label: "Move all to…", run: () => mergeTripInto(t) });
  }
  items.push({ label: "Pull from cloud…", run: () => openPull(t) });
  items.push({ label: "Sharing…", run: () => openShare(t) });
  items.push({ sep: true });
  items.push({ label: "Delete trip…", danger: true, run: () => deleteTripConfirm(t) });
  openMenu(anchor, items);
}

/// A dropdown anchored under `anchor`. Items are `{label, run, danger}` or `{sep}`.
function openMenu(anchor, items) {
  closeMenu();
  const menu = el("div", "menu");
  for (const it of items) {
    if (it.sep) {
      menu.append(el("div", "menu-sep"));
      continue;
    }
    const b = el("button", `menu-item${it.danger ? " danger" : ""}`, it.label);
    b.type = "button";
    b.onclick = () => {
      closeMenu();
      it.run();
    };
    menu.append(b);
  }
  document.body.append(menu);
  const r = anchor.getBoundingClientRect();
  const left = Math.max(8, Math.min(r.right - menu.offsetWidth, window.innerWidth - menu.offsetWidth - 8));
  const top = Math.min(r.bottom + 6, window.innerHeight - menu.offsetHeight - 8);
  menu.style.left = `${left}px`;
  menu.style.top = `${top}px`;
  openMenuEl = menu;
  setTimeout(() => document.addEventListener("click", closeMenu), 0);
  document.addEventListener("keydown", menuEsc, true);
}

// The player's ⋯ — the same affordance a trip card uses, holding the two clip-level
// actions that used to sit permanently in the header as a button and a trash icon.
function openPlayerMenu(anchor) {
  if (!curClip()) return;
  openMenu(anchor, [
    { label: "Move to trip…", run: () => moveCurrentClip() },
    { sep: true },
    { label: "Delete clip…", danger: true, run: () => deleteCurrentClip() },
  ]);
}

async function renameTrip(t) {
  const name = await askName({ title: `Rename ${t.name}`, label: "New name", value: t.name, confirmLabel: "Rename" });
  if (!name || name === t.name) return;
  try {
    const r = await invoke("rename_trip", { old: t.name, new: name });
    toast(`Renamed to ${r.dest}${r.cloudSynced ? "" : " · cloud move queued — Sync to apply"}`);
    await load();
  } catch (e) {
    toast(String(e));
  }
}
async function mergeTripInto(t) {
  const dst = await pickTrip({
    title: `Move all of ${t.name}`,
    meta: `Fold its ${plural(t.masters, "clip")} into another trip — ${t.name} is then removed.`,
    confirmLabel: "Move all",
    exclude: t.name,
  });
  if (!dst) return;
  try {
    const r = await invoke("merge_trips", { src: t.name, dst });
    toast(`Moved ${plural(r.moved, "clip")} into ${dst}${r.skipped ? ` · ${r.skipped} already there` : ""}`);
    await load();
  } catch (e) {
    toast(String(e));
  }
}
async function deleteTripConfirm(t) {
  const cloud = t.mine > 0 && t.share === "shared" ? " Your footage is erased from the cloud too." : "";
  const ok = await confirmDanger({
    title: `Delete ${t.name}`,
    body: `Permanently delete <strong>${esc(t.name)}</strong> — ${plural(t.masters, "clip")}, its cut, and marks.${cloud} This can't be undone.`,
    confirmLabel: "Delete forever",
  });
  if (!ok) return;
  try {
    const r = await invoke("delete_trip", { trip: t.name });
    toast(deleteSummary(r));
    await load();
  } catch (e) {
    toast(String(e));
  }
}

// ---- clear discarded (trash you deleted, still on the card) ----
async function clearTrash(s) {
  const ok = await confirmDanger({
    title: "Clear trash from card",
    body: `Remove ${plural(s.discarded, "clip")} you already deleted from the card. They're gone for good already — this just frees the space.`,
    confirmLabel: "Clear from card",
  });
  if (!ok) return;
  try {
    const r = await invoke("clear_discarded", { start: s.start, end: s.end });
    toast(`Cleared ${plural(r.deleted, "clip")} · ${fmtBytes(r.bytes)} from the card`);
    await load();
  } catch (e) {
    toast(String(e));
  }
}

// ---- player: whose clip, move it, delete it ----
function updateWho() {
  const c = curClip();
  const who = $("#player-who");
  if (!c || !c.person) return (who.hidden = true);
  who.hidden = false;
  who.textContent = c.mine ? "you" : c.person;
  who.classList.toggle("mine", !!c.mine);
}
async function moveCurrentClip() {
  const c = curClip();
  if (!c) return;
  const dest = await pickTrip({ title: "Move clip", meta: `${c.name} → another trip`, confirmLabel: "Move", exclude: P.trip });
  if (!dest) return;
  try {
    const r = await invoke("move_clips", { masters: [c.master], dest });
    if (r.moved > 0) {
      toast(`Moved ${c.name} → ${dest}${r.cloudSynced ? "" : " · re-share needed"}`);
      dropClipFromPlayer(P.i);
    } else toast(r.skipped ? `Already in ${dest}` : "Nothing moved.");
  } catch (e) {
    toast(String(e));
  }
}
async function deleteCurrentClip() {
  const c = curClip();
  if (!c) return;
  const cloud = c.mine
    ? " It's erased from the cloud too."
    : " (Your local copy only — the cloud keeps its owner's copy.)";
  const ok = await confirmDanger({
    title: "Delete clip",
    body: `Permanently delete <strong>${esc(c.name)}</strong>.${cloud} This can't be undone.`,
    confirmLabel: "Delete forever",
  });
  if (!ok) return;
  try {
    const r = await invoke("delete_clips", { masters: [c.master] });
    toast(deleteSummary(r));
    dropClipFromPlayer(P.i);
  } catch (e) {
    toast(String(e));
  }
}
// After a clip moves or is deleted, drop it from the open player and keep the
// in-memory marks/filmstrip matching what the engine left on disk for this trip.
function dropClipFromPlayer(i) {
  const gone = P.clips[i];
  if (!gone) return;
  P.marks = P.marks.filter((m) => m.master !== gone.master);
  P.clips.splice(i, 1);
  P.selected = null;
  if (!P.clips.length) return closeReview();
  renderFilmstrip();
  renderMarks();
  loadClip(Math.min(i, P.clips.length - 1), null, 1);
}
$("#player-menu").addEventListener("click", (e) => {
  e.stopPropagation();
  openPlayerMenu($("#player-menu"));
});

// ============================ organize board ============================
// The whole library as lanes — one per trip, each holding that trip's clips.
// Drag clips across lanes to move them (or select and press m / use the toolbar),
// drop onto "+ New trip" to split some off, and a lane's ⋯ does trip-level ops.
// Clips are keyed by their master path (stable across moves), so selection and
// the keyboard cursor survive re-renders.
const ORG = {
  open: false,
  lanes: [], // [{ name, color, clips: [clip] }]
  sel: new Set(), // selected master paths, across all lanes
  flat: [], // [{ master, lane, idx }] in board order — the keyboard cursor's space
  byMaster: new Map(), // master -> { clip, lane }
  anchor: null, // { lane, idx } for shift-range within one lane
  focus: 0, // index into flat
  drag: null, // masters snapshot taken at dragstart
  cellByMaster: new Map(), // master -> cell element, for O(1) focus/selection paints
};
// The DOM node currently carrying the keyboard cursor, so moving focus touches
// just the outgoing and incoming cells instead of walking the whole board.
let orgFocusEl = null;

// Lazy thumbnails: a cell fetches its poster only once it scrolls near the board
// viewport — and because IntersectionObserver honours each strip's horizontal
// clipping too, clips scrolled off a row aren't fetched either. So thumbnail work
// (ffmpeg on a cache miss, a base64 read on a hit) scales with what's on screen,
// not with the whole library, which is what made a many-trip board lag.
const orgThumbRefs = new WeakMap();
const orgThumbObserver = new IntersectionObserver(
  (entries) => {
    for (const en of entries) {
      if (!en.isIntersecting) continue;
      const img = en.target;
      orgThumbObserver.unobserve(img);
      const ref = orgThumbRefs.get(img);
      if (ref) {
        orgThumbRefs.delete(img);
        loadThumb(img, ref);
      }
    }
  },
  { root: $("#org-board"), rootMargin: "250px 400px", threshold: 0 }
);
function lazyThumb(img, ref) {
  if (!ref || !ref.path) return;
  orgThumbRefs.set(img, ref);
  orgThumbObserver.observe(img);
}

// Open the board over the dashboard. `focusName` (a trip) scrolls its lane in.
async function openOrganize(focusName = null) {
  if (ORG.open) return; // same trap as openReview: the opener keeps focus behind us
  parkFocus();
  if (!lastTrips.length) {
    try {
      lastTrips = await invoke("list_trips");
    } catch (e) {
      return toast(String(e));
    }
  }
  if (!lastTrips.length) return toast("No trips to organize yet.");
  Object.assign(ORG, { open: true, sel: new Set(), anchor: null, focus: 0, drag: null });
  $("#organize").hidden = false;
  document.body.classList.add("organizing");
  await loadLanes(focusName);
}
function closeOrganize() {
  if (!ORG.open) return;
  ORG.open = false;
  $("#organize").hidden = true;
  document.body.classList.remove("organizing");
  load();
}
// (Re)fetch every trip's clips and rebuild the lanes off the current trip list.
// A trip with no local masters (e.g. archived) is still shown — an empty lane
// that stays a valid drop target.
async function loadLanes(focusName = null) {
  $("#org-sub").textContent = "Loading…";
  const trips = lastTrips;
  const clipsFor = await Promise.all(
    trips.map((t) =>
      t.masters > 0
        ? invoke("review_playlist", { trip: t.name })
            .then((pl) => pl.clips)
            .catch(() => [])
        : Promise.resolve([])
    )
  );
  ORG.lanes = trips.map((t, i) => ({
    name: t.name,
    color: tripColor(t.name),
    // chronological, so a strip reads left→right in time and day dividers are clean
    clips: clipsFor[i].slice().sort((a, b) => (a.captured || 0) - (b.captured || 0)),
  }));
  rebuildOrgIndex();
  renderOrgBoard();
  updateOrgTools();
  if (focusName) {
    for (const laneEl of $("#org-board").children)
      if (laneEl.dataset.trip === focusName) laneEl.scrollIntoView({ inline: "center", block: "nearest" });
  }
}
// Flatten lanes → the cursor space, and a master→clip index. Keeps focus in
// range and drops any selected masters that no longer exist (moved/deleted).
function rebuildOrgIndex() {
  ORG.flat = [];
  ORG.byMaster = new Map();
  ORG.lanes.forEach((lane, li) => {
    lane.clips.forEach((c, idx) => {
      ORG.flat.push({ master: c.master, lane: li, idx });
      ORG.byMaster.set(c.master, { clip: c, lane: li });
    });
  });
  if (ORG.focus >= ORG.flat.length) ORG.focus = Math.max(0, ORG.flat.length - 1);
  for (const m of [...ORG.sel]) if (!ORG.byMaster.has(m)) ORG.sel.delete(m);
}
function renderOrgBoard() {
  const board = $("#org-board");
  orgThumbObserver.disconnect(); // drop observations from the previous render
  ORG.cellByMaster = new Map();
  orgFocusEl = null;
  board.innerHTML = "";
  const total = ORG.flat.length;
  $("#org-sub").textContent = `${plural(ORG.lanes.length, "trip")} · ${plural(total, "clip")}`;
  ORG.lanes.forEach((lane, li) => board.append(mkLane(lane, li)));
  board.append(mkNewLane());
  paintOrgFocus();
}
function mkLane(lane, li) {
  const t = lastTrips.find((x) => x.name === lane.name);
  const laneEl = el("section", "org-lane");
  laneEl.dataset.trip = lane.name;
  laneEl.style.setProperty("--tc", lane.color);

  const head = el("header", "org-lane-head");
  const id = el("div", "org-lane-id");
  id.append(
    el("span", "org-lane-dot"),
    el("span", "org-lane-name", lane.name),
    el("span", "org-lane-count tnum", String(lane.clips.length))
  );
  // clicking the header (not the ⋯) grabs / releases the whole lane
  id.onclick = () => selectLane(li);
  id.title = "Select all clips in this trip";
  const menuBtn = el("button", "org-lane-menu", "⋯");
  menuBtn.type = "button";
  menuBtn.title = "Rename · move all · pull · delete";
  menuBtn.onclick = (e) => {
    e.stopPropagation();
    if (t) openTripMenu(menuBtn, t, { inBoard: true });
  };
  head.append(id, menuBtn);

  const clips = el("div", "org-lane-clips");
  if (!lane.clips.length) clips.append(el("div", "org-lane-empty", "empty"));
  else {
    let prevDay = null;
    lane.clips.forEach((c, idx) => {
      const day = c.captured ? isoDay(c.captured) : prevDay;
      // a subtle divider wherever the capture day changes within the strip
      if (idx > 0 && c.captured && day !== prevDay) clips.append(mkDaySep(c.captured));
      prevDay = day;
      clips.append(mkCell(c, li, idx));
    });
  }

  laneEl.append(head, clips);
  // the whole lane is a drop target — dropping moves the dragged selection here
  laneEl.ondragover = (e) => {
    e.preventDefault();
    laneEl.classList.add("over");
  };
  laneEl.ondragleave = (e) => {
    if (!laneEl.contains(e.relatedTarget)) laneEl.classList.remove("over");
  };
  laneEl.ondrop = (e) => {
    e.preventDefault();
    laneEl.classList.remove("over");
    orgMoveMasters([...(ORG.drag || ORG.sel)], lane.name);
  };
  return laneEl;
}
function mkCell(c, li, idx) {
  const cell = el("div", `org-cell${ORG.sel.has(c.master) ? " sel" : ""}${c.stub ? " stub" : ""}`);
  cell.dataset.master = c.master;
  cell.draggable = true;
  ORG.cellByMaster.set(c.master, cell);
  const img = el("img", "org-thumb");
  img.alt = "";
  if (!c.stub) lazyThumb(img, { path: c.master, fileid: c.fileid });
  cell.append(img);
  // provenance only when it isn't yours — "you" on every clip is just noise, so
  // an unbadged cell reads as yours and a badge singles out someone else's footage
  if (!c.mine) cell.append(el("span", "org-who", c.person));
  cell.append(el("span", "org-tick", "✓"), el("span", "org-name", c.name));
  cell.onclick = (e) => toggleCell(c.master, li, idx, e.shiftKey);
  cell.ondragstart = (e) => orgDragStart(e, c.master);
  cell.ondragend = () => document.body.classList.remove("org-dragging");
  return cell;
}
// A subtle vertical divider marking a change of capture day within a strip.
function mkDaySep(captured) {
  const sep = el("div", "org-day-sep");
  sep.append(el("span", null, fmtDay(new Date(captured * 1000))));
  return sep;
}
function mkNewLane() {
  const laneEl = el("div", "org-lane new", "+ New trip");
  laneEl.ondragover = (e) => {
    e.preventDefault();
    laneEl.classList.add("over");
  };
  laneEl.ondragleave = (e) => {
    if (!laneEl.contains(e.relatedTarget)) laneEl.classList.remove("over");
  };
  laneEl.ondrop = (e) => {
    e.preventDefault();
    laneEl.classList.remove("over");
    orgNewTrip([...(ORG.drag || ORG.sel)]);
  };
  laneEl.onclick = () => orgNewTrip([...ORG.sel]);
  return laneEl;
}
function flatIndexOf(master) {
  return ORG.flat.findIndex((f) => f.master === master);
}
function repaintOrgSel() {
  for (const [m, cell] of ORG.cellByMaster) cell.classList.toggle("sel", ORG.sel.has(m));
}
// The keyboard cursor: a focused cell distinct from selection, kept in view. Only
// the outgoing and incoming cells are touched, so arrow-key nav stays cheap no
// matter how many clips the board holds.
function paintOrgFocus() {
  const fm = ORG.flat[ORG.focus]?.master ?? null;
  if (orgFocusEl) orgFocusEl.classList.remove("focus");
  orgFocusEl = fm ? ORG.cellByMaster.get(fm) : null;
  if (orgFocusEl) {
    orgFocusEl.classList.add("focus");
    orgFocusEl.scrollIntoView({ block: "nearest", inline: "nearest" });
  }
}
// What move/delete act on: the selection, or the focused clip if nothing's picked.
function orgTargets() {
  if (ORG.sel.size) return [...ORG.sel];
  const f = ORG.flat[ORG.focus];
  return f ? [f.master] : [];
}
function toggleCell(master, li, idx, range) {
  ORG.focus = flatIndexOf(master);
  if (range && ORG.anchor && ORG.anchor.lane === li) {
    const [lo, hi] = [Math.min(ORG.anchor.idx, idx), Math.max(ORG.anchor.idx, idx)];
    for (let i = lo; i <= hi; i++) ORG.sel.add(ORG.lanes[li].clips[i].master);
  } else {
    ORG.sel.has(master) ? ORG.sel.delete(master) : ORG.sel.add(master);
    ORG.anchor = { lane: li, idx };
  }
  repaintOrgSel();
  paintOrgFocus();
  updateOrgTools();
}
// Header click: select the whole lane, or clear it if it's already all selected.
function selectLane(li) {
  const masters = ORG.lanes[li].clips.map((c) => c.master);
  if (!masters.length) return;
  const allSel = masters.every((m) => ORG.sel.has(m));
  for (const m of masters) allSel ? ORG.sel.delete(m) : ORG.sel.add(m);
  ORG.anchor = { lane: li, idx: 0 };
  repaintOrgSel();
  updateOrgTools();
}
function updateOrgTools() {
  const n = ORG.sel.size;
  $("#org-selcount").textContent = n ? `${n} selected` : "";
  $("#org-move").disabled = !n;
  $("#org-delete").disabled = !n;
}
function orgDragStart(e, master) {
  // Dragging an unselected clip selects just it — but toggle classes in place,
  // never re-render (replacing the dragged node mid-dragstart cancels the drag).
  if (!ORG.sel.has(master)) {
    ORG.sel = new Set([master]);
    ORG.anchor = null;
    ORG.focus = flatIndexOf(master);
    repaintOrgSel();
    paintOrgFocus();
    updateOrgTools();
  }
  ORG.drag = [...ORG.sel];
  e.dataTransfer.effectAllowed = "move";
  e.dataTransfer.setData("text/plain", master);
  document.body.classList.add("org-dragging");
}
// "+ New trip": name a fresh trip and move the picked clips into it. There's no
// empty-trip concept — a trip exists once footage lands in it — so this needs a
// selection, and it hands a concrete name to orgMoveMasters (skipping the picker).
async function orgNewTrip(masters) {
  ORG.drag = null;
  if (!masters.length) return toast("Select clips first, then make a new trip for them.");
  const name = await askName({ title: "New trip", label: "Trip name", value: "", confirmLabel: "Create & move" });
  if (!name) return;
  orgMoveMasters(masters, name);
}
async function orgMoveMasters(masters, dest) {
  ORG.drag = null;
  if (!masters.length) return;
  if (!dest) {
    dest = await pickTrip({ title: `Move ${plural(masters.length, "clip")}`, confirmLabel: "Move" });
    if (!dest) return;
  }
  try {
    const r = await invoke("move_clips", { masters, dest });
    toast(
      `Moved ${plural(r.moved, "clip")} → ${dest}${r.skipped ? ` · ${r.skipped} already there` : ""}${
        r.cloudSynced ? "" : " · re-share needed"
      }`
    );
    await refreshOrg();
  } catch (e) {
    toast(String(e));
  }
}
async function orgDeleteMasters(masters) {
  if (!masters.length) return;
  const clips = masters.map((m) => ORG.byMaster.get(m)?.clip).filter(Boolean);
  const mineN = clips.filter((c) => c.mine).length;
  const cloud = mineN ? ` ${plural(mineN, "clip")} of yours are erased from the cloud too.` : "";
  const ok = await confirmDanger({
    title: `Delete ${plural(masters.length, "clip")}`,
    body: `Permanently delete ${plural(masters.length, "clip")}.${cloud} This can't be undone.`,
    confirmLabel: "Delete forever",
  });
  if (!ok) return;
  try {
    const r = await invoke("delete_clips", { masters });
    toast(deleteSummary(r));
    await refreshOrg();
  } catch (e) {
    toast(String(e));
  }
}
// Rebuild the board from disk after a move/delete (dupes may have been skipped,
// trips may have appeared or emptied out).
async function refreshOrg() {
  ORG.sel = new Set();
  ORG.anchor = null;
  ORG.drag = null;
  try {
    lastTrips = await invoke("list_trips");
  } catch {}
  await loadLanes();
}
$("#org-back").addEventListener("click", closeOrganize);
$("#org-move").addEventListener("click", () => orgMoveMasters([...ORG.sel], null));
$("#org-delete").addEventListener("click", () => orgDeleteMasters([...ORG.sel]));
$("#organize-open").addEventListener("click", () => openOrganize());

// Keyboard on the board: ←/→ walk clips (across lanes), ↑/↓ jump a row within the
// focused lane, space toggles selection, shift+arrows extend it, ctrl+a selects
// all; m moves and ⌫/Del deletes the selection (or the focused clip if nothing's
// picked). All gated on the board being open with no dialog over it.
function orgKey(e) {
  if (!ORG.open || document.querySelector("dialog[open]")) return;
  const tgt = e.target;
  if (tgt && (tgt.tagName === "INPUT" || tgt.isContentEditable)) return;
  if (e.key === "Escape") {
    e.preventDefault();
    if (ORG.sel.size) {
      ORG.sel = new Set();
      repaintOrgSel();
      updateOrgTools();
    } else closeOrganize();
    return;
  }
  const n = ORG.flat.length;
  if (!n) return;
  // move the cursor to a flat index; shift adds every clip passed over
  const moveFocus = (to, extend) => {
    e.preventDefault();
    to = Math.max(0, Math.min(n - 1, to));
    if (extend) {
      const [lo, hi] = [Math.min(ORG.focus, to), Math.max(ORG.focus, to)];
      for (let i = lo; i <= hi; i++) ORG.sel.add(ORG.flat[i].master);
      repaintOrgSel();
      updateOrgTools();
    }
    ORG.focus = to;
    paintOrgFocus();
  };
  // ↑/↓ jump to the same spot in the previous/next non-empty trip (row)
  const vert = (dir, extend) => {
    e.preventDefault();
    const cur = ORG.flat[ORG.focus];
    for (let li = cur.lane + dir; li >= 0 && li < ORG.lanes.length; li += dir) {
      const row = ORG.lanes[li].clips;
      if (row.length) return moveFocus(flatIndexOf(row[Math.min(cur.idx, row.length - 1)].master), extend);
    }
  };
  switch (e.key) {
    case "ArrowRight":
      moveFocus(ORG.focus + 1, e.shiftKey);
      break;
    case "ArrowLeft":
      moveFocus(ORG.focus - 1, e.shiftKey);
      break;
    case "ArrowDown":
      vert(1, e.shiftKey);
      break;
    case "ArrowUp":
      vert(-1, e.shiftKey);
      break;
    case "Home":
      e.preventDefault();
      ORG.focus = 0;
      paintOrgFocus();
      break;
    case "End":
      e.preventDefault();
      ORG.focus = n - 1;
      paintOrgFocus();
      break;
    case " ": {
      e.preventDefault();
      const f = ORG.flat[ORG.focus];
      ORG.sel.has(f.master) ? ORG.sel.delete(f.master) : ORG.sel.add(f.master);
      ORG.anchor = { lane: f.lane, idx: f.idx };
      repaintOrgSel();
      updateOrgTools();
      break;
    }
    case "a":
      if (e.ctrlKey || e.metaKey) {
        e.preventDefault();
        ORG.sel = new Set(ORG.flat.map((f) => f.master));
        repaintOrgSel();
        updateOrgTools();
      }
      break;
    case "m":
      e.preventDefault();
      orgMoveMasters(orgTargets(), null);
      break;
    case "Delete":
    case "Backspace":
      e.preventDefault();
      orgDeleteMasters(orgTargets());
      break;
  }
}
document.addEventListener("keydown", orgKey);

// ============================ pull from cloud ============================
const pullDlg = $("#pull-dialog");
async function openPull(t) {
  $("#pull-title").textContent = `Pull into ${t.name}`;
  $("#pull-meta").textContent = "Checking the cloud…";
  $("#pull-list").innerHTML = "";
  $("#pull-error").hidden = true;
  pullDlg.showModal();
  let people;
  try {
    people = await invoke("cloud_contributors", { trip: t.name });
  } catch (e) {
    $("#pull-meta").textContent = "";
    $("#pull-error").textContent = String(e);
    $("#pull-error").hidden = false;
    return;
  }
  if (!pullDlg.open) return;
  if (!people.length) {
    $("#pull-meta").textContent = "No one else has shared footage to this trip yet.";
    return;
  }
  $("#pull-meta").textContent = "Bring others' footage into this trip.";
  const list = $("#pull-list");
  for (const p of people) list.append(pullRow(t, p));
}
function pullRow(t, p) {
  const row = el("div", "pull-row");
  const who = el("div", "pull-who");
  who.append(el("span", "pull-name", p.person), el("span", "pull-sub tnum", `${plural(p.clips, "clip")} · ${fmtBytes(p.bytes)}`));
  const act = el("div", "pull-act");
  if (p.pulled) act.append(chip({ cls: "safe", text: "✓ Pulled" }));
  else {
    const b = el("button", "btn small primary", "Pull →");
    b.type = "button";
    b.onclick = () => startPull(t, p, row, act);
    act.append(b);
  }
  row.append(who, act);
  return row;
}
async function startPull(t, p, row, act) {
  act.innerHTML = "";
  const pct = el("span", "pull-pct tnum", "0%");
  act.append(pct);
  const fill = el("div", "bar-fill");
  const bar = el("div", "bar pull-bar");
  bar.append(fill);
  row.append(bar);
  const setFill = (f) => (fill.style.transform = `scaleX(${Math.max(0, Math.min(1, f))})`);
  const Ch = window.__TAURI__?.core?.Channel;
  const channel = Ch ? new Ch() : null;
  if (channel)
    channel.onmessage = (pr) => {
      const f = pr.total ? pr.done / pr.total : 0;
      pct.textContent = `${Math.round(f * 100)}%`;
      setFill(f);
    };
  try {
    const r = await invoke("pull_person", { channel, trip: t.name, person: p.person });
    pct.textContent = "✓";
    setFill(1);
    toast(`Pulled ${plural(r.files, "clip")} from ${p.person} · ${fmtBytes(r.bytes)}`);
    await load();
  } catch (e) {
    $("#pull-error").textContent = String(e);
    $("#pull-error").hidden = false;
    bar.remove();
    act.innerHTML = "";
    const b = el("button", "btn small primary", "Retry");
    b.type = "button";
    b.onclick = () => startPull(t, p, row, act);
    act.append(b);
  }
}
$("#pull-close").addEventListener("click", () => pullDlg.close());
pullDlg.addEventListener("click", (e) => {
  if (e.target === pullDlg) pullDlg.close();
});

// ============================ share a trip with friends ============================
// Manage who a trip's cloud folder is shared with on Nextcloud — the OCS Share
// API, driven by reel-core (it reuses the rclone remote's credentials). A friend
// added here can see and contribute to just this trip, instead of the whole cloud.
// Availability is checked first: a non-Nextcloud remote (or missing curl) disables
// the panel with the reason, so this never fails halfway.
const shareDlg = $("#share-dialog");
let shareTrip = null;
let shareFriends = []; // people you already share with (union of caches) — dropdown seed
let shareLive = []; // last live sharee_search hits
let shareCurrent = []; // usernames already on THIS trip (excluded from the dropdown)
let shareActive = -1; // keyboard-highlighted item in the open menu

// "N people" (or "1 person") — the trip-sharing count doesn't pluralize with -s.
function peopleCount(n) {
  return `${n} ${n === 1 ? "person" : "people"}`;
}
function setShareMeta(n) {
  $("#share-meta").textContent = n
    ? `Shared with ${peopleCount(n)}.`
    : "Not shared with anyone yet — add a friend below.";
  // Reflect the count on the trip's card chip behind the dialog, live.
  if (shareTrip) updateShareChip(shareTrip, n);
}
function showShareError(e) {
  $("#share-error").textContent = String(e);
  $("#share-error").hidden = false;
}

async function openShare(t) {
  shareTrip = t;
  $("#share-title").textContent = `Share ${t.name}`;
  $("#share-meta").textContent = "Checking…";
  $("#share-list").innerHTML = "";
  $("#share-add").hidden = true;
  $("#share-error").hidden = true;
  $("#share-user").value = "";
  shareLive = [];
  shareCurrent = [];
  closeShareMenu();
  shareDlg.showModal();

  // Gate on whether sharing can run at all before offering any controls.
  try {
    await invoke("sharing_status");
  } catch (e) {
    if (shareDlg.open && shareTrip === t)
      $("#share-meta").textContent = `Per-trip sharing isn't available — ${e}`;
    return;
  }
  if (!shareDlg.open || shareTrip !== t) return;
  await refreshShares(t);
}

async function refreshShares(t) {
  let shares;
  try {
    shares = await invoke("trip_shares", { trip: t.name });
  } catch (e) {
    $("#share-meta").textContent = "";
    showShareError(e);
    return;
  }
  if (!shareDlg.open || shareTrip !== t) return;
  shareCurrent = shares.map((s) => s.user);
  $("#share-add").hidden = false;
  $("#share-list").innerHTML = "";
  setShareMeta(shares.length);
  for (const s of shares) $("#share-list").append(shareRow(s));
  loadShareFriends(); // seed the add-a-friend dropdown (best-effort, network-free)
}

// One shared-with person. Names go through textContent — a Nextcloud display
// name can hold HTML-special characters, and `el`'s text arg is innerHTML.
function shareRow(s) {
  const row = el("div", "pull-row");
  row.dataset.shareId = s.id;
  const who = el("div", "pull-who");
  const name = el("span", "pull-name");
  name.textContent = s.displayName || s.user;
  who.append(name);
  if (s.displayName && s.displayName !== s.user) {
    const sub = el("span", "pull-sub");
    sub.textContent = `@${s.user}`;
    who.append(sub);
  }
  const act = el("div", "pull-act");
  const b = el("button", "btn small ghost", "Remove");
  b.type = "button";
  b.onclick = () => removeShare(s, row, b);
  act.append(b);
  row.append(who, act);
  return row;
}

async function removeShare(s, row, btn) {
  btn.disabled = true;
  btn.textContent = "Removing…";
  $("#share-error").hidden = true;
  try {
    await invoke("share_remove", { trip: shareTrip.name, id: s.id });
    row.remove();
    shareCurrent = shareCurrent.filter((u) => u !== s.user);
    setShareMeta($("#share-list").children.length);
    toast(`Unshared from ${s.displayName || s.user}`);
  } catch (e) {
    btn.disabled = false;
    btn.textContent = "Remove";
    showShareError(e);
  }
}

function addShare() {
  addShareUser($("#share-user").value);
}

async function addShareUser(userRaw) {
  const t = shareTrip;
  const user = (userRaw || "").trim();
  if (!user) return;
  const go = $("#share-go");
  go.disabled = true;
  go.textContent = "Sharing…";
  $("#share-error").hidden = true;
  try {
    const s = await invoke("share_add", { trip: t.name, user });
    if (!shareDlg.open || shareTrip !== t) return;
    $("#share-user").value = "";
    $("#share-list").append(shareRow(s));
    shareCurrent.push(s.user);
    setShareMeta($("#share-list").children.length);
    toast(`Shared ${t.name} with ${s.displayName || s.user}`);
  } catch (e) {
    showShareError(e);
  } finally {
    go.disabled = false;
    go.textContent = "Share";
  }
}

// ---- add-a-friend combobox ----
// A dropdown of people you already share with (the union of the local share
// caches, network-free) plus live Nextcloud search as you type. Picking one
// shares the trip with them immediately; you can still type an exact username
// and hit Share for someone new.

async function loadShareFriends() {
  const t = shareTrip;
  let friends;
  try {
    friends = await invoke("share_friends");
  } catch {
    return; // no seed — the box still works as free text + live search
  }
  if (shareTrip !== t) return;
  shareFriends = friends;
  if (isShareMenuOpen()) renderShareMenu();
}

// Known friends first, then live hits — filtered by the box text, minus whoever's
// already on this trip, deduped by username.
function shareCandidates() {
  const q = $("#share-user").value.trim().toLowerCase();
  const already = new Set(shareCurrent);
  const seen = new Set();
  const out = [];
  const consider = (s) => {
    if (!s || !s.user || already.has(s.user) || seen.has(s.user)) return;
    const hay = `${s.user} ${s.displayName || ""}`.toLowerCase();
    if (q && !hay.includes(q)) return;
    seen.add(s.user);
    out.push(s);
  };
  shareFriends.forEach(consider);
  shareLive.forEach(consider);
  return out;
}

// One dropdown row. Names go through textContent (Nextcloud display names can
// hold HTML-special characters).
function shareMenuItem(s) {
  const it = el("div", "combo-item");
  it.setAttribute("role", "option");
  it.dataset.user = s.user;
  const name = el("span", "combo-name");
  name.textContent = s.displayName || s.user;
  it.append(name);
  if (s.displayName && s.displayName !== s.user) {
    const sub = el("span", "combo-sub");
    sub.textContent = `@${s.user}`;
    it.append(sub);
  }
  // mousedown (not click) so the pick fires before the input blur closes the menu
  it.onmousedown = (e) => {
    e.preventDefault();
    pickShare(s.user);
  };
  it.onmouseenter = () => setShareActive([...it.parentElement.children].indexOf(it));
  return it;
}

function shareMenuEmptyHint() {
  const q = $("#share-user").value.trim();
  if (q) return `No matches — press Share to add “${q}”.`;
  if (!shareFriends.length) return "Type a name to find people on your Nextcloud.";
  return "Everyone you share with is already on this trip.";
}

function renderShareMenu() {
  const menu = $("#share-menu");
  menu.innerHTML = "";
  shareActive = -1;
  const cands = shareCandidates();
  if (cands.length) {
    for (const s of cands) menu.append(shareMenuItem(s));
  } else {
    const empty = el("div", "combo-empty");
    empty.textContent = shareMenuEmptyHint(); // may hold typed text → textContent
    menu.append(empty);
  }
  menu.hidden = false;
  $("#share-user").setAttribute("aria-expanded", "true");
}

function openShareMenu() {
  if (!$("#share-add").hidden) renderShareMenu();
}
function closeShareMenu() {
  const menu = $("#share-menu");
  if (!menu) return;
  menu.hidden = true;
  menu.innerHTML = "";
  shareActive = -1;
  const inp = $("#share-user");
  if (inp) inp.setAttribute("aria-expanded", "false");
}
function isShareMenuOpen() {
  const menu = $("#share-menu");
  return !!menu && !menu.hidden;
}

function setShareActive(i) {
  const items = [...$("#share-menu").querySelectorAll(".combo-item")];
  if (!items.length) return;
  shareActive = Math.max(0, Math.min(i, items.length - 1));
  items.forEach((node, j) => node.classList.toggle("active", j === shareActive));
  items[shareActive].scrollIntoView({ block: "nearest" });
}

function pickShare(user) {
  $("#share-user").value = "";
  closeShareMenu();
  addShareUser(user);
}

// Debounced live Nextcloud search feeds shareLive, then re-renders the open menu.
let shareeTimer = null;
$("#share-user").addEventListener("input", () => {
  openShareMenu();
  clearTimeout(shareeTimer);
  const q = $("#share-user").value.trim();
  if (q.length < 2) {
    shareLive = [];
    if (isShareMenuOpen()) renderShareMenu();
    return;
  }
  shareeTimer = setTimeout(async () => {
    let hits;
    try {
      hits = await invoke("sharee_search", { query: q });
    } catch {
      return;
    }
    shareLive = hits;
    if (isShareMenuOpen()) renderShareMenu();
  }, 250);
});
$("#share-user").addEventListener("focus", openShareMenu);
$("#share-toggle").addEventListener("mousedown", (e) => {
  e.preventDefault(); // keep the input focused
  if (isShareMenuOpen()) {
    closeShareMenu();
  } else {
    $("#share-user").focus();
    openShareMenu();
  }
});
$("#share-user").addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    if (!isShareMenuOpen()) openShareMenu();
    else setShareActive(shareActive + 1);
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    if (isShareMenuOpen()) setShareActive(shareActive - 1);
  } else if (e.key === "Enter") {
    e.preventDefault();
    const items = [...$("#share-menu").querySelectorAll(".combo-item")];
    if (isShareMenuOpen() && shareActive >= 0 && items[shareActive]) {
      pickShare(items[shareActive].dataset.user);
    } else {
      addShare();
    }
  } else if (e.key === "Escape" && isShareMenuOpen()) {
    e.preventDefault();
    e.stopPropagation(); // close the menu, not the dialog
    closeShareMenu();
  }
});
$("#share-go").addEventListener("click", addShare);
// A click elsewhere in the dialog (outside the combo) dismisses the menu.
shareDlg.addEventListener("mousedown", (e) => {
  if (isShareMenuOpen() && !e.target.closest("#share-combo")) closeShareMenu();
});
$("#share-close").addEventListener("click", () => shareDlg.close());
shareDlg.addEventListener("click", (e) => {
  if (e.target === shareDlg) shareDlg.close();
});

// ============================ sync a trip with the cloud ============================
// Show a trip's drift from the shared cloud (what to push/pull/clean, plus owed
// ops) and reconcile the chosen groups. The status is fetched live on open and on
// Refresh; the card chip meanwhile rides on the cached SyncBrief.
const syncDlg = $("#sync-dialog");
let syncTrip = null;
let syncData = null;
let syncActions = {};

// Each drift bucket: a short chip label naming what the clips *are*, the chip
// colour, the reconcile action it enables (null = surfaced only, never
// auto-applied), the checkbox verb, and a line saying plainly what ticking it
// does. The labels used to mix states with imperatives, which left you guessing
// which side of the sync a group sat on and what the tickbox was about to touch —
// hence the `hint`, and the size inside it, since "download · 250" said nothing
// about the 100+ GiB behind it.
const SYNC_GROUPS = [
  {
    key: "toPush",
    label: "Yours, not shared yet",
    cls: "new",
    act: "push",
    verb: "upload",
    hint: (size) => `Uploads ${size} to the cloud so everyone on this trip can pull it. Your copies stay put.`,
  },
  {
    key: "toPull",
    label: "New from others",
    cls: "new",
    act: "pull",
    verb: "download",
    hint: (size) =>
      `Footage a friend added that has never been on this machine. Copies ${size} here; nothing in the cloud changes.`,
  },
  {
    key: "deletedLocal",
    label: "Deleted here, still in the cloud",
    cls: "warn",
    act: "pushDeletions",
    verb: "delete for everyone",
    hint: (size) =>
      `You deleted these locally. Ticking this erases ${size} from the cloud for everyone sharing the trip; leave it unticked and they stay.`,
  },
  {
    // The pair above and below are the two "it's in the cloud, not here" cases, and
    // the difference is whether you've *had* it: toPull has never been on this
    // machine, cloudOnly was and you freed it. That's also why one is ticked by
    // default and the other isn't.
    key: "cloudOnly",
    label: "Freed here, still in the cloud",
    cls: "unknown",
    act: "restoreCloud",
    verb: "bring back",
    hint: (size) =>
      `Footage you had and freed — archived to reclaim disk, or pulled and later cleared. Brings ${size} back down; the cloud copy stays where it is.`,
  },
  {
    key: "conflicts",
    label: "Differ from the cloud",
    cls: "warn",
    act: null,
    hint: () => "Same name either side, different size. reel won't guess which wins, so both are left alone.",
  },
  {
    key: "deletedUpstream",
    label: "Gone from the cloud",
    cls: "unknown",
    act: null,
    hint: () => "Whoever shared these removed them. Your local copy is untouched — it just isn't shared any more.",
  },
];

// `preselect` ticks one action's box on arrival — used by an archived trip's
// Restore button, which would otherwise land you on a panel whose only checkbox
// is deliberately off by default.
let syncPreselect = null;
async function openSync(t, preselect = null) {
  syncTrip = t;
  syncData = null;
  syncPreselect = preselect;
  $("#sync-title").textContent = `Sync ${t.name}`;
  $("#sync-meta").textContent = "Checking the cloud…";
  $("#sync-list").innerHTML = "";
  $("#sync-error").hidden = true;
  $("#sync-prog").hidden = true;
  $("#sync-go").disabled = true;
  syncDlg.showModal();
  await refreshSync();
}

async function refreshSync() {
  if (!syncTrip) return;
  $("#sync-refresh").disabled = true;
  $("#sync-error").hidden = true;
  try {
    syncData = await invoke("sync_status", { trip: syncTrip.name, refresh: true });
  } catch (e) {
    $("#sync-meta").textContent = "";
    $("#sync-error").textContent = String(e);
    $("#sync-error").hidden = false;
    $("#sync-refresh").disabled = false;
    return;
  }
  if (!syncDlg.open) return;
  $("#sync-refresh").disabled = false;
  renderSync();
}

function renderSync() {
  const d = syncData;
  const list = $("#sync-list");
  list.innerHTML = "";
  $("#sync-prog").hidden = true;
  syncActions = {};

  const when = d.lastCloudCheck ? `checked ${ago(d.lastCloudCheck)}` : "not checked yet";
  if (d.offline) $("#sync-meta").textContent = `Cloud unreachable — last known state (${when}).`;
  else if (d.inSync && !d.pending) $("#sync-meta").textContent = `In sync with the cloud · ${when}.`;
  else $("#sync-meta").textContent = `Pick what to reconcile, then Sync · ${when}.`;

  let anyAction = false;
  for (const g of SYNC_GROUPS) {
    const items = d[g.key] || [];
    if (!items.length) continue;
    const row = el("div", "sync-group");
    const head = el("div", "sync-group-head");
    head.append(chip({ cls: g.cls, text: `${g.label} · ${items.length}` }));
    if (g.act && !d.offline) {
      const cb = el("input");
      cb.type = "checkbox";
      cb.id = `sync-act-${g.act}`;
      // Additive-and-expected legs are on by default. The two that aren't: deleting
      // cloud copies is destructive, and bringing footage back re-fills disk you
      // freed on purpose — neither should ride along on a click of Sync.
      cb.checked =
        g.act === syncPreselect || (g.act !== "pushDeletions" && g.act !== "restoreCloud");
      syncActions[g.act] = cb.checked;
      cb.onchange = () => (syncActions[g.act] = cb.checked);
      anyAction = true;
      const lab = el("label", "sync-act");
      lab.htmlFor = cb.id;
      lab.append(cb, el("span", null, g.verb));
      head.append(lab);
    }
    row.append(head);
    const total = items.reduce((s, i) => s + (i.bytes || 0), 0);
    row.append(el("div", "sync-group-hint", g.hint(fmtBytes(total))));
    const names = items
      .slice(0, 6)
      .map((i) => (i.person && !i.mine ? `${i.person}/` : "") + i.name)
      .join(", ");
    row.append(el("div", "sync-group-items tnum", names + (items.length > 6 ? ` +${items.length - 6}` : "")));
    list.append(row);
  }

  if (d.pending) {
    const row = el("div", "sync-group");
    row.append(elHTML("div", "sync-group-head", `<span class="chip warn">⟳ ${plural(d.pending, "owed cloud op")}</span>`));
    row.append(el("div", "sync-group-items", "A move, rename, or purge queued while offline — applied first when you Sync."));
    list.append(row);
  }

  if (!list.children.length) {
    list.append(el("div", "empty-note", d.offline ? "Can't reach the cloud right now." : "Everything is in sync."));
  }
  $("#sync-go").disabled = d.offline || (!anyAction && !d.pending);
}

const SYNC_PHASE = {
  replay: "Applying owed ops…",
  check: "Checking the cloud…",
  push: "Uploading",
  pull: "Downloading",
  restore: "Bringing back",
  delete: "Removing from cloud",
};

async function startSync() {
  if (!syncTrip || !syncData) return;
  const actions = {
    push: !!syncActions.push,
    pull: !!syncActions.pull,
    pushDeletions: !!syncActions.pushDeletions,
    restoreCloud: !!syncActions.restoreCloud,
  };
  $("#sync-go").disabled = true;
  $("#sync-refresh").disabled = true;
  $("#sync-error").hidden = true;
  $("#sync-prog").hidden = false;
  const stage = $("#sync-stage");
  const pct = $("#sync-pct");
  const fill = $("#sync-fill");
  const setFill = (f) => (fill.style.transform = `scaleX(${Math.max(0, Math.min(1, f))})`);
  setFill(0);

  const Ch = window.__TAURI__?.core?.Channel;
  const channel = Ch ? new Ch() : null;
  if (channel)
    channel.onmessage = (p) => {
      const label = SYNC_PHASE[p.phase] || p.phase;
      stage.textContent = p.file ? `${label} ${p.file}` : label;
      const f = p.total ? p.done / p.total : 0;
      pct.textContent = p.total ? `${Math.round(f * 100)}%` : "";
      setFill(p.phase === "check" || p.phase === "replay" ? 0.08 : f);
    };

  try {
    const r = await invoke("sync_trip", { channel, trip: syncTrip.name, actions });
    setFill(1);
    toast(syncSummary(r));
    await load(); // the cards' chips reflect the new state
    syncTrip = (lastTrips || []).find((x) => x.name === syncTrip.name) || syncTrip;
    await refreshSync();
  } catch (e) {
    $("#sync-error").textContent = String(e);
    $("#sync-error").hidden = false;
    $("#sync-prog").hidden = true;
    $("#sync-go").disabled = false;
    $("#sync-refresh").disabled = false;
  }
}

function syncSummary(r) {
  const bits = [];
  if (r.replayed) bits.push(`${plural(r.replayed, "owed op")} applied`);
  if (r.pushed) bits.push(`${plural(r.pushed, "clip")} shared`);
  if (r.pulled) bits.push(`${plural(r.pulled, "clip")} pulled`);
  if (r.restored) bits.push(`${plural(r.restored, "clip")} brought back`);
  if (r.deleted) bits.push(`${plural(r.deleted, "clip")} removed from the cloud`);
  if (r.stillPending) bits.push(`${plural(r.stillPending, "op")} still owed`);
  if (!bits.length) return r.inSync ? "Already in sync" : "Nothing to do";
  return bits.join(" · ");
}

$("#sync-refresh").addEventListener("click", refreshSync);
$("#sync-go").addEventListener("click", startSync);
$("#sync-close").addEventListener("click", () => syncDlg.close());
syncDlg.addEventListener("click", (e) => {
  if (e.target === syncDlg) syncDlg.close();
});

// ---- Duplicates panel — the same clip in more than one trip, or a cloud orphan.
// Scans the whole library + cloud (dedup_scan), lets you pick which copy to keep
// per group, and prunes the rest (dedup_resolve). Pruning is NOT a permanent
// delete: the content survives in the kept copy, so nothing is tombstoned.
const dedupDlg = $("#dedup-dialog");
let dedupData = null;
let dedupPick = {}; // group key → index of the copy to keep
let dedupOn = {}; // group key → included in the prune?

async function openDedup() {
  dedupData = null;
  dedupPick = {};
  dedupOn = {};
  $("#dedup-list").innerHTML = "";
  $("#dedup-error").hidden = true;
  $("#dedup-prog").hidden = true;
  $("#dedup-go").disabled = true;
  dedupDlg.showModal();
  await rescanDedup();
}

async function rescanDedup() {
  $("#dedup-rescan").disabled = true;
  $("#dedup-error").hidden = true;
  $("#dedup-meta").textContent = "Scanning your library and the cloud…";
  try {
    dedupData = await invoke("dedup_scan");
  } catch (e) {
    $("#dedup-meta").textContent = "";
    $("#dedup-error").textContent = String(e);
    $("#dedup-error").hidden = false;
    $("#dedup-rescan").disabled = false;
    return;
  }
  if (!dedupDlg.open) return;
  $("#dedup-rescan").disabled = false;
  renderDedup();
}

function renderDedup() {
  const d = dedupData;
  const list = $("#dedup-list");
  list.innerHTML = "";
  $("#dedup-prog").hidden = true;
  dedupPick = {};
  dedupOn = {};

  const scanned = `${d.scannedLocal} local${d.offline ? "" : ` · ${d.scannedCloud} cloud`} clips scanned`;
  if (d.offline)
    $("#dedup-meta").textContent = `Cloud unreachable — checked local footage only. ${scanned}.`;
  else if (!d.groups.length) $("#dedup-meta").textContent = `No duplicates found. ${scanned}.`;
  else
    $("#dedup-meta").textContent = `${plural(d.groupsCount, "duplicate")} · ${fmtBytes(d.totalReclaimable)} reclaimable · ${scanned}.`;

  for (const g of d.groups) {
    dedupPick[g.key] = g.suggestedKeep;
    dedupOn[g.key] = true;
    const row = el("div", "dup-group");

    const head = el("div", "dup-head");
    const inc = el("input", "dup-inc");
    inc.type = "checkbox";
    inc.checked = true;
    inc.onchange = () => {
      dedupOn[g.key] = inc.checked;
      row.classList.toggle("off", !inc.checked);
      updateDedupGo();
    };
    const nm = el("span", "dup-name");
    nm.textContent = g.name; // basename — user-controlled, textContent
    head.append(inc, nm, chip({ cls: "new", text: `${g.copies.length} copies · frees ${fmtBytes(g.reclaimable)}` }));
    row.append(head);

    const copies = el("div", "dup-copies");
    g.copies.forEach((c, i) => {
      const lab = el("label", "dup-copy");
      const rb = el("input");
      rb.type = "radio";
      rb.name = `dup-${g.key}`;
      rb.checked = i === g.suggestedKeep;
      rb.onchange = () => {
        if (rb.checked) dedupPick[g.key] = i;
      };
      const where = el("span", "dup-where");
      where.textContent = `${c.trip} / ${c.person}`; // trip + person — textContent
      const badge = el("span", "dup-loc " + (c.local && c.inCloud ? "both" : c.local ? "local" : "cloud"));
      badge.textContent = c.local && c.inCloud ? "local + cloud" : c.local ? "local only" : "cloud only";
      lab.append(rb, where, badge);
      copies.append(lab);
    });
    row.append(copies);
    row.append(el("div", "dup-hint", "Keep the checked copy · the rest are pruned"));
    list.append(row);
  }

  if (!d.groups.length)
    list.append(el("div", "empty-note", d.offline ? "Can't reach the cloud right now." : "No duplicate clips — everything lives in one place."));
  updateDedupGo();
}

function updateDedupGo() {
  const any = dedupData && dedupData.groups.some((g) => dedupOn[g.key]);
  $("#dedup-go").disabled = !any;
}

function dedupLoc(c) {
  return { trip: c.trip, rel: c.rel, local: c.local, inCloud: c.inCloud, bytes: c.bytes };
}

async function startDedup() {
  if (!dedupData) return;
  const resolutions = [];
  for (const g of dedupData.groups) {
    if (!dedupOn[g.key]) continue;
    const keepIdx = dedupPick[g.key] ?? g.suggestedKeep;
    resolutions.push({
      keep: dedupLoc(g.copies[keepIdx]),
      remove: g.copies.filter((_, i) => i !== keepIdx).map(dedupLoc),
    });
  }
  if (!resolutions.length) return;

  $("#dedup-go").disabled = true;
  $("#dedup-rescan").disabled = true;
  $("#dedup-error").hidden = true;
  $("#dedup-prog").hidden = false;
  const stage = $("#dedup-stage");
  const pct = $("#dedup-pct");
  const fill = $("#dedup-fill");
  const setFill = (f) => (fill.style.transform = `scaleX(${Math.max(0, Math.min(1, f))})`);
  setFill(0);

  const Ch = window.__TAURI__?.core?.Channel;
  const channel = Ch ? new Ch() : null;
  if (channel)
    channel.onmessage = (p) => {
      stage.textContent = p.file ? `Removing ${p.file}` : "Removing…";
      const f = p.total ? p.done / p.total : 0;
      pct.textContent = p.total ? `${Math.round(f * 100)}%` : "";
      setFill(f);
    };

  try {
    const r = await invoke("dedup_resolve", { channel, resolutions });
    setFill(1);
    toast(dedupSummary(r));
    await load(); // cards reflect freed local space / changed trips
    await rescanDedup(); // the list should now be empty or smaller
  } catch (e) {
    $("#dedup-error").textContent = String(e);
    $("#dedup-error").hidden = false;
    $("#dedup-prog").hidden = true;
    $("#dedup-go").disabled = false;
    $("#dedup-rescan").disabled = false;
  }
}

function dedupSummary(r) {
  const bits = [];
  if (r.removedLocal) bits.push(`${r.removedLocal} local removed`);
  if (r.removedCloud) bits.push(`${r.removedCloud} cloud removed`);
  if (r.freed) bits.push(`${fmtBytes(r.freed)} freed`);
  // a duplicate under someone else's folder: the local copy goes, their cloud copy stays
  if (r.keptCloud) bits.push(`${r.keptCloud} kept in cloud (not yours)`);
  if (r.skipped) bits.push(`${r.skipped} skipped`);
  if (!r.cloudOk) bits.push("cloud cleanup owed (offline)");
  return bits.length ? bits.join(" · ") : "Nothing to prune";
}

$("#dedup-open").addEventListener("click", openDedup);
$("#dedup-rescan").addEventListener("click", rescanDedup);
$("#dedup-go").addEventListener("click", startDedup);
$("#dedup-close").addEventListener("click", () => dedupDlg.close());
dedupDlg.addEventListener("click", (e) => {
  if (e.target === dedupDlg) dedupDlg.close();
});

// Global Sync — sweep every trip through the cloud: replay owed ops, then upload
// your unshared footage and pull what others added (deletions/conflicts stay a
// per-trip, opt-in decision). Also the only place a purge owed from an offline
// trip-delete can land (that trip has no card to open). Progress opens a panel
// with an overall "trip X of Y", the trip in flight, and a per-trip log.
const syncAllDlg = $("#syncall-dialog");

// One log row per trip, ticking from busy → done as the sweep passes over it.
function syncAllRow(trip) {
  const li = el("li", "busy");
  li.dataset.trip = trip;
  const name = el("span", "log-name");
  name.textContent = trip;
  li.append(el("span", "log-mark", "⟳"), name, el("span", "log-note", "checking…"));
  $("#syncall-log").append(li);
  li.scrollIntoView({ block: "nearest" });
  return li;
}

// Settle a finished row: "updated" if it pushed/pulled/removed anything this run,
// else "in sync" (only checked). We infer from the phases seen, since the sweep
// streams per-file progress, not a per-trip tally.
function finishSyncRow(li) {
  if (!li) return;
  li.classList.remove("busy");
  const mark = li.querySelector(".log-mark");
  const note = li.querySelector(".log-note");
  if (li.dataset.worked) {
    li.classList.add("done");
    mark.textContent = "✓";
    note.textContent = "updated";
  } else {
    li.classList.add("synced");
    mark.textContent = "·";
    note.textContent = "in sync";
  }
}

$("#sync-all").addEventListener("click", async () => {
  const b = $("#sync-all");
  if (b.disabled) return;
  b.disabled = true;

  // reset the panel to a clean "starting" state
  $("#syncall-log").replaceChildren();
  $("#syncall-error").hidden = true;
  $("#syncall-meta").textContent = "Checking the cloud…";
  $("#syncall-overall").textContent = "Preparing…";
  $("#syncall-count").textContent = "";
  $("#syncall-stage").textContent = "";
  $("#syncall-pct").textContent = "";
  const obar = $("#syncall-obar");
  const fill = $("#syncall-fill");
  const setBar = (node, f) => (node.style.transform = `scaleX(${Math.max(0, Math.min(1, f))})`);
  setBar(obar, 0);
  setBar(fill, 0);
  const closeBtn = $("#syncall-close");
  closeBtn.disabled = true;
  syncAllDlg.showModal();

  let curTrip = null;
  let curRow = null;
  // fraction through the current trip's active transfer (0 while just checking)
  const inner = (p) => (p.total ? p.done / p.total : 0);

  const Ch = window.__TAURI__?.core?.Channel;
  const channel = Ch ? new Ch() : null;
  if (channel)
    channel.onmessage = (p) => {
      const label = SYNC_PHASE[p.phase] || p.phase;
      if (p.tripCount > 0) {
        // the per-trip sweep — advance the overall counter and the trip log
        $("#syncall-overall").textContent = `Trip ${p.tripIndex} of ${p.tripCount}`;
        $("#syncall-count").textContent = `${p.tripIndex}/${p.tripCount}`;
        setBar(obar, (p.tripIndex - 1 + inner(p)) / p.tripCount);
        if (p.trip && p.trip !== curTrip) {
          finishSyncRow(curRow);
          curTrip = p.trip;
          curRow = syncAllRow(p.trip);
        }
        if (curRow) {
          if (p.phase === "push" || p.phase === "pull" || p.phase === "delete") {
            curRow.dataset.worked = "1";
          }
          curRow.querySelector(".log-note").textContent = p.file
            ? `${label.toLowerCase()} ${p.file}`
            : label.toLowerCase();
        }
        $("#syncall-meta").textContent = p.trip;
      } else if (p.phase === "replay") {
        // the pre-sweep owed-op flush (no per-trip counter yet)
        $("#syncall-overall").textContent = "Applying owed ops…";
        $("#syncall-count").textContent = p.total ? `${p.done}/${p.total}` : "";
        setBar(obar, p.total ? p.done / p.total : 0.05);
        $("#syncall-meta").textContent = p.file ? `Owed op on ${p.file}` : "Applying owed ops…";
      }
      $("#syncall-stage").textContent = p.file ? `${label} ${p.file}` : label;
      $("#syncall-pct").textContent = p.total ? `${Math.round((p.done / p.total) * 100)}%` : "";
      setBar(fill, inner(p));
    };

  try {
    const r = await invoke("sync_all", { channel });
    finishSyncRow(curRow);
    setBar(obar, 1);
    setBar(fill, 1);
    $("#syncall-overall").textContent = "Done";
    $("#syncall-stage").textContent = "";
    $("#syncall-pct").textContent = "";
    $("#syncall-meta").textContent = syncSummary(r);
    await load(); // cards' chips reflect the new state
  } catch (e) {
    $("#syncall-error").textContent = String(e);
    $("#syncall-error").hidden = false;
    $("#syncall-overall").textContent = "Sync failed";
    $("#syncall-meta").textContent = "Couldn't reach the cloud.";
  } finally {
    closeBtn.disabled = false;
    b.disabled = false;
  }
});

$("#syncall-close").addEventListener("click", () => syncAllDlg.close());
// backdrop dismiss only once the run has finished (Close enabled)
syncAllDlg.addEventListener("click", (e) => {
  if (e.target === syncAllDlg && !$("#syncall-close").disabled) syncAllDlg.close();
});
// block Escape while a sweep is still running
syncAllDlg.addEventListener("cancel", (e) => {
  if ($("#syncall-close").disabled) e.preventDefault();
});

setPlayIcon(false);

load();
