"use strict";

// Tauri v2 with withGlobalTauri exposes invoke here.
const invoke = (cmd, args) =>
  window.__TAURI__?.core?.invoke
    ? window.__TAURI__.core.invoke(cmd, args)
    : Promise.reject(new Error("not running inside the reel window"));

const $ = (sel) => document.querySelector(sel);

// ---- formatting ----
function fmtBytes(b) {
  const gib = b / 1073741824;
  if (gib >= 1) return `${gib.toFixed(1)} GiB`;
  const mib = b / 1048576;
  return `${mib.toFixed(0)} MiB`;
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

function el(tag, cls, html) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (html != null) n.innerHTML = html;
  return n;
}

// latest trips, kept so the import dialog can offer them as destinations
let lastTrips = [];

let toastTimer;
function toast(msg) {
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
  if (!ref || !ref.path) return; // leave the placeholder
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

// ---- share vocabulary (the shared pool of everyone's clips — not a personal
// backup; your footage is "shared" once it's pushed up to the remote) ----
const SHARE_CHIP = {
  shared: { cls: "safe", text: "✓ Shared" },
  local: { cls: "warn", text: "⚠ Local only" },
  unknown: { cls: "unknown", text: "Sharing unknown" },
};
function chip(spec) {
  return el("span", `chip ${spec.cls}`, spec.text);
}

// A session's status chips + its primary action, by import/share state. `act`
// drives which handler the button runs: import, clear (reclaim), or add.
function sessionStatus(s) {
  const where = s.owners.length ? `in ${s.owners.join(", ")}` : "imported";
  if (s.newClips === s.clips) {
    return { chips: [{ cls: "new", text: `● ${plural(s.clips, "clip")} new` }], action: "Import →", act: "import" };
  }
  if (s.newClips === 0) {
    if (s.safe)
      return {
        chips: [{ cls: "safe", text: "✓ Safe to clear" }, { cls: "owned", text: where }],
        action: "Clear →",
        act: "clear",
      };
    return {
      chips: [{ cls: "owned", text: where }, { cls: "warn", text: "⚠ Share to clear" }],
      action: "Add to…",
      act: "add",
    };
  }
  return {
    chips: [
      { cls: "new", text: `● ${s.newClips} new` },
      { cls: "owned", text: `${s.clips - s.newClips} ${where}` },
    ],
    action: "Import new →",
    act: "import",
  };
}

function sessionAction(act, s) {
  if (act === "import") return openImport(s);
  if (act === "clear")
    return openWipe({ window: [s.start, s.end], label: `the ${fmtRange(s.start, s.end)} session` });
  toast("Adding to an existing trip lands in a later build.");
}

// ---- render: inserted card ----
function renderCard(card) {
  const panel = $("#card-panel");
  panel.innerHTML = "";

  if (!card) {
    const empty = el("div", "card-empty");
    empty.append(
      el("div", null, "<strong>No card inserted</strong>"),
      el("div", "hint", "Insert a DJI, GoPro, or iPhone card to import a session.")
    );
    panel.append(empty);
    return;
  }

  const box = el("div", "cardbox");
  const head = el("div", "cardbox-head");
  const id = el("div", "card-id");
  id.append(el("span", "card-h", "Card inserted"), el("span", "card-path", card.roots[0] ?? ""));
  head.append(
    id,
    el(
      "span",
      "card-totals tnum",
      `${plural(card.clips, "clip")} · ${fmtBytes(card.bytes)} · ${plural(card.sessions.length, "session")}`
    )
  );
  box.append(head);

  const newCount = card.sessions.filter((s) => s.newClips > 0).length;
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
    const stuck = card.sessions.filter((s) => s.newClips === 0 && !s.safe).length;
    if (stuck) parts.push(`${stuck} imported, not shared`);
    if (newCount) safety.classList.add("has-new");
    safety.append(el("span", null, parts.join(" · ") || "Nothing to do here."));
  }
  box.append(safety);

  const list = el("div", "sessions");
  for (const s of card.sessions) {
    const row = el("div", "session");

    const strip = el("div", "strip");
    for (const ref of s.strip) strip.append(frameImg(ref));
    if (s.clips > s.strip.length) strip.append(el("div", "more tnum", `+${s.clips - s.strip.length}`));

    const when = el("div", "when");
    when.append(el("div", "range tnum", fmtRange(s.start, s.end)), el("div", "ago", ago(s.end)));

    const st = sessionStatus(s);
    const status = el("div", "status");
    for (const c of st.chips) status.append(chip(c));

    const actions = el("div", "actions");
    const btn = el("button", `btn small ${st.act === "import" ? "primary" : "ghost"}`, st.action);
    btn.type = "button";
    btn.onclick = () => sessionAction(st.act, s);
    actions.append(btn);

    row.append(strip, when, el("div", "s-meta tnum", `${plural(s.clips, "clip")} · ${fmtBytes(s.bytes)}`), status, actions);
    list.append(row);
  }
  box.append(list);
  panel.append(box);
}

// ---- render: trips ----
const NEXT_LABEL = { review: "Review →", cut: "Cut →", edit: "Edit →", import: "Import →" };

// "12 yours · 3 pulled from alice" — shown only when a trip mixes your footage
// with clips pulled from the shared pool; an all-yours trip needs no caption.
function provRow(t) {
  if (!t.pulled) return null;
  const names = t.contributors || [];
  const who =
    names.length > 2 ? `${names.slice(0, 2).join(", ")} +${names.length - 2}` : names.join(", ");
  const from = who ? ` <span class="prov-from">from ${who}</span>` : "";
  const row = el("div", "prov tnum");
  row.append(
    el("span", null, `<span class="n">${t.mine}</span> yours`),
    el("span", "prov-sep", "·"),
    el("span", null, `<span class="n">${t.pulled}</span> pulled${from}`)
  );
  return row;
}

// The trip card's footer row: size on the left, actions on the right. A "Share"
// action appears whenever you have footage here that isn't in the pool yet — it's
// the step that flips the trip to ✓ Shared and its card sessions to safe-to-clear.
function footActions(t, card) {
  const actions = el("div", "trip-actions");
  if (t.mine > 0 && t.share !== "shared") {
    const share = el("button", "btn small ghost", '<span class="btn-ico" aria-hidden="true">↑</span> Share');
    share.type = "button";
    share.onclick = () => startShare(t, card);
    actions.append(share);
  }
  // once everything's in the pool (yours shared, or all of it pulled), the local
  // raw can be freed — kept re-pullable, with clips/marks staying put
  if (t.masters > 0 && (t.mine === 0 || t.share === "shared")) {
    const arch = el("button", "btn small ghost", "Archive →");
    arch.type = "button";
    arch.onclick = () => openArchive(t);
    actions.append(arch);
  }
  // a trip that's already been cut but still has marks (you added more since) can
  // cut the new ones — additive, existing clips are left untouched. The fresh cut
  // is the primary button below when the trip is at the Marked step.
  if (t.next !== "cut" && t.marks > 0 && t.clips > 0) {
    const recut = el("button", "btn small ghost", '<span class="btn-ico" aria-hidden="true">✂</span> Cut');
    recut.type = "button";
    recut.onclick = () => startCut(t, card);
    actions.append(recut);
  }
  const next = el("button", "btn small primary", NEXT_LABEL[t.next] ?? t.next);
  next.type = "button";
  next.onclick =
    t.next === "review"
      ? () => openReview(t)
      : t.next === "cut"
        ? () => startCut(t, card)
        : () => toast(`"${t.next}" for ${t.name} is coming in a later build.`);
  actions.append(next);
  return [el("div", "trip-size tnum", fmtBytes(t.bytes)), actions];
}

function renderTrips(trips) {
  const wrap = $("#trips");
  wrap.innerHTML = "";
  $("#trips-sub").textContent = trips.length ? plural(trips.length, "trip") : "";

  if (!trips.length) {
    wrap.append(
      el("div", "empty-note", "No trips yet. Insert a card and import a session, or pull one from the pool.")
    );
    return;
  }

  for (const t of trips) {
    const card = el("div", "trip");
    card.style.setProperty("--tc", tripColor(t.name));

    const cover = el("div", "cover-wrap");
    if (t.cover) {
      const img = el("img", "cover");
      img.alt = "";
      cover.append(img);
      loadThumb(img, t.cover);
    }
    // any trip with footage can be skimmed — the cover is the way in
    if (t.masters > 0) {
      cover.classList.add("playable");
      cover.append(el("div", "cover-play", '<span aria-hidden="true">▶</span>'));
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
    top.append(el("div", "trip-name", t.name));
    const range = fmtTripRange(t.start, t.end) || (t.from || t.to ? `${t.from ?? "…"} → ${t.to ?? "…"}` : null);
    if (range) top.append(el("div", "trip-window", range));

    const chips = el("div", "trip-chips");
    const badge = el("span", "badge", `<span class="dot"></span>${t.state}`);
    badge.dataset.state = t.state;
    chips.append(badge);
    // the share chip is about YOUR footage; skip it on an all-pulled trip
    if (t.mine > 0) chips.append(chip(SHARE_CHIP[t.share] ?? SHARE_CHIP.unknown));

    const stats = el("div", "stats tnum");
    stats.append(
      el("div", null, `<span class="n">${t.masters}</span> clips`),
      el("div", null, `<span class="n">${t.marks}</span> marks`),
      el("div", null, `<span class="n">${t.clips}</span> cut`)
    );

    const foot = el("div", "trip-foot");
    foot.append(...footActions(t, card));

    body.append(top, chips);
    const prov = provRow(t);
    if (prov) body.append(prov);
    body.append(stats, foot);
    card.append(body);
    wrap.append(card);
  }
}

// ---- share: push a trip's footage to the pool, with progress inline on the card ----
function shareSummary(r) {
  if (r.uploaded > 0)
    return `Shared ${plural(r.files, "clip")} · ${fmtBytes(r.uploaded)} up to the pool`;
  return `${r.trip} already shared — ${plural(r.files, "clip")} verified in the pool`;
}

async function startShare(t, card) {
  const foot = card.querySelector(".trip-foot");
  if (!foot || card.dataset.pushing) return;
  card.dataset.pushing = "1";

  // Swap the footer for a progress readout, tinted the trip's own colour.
  foot.innerHTML = "";
  const stage = el("span", "share-stage", "Preparing…");
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
      if (p.phase === "verify") {
        // upload's done — hold the bar full and let the file count tick up
        stage.textContent = "Verifying…";
        pct.textContent = p.total ? `${p.done}/${p.total}` : "";
        setFill(1);
      } else {
        stage.textContent = p.file ? `Uploading ${p.file}` : "Uploading…";
        const f = p.total ? p.done / p.total : 0;
        pct.textContent = `${Math.round(f * 100)}%`;
        setFill(f);
      }
    };

  try {
    const res = await invoke("share_trip", { channel, trip: t.name });
    card.dataset.pushing = "";
    toast(shareSummary(res));
    await load(); // re-render: chip flips to ✓ Shared, the Share button drops off
  } catch (e) {
    card.dataset.pushing = "";
    foot.innerHTML = "";
    foot.append(el("div", "share-error", String(e)), ...footActions(t, card));
  }
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
  const fresh = s.newClips || s.clips;
  impMeta.textContent = `${fmtRange(s.start, s.end)} · ${plural(fresh, "clip")} · ${fmtBytes(s.bytes)}`;
  impHint.textContent =
    s.newClips < s.clips
      ? `${s.clips - s.newClips} already imported — only the ${plural(s.newClips, "new clip")} copy in.`
      : "A new name creates a trip; pick one below to add to it.";

  // existing trips as quick picks, each dotted in its own colour
  impPicks.innerHTML = "";
  for (const t of lastTrips) {
    const b = el("button", "trip-pick", `<span class="pdot"></span>${t.name}`);
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
  if (r.copied > 0) {
    const extra = r.skippedOther ? ` · ${r.skippedOther} left for other trips` : "";
    return `Imported ${plural(r.copied, "clip")} · ${fmtBytes(r.bytes)} into ${r.trip}${extra}`;
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

// ---- destructive-confirm dialog: stream a pool check, show the plan, then on
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
      ? `Verifying ${p.label} in the pool…`
      : "Verifying in the pool…";
  setCBar(p.total ? p.done / p.total : 0);
}

function mkChannel(handler) {
  const Ch = window.__TAURI__?.core?.Channel;
  if (!Ch) return null;
  const ch = new Ch();
  ch.onmessage = handler;
  return ch;
}

// Resolve true when the user hits the danger confirm, false on any dismissal.
function awaitConfirm() {
  return new Promise((resolve) => {
    const settle = (v) => {
      cGo.removeEventListener("click", onGo);
      cCancel.removeEventListener("click", onNo);
      cdlg.removeEventListener("close", onNo);
      resolve(v);
    };
    const onGo = () => settle(true);
    const onNo = () => settle(false);
    cGo.addEventListener("click", onGo);
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
async function runDestructive({ title, meta, confirmLabel, plan, summarize, commit, done }) {
  cTitle.textContent = title;
  cMeta.textContent = meta;
  cCheck.hidden = false;
  cStage.textContent = "Checking…";
  cCount.textContent = "";
  setCBar(0);
  cSummary.hidden = true;
  cErr.hidden = true;
  cGo.disabled = true;
  cGo.textContent = confirmLabel;
  cCancel.disabled = false;
  confirmBusy = false;
  cdlg.showModal();

  // 1) plan — the live pool check
  let planned;
  try {
    planned = await plan(mkChannel(onCheckProgress));
  } catch (e) {
    if (cdlg.open) showConfirmError(e);
    return;
  }
  if (!cdlg.open) return; // dismissed mid-check

  // 2) show the plan; wait for the explicit confirm
  cCheck.hidden = true;
  cSummary.innerHTML = summarize(planned);
  cSummary.hidden = false;
  cGo.disabled = false;
  if (!(await awaitConfirm())) return;

  // 3) commit
  confirmBusy = true;
  cGo.disabled = true;
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
  const where = plan.trips.length ? ` from ${plan.trips.join(", ")}` : "";
  const left = [];
  if (plan.notImported) left.push(`${plan.notImported} not imported`);
  if (plan.notVerified) left.push(`${plan.notVerified} not verified`);
  const tail = left.length ? ` <span class="left">${left.join(", ")} — left on the card.</span>` : "";
  return `<span class="free">${plural(plan.files.length, "clip")} · ${fmtBytes(
    plan.bytes
  )}</span> verified in the pool${where} and safe to delete.${tail}`;
}

function openWipe({ window: win, label }) {
  let files = [];
  return runDestructive({
    title: "Reclaim card",
    meta: `Clearing ${label}. Confirming each clip is in the pool before anything is deleted.`,
    confirmLabel: "Delete from card",
    plan: (channel) =>
      invoke("plan_reclaim", { channel, start: win ? win[0] : null, end: win ? win[1] : null }).then(
        (p) => ((files = p.files), p)
      ),
    summarize: reclaimSummary,
    commit: () => invoke("commit_reclaim", { files }),
    done: (res) => `Reclaimed ${plural(res.deleted, "clip")} · ${fmtBytes(res.bytes)} from the card`,
  });
}

// ---- trip archive (free local raw, keep clips) ----
function archiveSummary(plan) {
  return `<span class="free">${plural(plan.masters, "clip")} · ${fmtBytes(
    plan.bytes
  )}</span> of raw is safe in the pool. <span class="left">Freeing keeps your cut clips and marks — re-pull the raw anytime.</span>`;
}

function openArchive(t) {
  return runDestructive({
    title: `Archive ${t.name}`,
    meta: `Freeing ${t.name}'s local raw. Confirming all of it is in the pool first.`,
    confirmLabel: "Free local raw",
    plan: (channel) => invoke("plan_archive", { channel, trip: t.name }),
    summarize: archiveSummary,
    commit: (channel) => invoke("commit_archive", { channel, trip: t.name }),
    done: (res) => `Archived ${res.trip} — freed ${fmtBytes(res.freed)} (clips kept)`,
  });
}

// ---- load ----
async function load() {
  thumbQueue = []; // drop any pending thumbs from a previous scan
  try {
    const [card, trips] = await Promise.all([invoke("scan_card"), invoke("list_trips")]);
    lastTrips = trips;
    renderCard(card);
    renderTrips(trips);
    const bits = [plural(trips.length, "trip")];
    if (card) bits.push(`card: ${plural(card.sessions.length, "session")}`);
    $("#summary").textContent = bits.join("  ·  ");
  } catch (e) {
    $("#card-panel").innerHTML = "";
    $("#trips").innerHTML = "";
    $("#trips").append(el("div", "empty-note", `Couldn't load: ${e}`));
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

const HL_PRE = 2,
  HL_POST = 8; // `h` grabs [now-2s, now+8s], matching the script
const ICON_PLAY = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5v14l11-7z"/></svg>';
const ICON_PAUSE =
  '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1"/><rect x="14" y="5" width="4" height="14" rx="1"/></svg>';

const video = $("#video");

// player state
const P = {
  open: false,
  trip: null,
  clips: [],
  marks: [], // every mark in the trip, file order
  i: 0, // current clip index
  selected: null, // selected mark index, or null
  pendingIn: null, // an in-point awaiting its out
  scrubbing: false,
  seekAfterLoad: null, // seek target applied once the next clip's metadata loads
  saveT: null,
  clipBase: null, // loopback server base URL, fetched once (see clipUrl)
  loading: false, // a clip is loading/buffering (drives the stage spinner)
  loadT: null, // delay timer before the loading spinner appears
  loadTimeout: null, // backstop timer: give up on a stuck load
  preparing: false, // a proxy is being built (the "Preparing…" overlay owns the stage)
  zoneStart: null, // Ctrl+Space plays this segment [start,end]; loop repeats it
  zoneEnd: null,
  zoneLoop: false,
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
async function openReview(t) {
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
    trip: pl.trip,
    clips: pl.clips,
    marks: pl.marks || [],
    i: 0,
    selected: null,
    pendingIn: null,
    seekAfterLoad: null,
  });
  $("#player").style.setProperty("--tc", tripColor(t.name));
  $("#player-trip").textContent = t.name;
  $("#player").hidden = false;
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
  clearTimeout(P.loadT);
  clearTimeout(P.loadTimeout);
  P.loading = false;
  P.preparing = false;
  stopShuttle();
  clearZone();
  video.pause();
  video.removeAttribute("src");
  video.load();
  $("#player").hidden = true;
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
  P.pendingIn = null;
  hidePending();
  armPending(false);
  const c = curClip();
  video.pause(); // stop the outgoing clip while we probe/load the next
  stopShuttle();
  clearZone();
  video.poster = "";
  setPoster(c, i);
  hideStageOverlay();
  updateHead();
  updateProxyTag();
  updateFilmstripActive();
  renderScrubMarks();
  updateTime();

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
  if (!c.checked) {
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

  if (c.proxied) {
    playSrc(c.play, seekTo); // a clean cached proxy — load it directly
  } else if (c.hasProxy) {
    prepareThenPlay(c, seekTo); // native LRF/LRV present → fast remux, then play
  } else {
    playSrc(c.play, seekTo); // no fast source: try the master; onerror builds one
  }
}

function playSrc(path, seekTo) {
  P.seekAfterLoad = seekTo;
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
    const uri = await invoke("thumb", { path: c.master, fileid: c.fileid });
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
    const play = await invoke("make_proxy", { trip: P.trip, master: c.master });
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
  if (!c) return (tag.hidden = true);
  tag.hidden = false;
  tag.textContent = c.proxied ? "proxy" : "master";
  tag.classList.toggle("is-proxy", c.proxied);
  tag.disabled = c.proxied;
  tag.title = c.proxied
    ? "Scrubbing a fast proxy"
    : "Playing the master — click to build a fast proxy if it won't scrub";
}

// ---- transport ----
function togglePlay() {
  stopShuttle(); // Space always drops back to normal-rate playback
  if (video.paused) video.play().catch(() => {});
  else video.pause();
}
function setPlayIcon(playing) {
  $("#play").innerHTML = playing ? ICON_PAUSE : ICON_PLAY;
}
function nudge(dt) {
  if (!video.duration) return;
  stopShuttle();
  clearZone();
  video.currentTime = Math.max(0, Math.min(video.duration, video.currentTime + dt));
}
// Jump to the current clip's start (0) or end. `which`: 0 = Home, 1 = End.
function goToEnds(which) {
  if (!video.duration) return;
  stopShuttle();
  clearZone();
  video.currentTime = which ? Math.max(0, video.duration - 0.05) : 0;
}

// ---- shuttle (J/K/L): the editor-standard transport ----
// L ramps forward through these speeds, J ramps in reverse, K stops. A native
// <video> can't play a negative rate, so reverse is a stepped scrub on a timer
// (no audio — like a real shuttle held at speed), matching Kdenlive's J/K/L.
const SHUTTLE = [1, 1.5, 2, 3, 5.5, 10];
let revTimer = null,
  revRate = 0;
function stopReverse() {
  if (revTimer) clearInterval(revTimer);
  revTimer = null;
  revRate = 0;
}
function stopShuttle() {
  stopReverse();
  video.playbackRate = 1;
}
function pauseShuttle() {
  stopShuttle();
  video.pause();
}
function shuttle(dir) {
  const c = curClip();
  if (!c || c.stub || !video.duration) return;
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
function clearZone() {
  P.zoneStart = P.zoneEnd = null;
  P.zoneLoop = false;
}
function playZone(loop) {
  const z = currentZone();
  if (!z) return toast("No segment to play — set one with i / o, or pick it in Marks.");
  const start = () => {
    stopShuttle();
    video.currentTime = z.start;
    P.zoneStart = z.start;
    P.zoneEnd = z.end;
    P.zoneLoop = loop;
    video.play().catch(() => {});
  };
  const c = curClip();
  if (!c || z.master !== c.master) {
    // the mark lives on another clip — load it, then start once it's ready
    const ci = P.clips.findIndex((x) => x.master === z.master);
    if (ci < 0) return;
    loadClip(ci, z.start);
    const arm = () => {
      video.removeEventListener("canplay", arm);
      start();
    };
    video.addEventListener("canplay", arm);
  } else {
    start();
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
function updateTime() {
  const d = video.duration || 0,
    t = video.currentTime || 0;
  // zone play/loop: stop (or loop back) when the playhead reaches the zone's end
  if (P.zoneEnd != null && t >= P.zoneEnd) {
    if (P.zoneLoop) video.currentTime = P.zoneStart;
    else {
      video.pause();
      clearZone();
    }
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
  if (!video.duration) return;
  stopShuttle();
  clearZone();
  video.currentTime = f * video.duration;
}

// ---- marking ----
// A pending in-point arms the "o out" key hint (in the trip's colour) so the
// next step reads off the TUI strip; cleared when the segment closes or undoes.
function armPending(on) {
  $("#hint-out")?.classList.toggle("armed", on);
}
function markIn() {
  if (!video.duration) return;
  P.pendingIn = video.currentTime;
  armPending(true);
  updateTime();
}
function markOut() {
  const c = curClip();
  if (!c) return;
  if (P.pendingIn == null) return toast("No in-point yet — press i first.");
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
  } else if (video.duration) {
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
  $("#player-mark-count").textContent = P.marks.length ? plural(P.marks.length, "mark") : "no marks";
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
    if (!c.stub) loadThumb(img, { path: c.master, fileid: c.fileid });
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
$("#player-proxy").addEventListener("click", () => {
  const c = curClip();
  if (c && !c.proxied) prepareThenPlay(c, video.currentTime || null);
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

document.addEventListener("keydown", (e) => {
  if (!P.open) return;
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
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      gotoAdjacentMark(-1);
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      gotoAdjacentMark(1);
    }
    return;
  }
  switch (e.key) {
    case " ":
      e.preventDefault();
      togglePlay();
      break;
    case "ArrowLeft":
      e.preventDefault();
      nudge(e.shiftKey ? -1 : -5);
      break;
    case "ArrowRight":
      e.preventDefault();
      nudge(e.shiftKey ? 1 : 5);
      break;
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
    case "u":
      undo();
      break;
    case "e":
      e.preventDefault();
      labelLast();
      break;
    case "x":
      toggleMarksPanel();
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
      if (P.selected != null) {
        e.preventDefault();
        deleteMark(P.selected);
      }
      break;
  }
});

setPlayIcon(false);

load();
