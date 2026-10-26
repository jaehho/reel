// Zone-tick rules, pulled straight out of ui/app.js so the test can't drift from
// the source. Run by `make test`.
//
// This one logic has been wrong twice, both times in ways that only showed up
// under a specific key sequence, so it lives behind a test rather than a comment:
//   1. wrapping a playhead that had been seeked out past the end, which made a
//      mark impossible to *grow* — only shrink.
//   2. destroying a one-shot zone when ctrl+→ parked the playhead on its end, so
//      the next ctrl+→ found no zone and hopped to an unrelated mark.
import { readFileSync } from "node:fs";

const src = readFileSync(new URL("../ui/app.js", import.meta.url), "utf8");
const found = src.match(/function zoneAction\(t, z\) \{[\s\S]*?\n\}/);
if (!found) {
  console.error("FAIL: zoneAction not found in ui/app.js — was it renamed?");
  process.exit(1);
}
const zoneAction = new Function(`${found[0]}; return zoneAction;`)();

let fails = 0;
const check = (name, got, want) => {
  if (got !== want) fails++;
  console.log(`${got === want ? "ok  " : "FAIL"}  ${name}${got === want ? "" : `  (got ${got}, want ${want})`}`);
};

const LOOP = { zoneStart: 3, zoneEnd: 11, zoneLoop: true, zoneFree: false };
const ONCE = { zoneStart: 3, zoneEnd: 11, zoneLoop: false, zoneFree: false };

check("no zone does nothing", zoneAction(9, { zoneEnd: null }), "none");
check("inside the segment, loop", zoneAction(7, LOOP), "none");
check("inside the segment, one-shot", zoneAction(7, ONCE), "none");
check("playing through the end loops", zoneAction(11, LOOP), "wrap");
check("playing through the end stops a one-shot", zoneAction(11, ONCE), "stop");

// Trimming: `free` means the playhead is out here on purpose. Neither the wrap
// nor the stop may touch it — including for a one-shot zone (bug 2).
const parked = (base, end) => ({ ...base, zoneEnd: end, zoneFree: true });
check("parked on the end, loop", zoneAction(11.5, parked(LOOP, 11.5)), "none");
check("parked on the end, one-shot", zoneAction(11.5, parked(ONCE, 11.5)), "none");
check("parked again — trims repeatedly", zoneAction(12, parked(ONCE, 12)), "none");
check("seeked past the end to grow it", zoneAction(20, { ...LOOP, zoneFree: true }), "none");
check("parked on the start", zoneAction(2.5, { ...LOOP, zoneStart: 2.5 }), "none");

// Resuming clears `free`, so the loop behaves normally again.
check("loop resumed after a trim", zoneAction(11.5, { ...LOOP, zoneEnd: 11.5 }), "wrap");

// `x`/⌫ may only delete the selected mark when it is on the clip on screen: the
// selection survives `[`/`]`, so it can name a mark on another clip.
const sel = src.match(/function selectedMarkOn\(marks, selected, master\) \{[\s\S]*?\n\}/);
if (!sel) {
  console.error("FAIL: selectedMarkOn not found in ui/app.js — was it renamed?");
  process.exit(1);
}
const selectedMarkOn = new Function(`${sel[0]}; return selectedMarkOn;`)();
const marks = [{ master: "a.mp4" }, { master: "b.mp4" }];
check("selected mark on this clip", selectedMarkOn(marks, 1, "b.mp4"), 1);
check("selected mark on another clip", selectedMarkOn(marks, 1, "a.mp4"), -1);
check("nothing selected", selectedMarkOn(marks, null, "a.mp4"), -1);
check("stale selection past the end", selectedMarkOn(marks, 5, "a.mp4"), -1);

// i / o / h read the playhead, so they have to wait until <video> holds the clip on
// screen (#51): while the next clip is being probed the element still holds the
// last one, and its time would land on the clip you just picked. The real
// functions, run against stubs.
const grab = (re, what) => {
  const m = src.match(re);
  if (!m) {
    console.error(`FAIL: ${what} not found in ui/app.js — was it renamed?`);
    process.exit(1);
  }
  return m[0];
};
const markCode = [
  grab(/const HL_PRE = [\s\S]*?;/, "HL_PRE"),
  grab(/const MIN_MARK = [^;]+;/, "MIN_MARK"),
  ...["markReady", "markIn", "markOut", "highlightToExtend", "highlight"].map((n) =>
    grab(new RegExp(`function ${n}\\([^)]*\\) \\{[\\s\\S]*?\\n\\}`), n)
  ),
].join("\n");
const marking = ({ ready = true, t = 10, marks = [], pendingIn = null, clip = { master: "a.mp4" } } = {}) => {
  const seen = { toasts: 0, added: [], extended: [] };
  const stubs = {
    P: { pendingIn, zoneLoop: false, marks },
    video: { currentTime: t, duration: 20 },
    toast: () => seen.toasts++,
    curClip: () => clip,
    videoReady: () => ready,
    addMark: (...a) => seen.added.push(a),
    extendMark: (...a) => seen.extended.push(a),
    armPending: () => {},
    hidePending: () => {},
    updateTime: () => {},
    trimLoop: () => false,
  };
  const names = Object.keys(stubs);
  const fns = new Function(...names, `${markCode}; return { markIn, markOut, highlight, highlightToExtend };`)(
    ...names.map((k) => stubs[k])
  );
  return { ...fns, ...stubs, seen };
};
const json = JSON.stringify;

let m = marking({ ready: false });
m.highlight();
m.markIn();
check("h/i while the clip loads mark nothing", json([m.seen.added, m.seen.extended, m.P.pendingIn]), json([[], [], null]));
check("...and say why", m.seen.toasts, 2);
m = marking({ ready: false, pendingIn: 3 });
m.markOut();
check("o while the clip loads saves nothing", json(m.seen.added), json([]));
m = marking({ ready: false, clip: { master: "a.mp4", stub: true } });
m.highlight();
check("a stub clip is silent: its stage note says why", m.seen.toasts, 0);

m = marking({ t: 5 });
m.markIn();
m.markOut();
check("i then o at one instant saves nothing", json([m.seen.added, m.P.pendingIn, m.seen.toasts]), json([[], 5, 1]));
m.video.currentTime = 5.05;
m.markOut();
check("...nor 0.05 s", m.seen.added.length, 0);
m.video.currentTime = 5.2;
m.markOut();
check("...but 0.2 s is a mark", json([m.seen.added, m.P.pendingIn]), json([[["a.mp4", 5, 5.2, ""]], null]));
m = marking({ t: 9, pendingIn: 4 });
m.markOut();
check("o before i's time still orders the pair", json(m.seen.added), json([["a.mp4", 4, 9, ""]]));

const hl = (start, end, o = {}) => ({ master: "a.mp4", start, end, label: "hl", ...o });
m = marking({ t: 12 });
m.highlight();
check("h on a fresh clip marks 8 s before to 2 s after", json(m.seen.added), json([["a.mp4", 4, 14, "hl"]]));
m = marking({ t: 13, marks: [hl(4, 14)] });
m.highlight();
check("h again 1 s later extends it", json([m.seen.added, m.seen.extended]), json([[], [[0, 4, 15]]]));
m = marking({ t: 19, marks: [hl(0, 8)] });
m.highlight();
check("h far from the last one starts a new mark", json([m.seen.added.length, m.seen.extended]), json([1, []]));

const touch = (marks, s, e, master = "a.mp4") => m.highlightToExtend(marks, master, s, e);
check("touching windows merge", touch([hl(4, 14)], 14, 20), 0);
check("a gap does not", touch([hl(4, 14)], 15, 20), -1);
check("another clip's highlight does not", touch([hl(4, 14, { master: "b.mp4" })], 5, 15), -1);
check("a renamed mark is not a highlight", touch([hl(4, 14, { label: "waterfall" })], 5, 15), -1);
check("an i/o mark is not a highlight", touch([hl(4, 14, { label: "" })], 5, 15), -1);
check("the latest overlapping highlight wins", touch([hl(0, 10), hl(5, 15)], 8, 12), 1);

// Ctrl+Space names a zone from the selection or the mark under the playhead —
// never the clip's last mark (#10). That fallback armed a zone nobody asked
// for: one keypress yanked the playhead to whatever was marked last.
const zoneCode = grab(/function currentZone\(\) \{[\s\S]*?\n\}/, "currentZone");
const zoneOf = (marks, t, selected = null) => {
  const stubs = {
    P: { marks, selected },
    curClip: () => ({ master: "a.mp4" }),
    video: { currentTime: t },
  };
  const names = Object.keys(stubs);
  const currentZone = new Function(...names, `${zoneCode}; return currentZone;`)(...names.map((k) => stubs[k]));
  return currentZone();
};
const a = hl(0, 4),
  b = hl(8, 12);
check("zone: the selected mark wins", zoneOf([a, b], 2, 1), b);
check("zone: a mark under the playhead", zoneOf([a, b], 9, null), b);
check("zone: selection on another clip still names it", zoneOf([a, b], 5, 1), b);
check("zone: no mark here is not the clip's last mark", zoneOf([a, b], 5, null), null);
check("zone: an unmarked clip has no zone", zoneOf([], 5, null), null);

// `i` renders the readout; it never seeks or pauses (#10). The wrap used to
// live in updateTime, so markIn's render call could move the playhead — seen
// when a loop zone's mark had gone stale (zoneMark past an undo'd marks array),
// trimLoop declined, and `i` past the zone's end wrapped the picture away.
const liveCode = [grab(/function markReady\(\) \{[\s\S]*?\n\}/, "markReady"), grab(/function markIn\(\) \{[\s\S]*?\n\}/, "markIn"), grab(/function updateTime\(\) \{[\s\S]*?\n\}/, "updateTime")].join("\n");
const pressI = (zone) => {
  const seen = { seeks: 0, pauses: 0, clears: 0, toasts: 0 };
  let t = 20;
  const video = {
    duration: 30,
    get currentTime() {
      return t;
    },
    set currentTime(v) {
      seen.seeks++;
      t = v;
    },
    pause() {
      seen.pauses++;
    },
  };
  const stubs = {
    P: { pendingIn: null, zoneLoop: false, zoneMark: null, zoneStart: 3, zoneEnd: 11, zoneFree: false, marks: [], ...zone },
    video,
    $: () => ({ textContent: "", style: {}, hidden: false }),
    fmtTime: (x) => String(x),
    toast: () => seen.toasts++,
    curClip: () => ({ master: "a.mp4" }),
    videoReady: () => true,
    armPending: () => {},
    trimLoop: () => false, // a stale zoneMark: the mark it named is gone
    clearZone: () => seen.clears++,
  };
  const names = Object.keys(stubs);
  const { markIn } = new Function(...names, `${liveCode}; return { markIn };`)(...names.map((k) => stubs[k]));
  markIn();
  return { seen, P: stubs.P };
};
let p = pressI({ zoneLoop: true });
check("i past the loop zone's end seeks nothing", p.seen.seeks, 0);
check("...pauses nothing", p.seen.pauses, 0);
check("...clears no zone", p.seen.clears, 0);
check("...and still arms the mark at the playhead", p.P.pendingIn, 20);
p = pressI({});
check("i past a one-shot zone's end seeks nothing", p.seen.seeks, 0);
check("...and pauses nothing", p.seen.pauses, 0);

// Toasts (#57): the stay scales with the length of the line, an error waits to
// be dismissed instead of vanishing mid-read, and a toast may carry one action.
const toastCode = [
  "let toastTimer;",
  grab(/function toastStay\(msg\) \{[\s\S]*?\n\}/, "toastStay"),
  grab(/function hideToast\(\) \{[\s\S]*?\n\}/, "hideToast"),
  grab(/function toast\(msg, opts = \{\}\) \{[\s\S]*?\n\}/, "toast"),
].join("\n");
const toastBox = () => {
  const seen = { timers: [], logs: [], kids: [] };
  const node = {
    hidden: true,
    innerHTML: "",
    attrs: {},
    classList: { toggle() {}, add() {}, remove() {} },
    setAttribute(k, v) {
      node.attrs[k] = v;
    },
    append(...xs) {
      seen.kids.push(...xs);
    },
  };
  const stubs = {
    $: () => node,
    el: (tag, cls, text) => ({
      tag,
      cls,
      text,
      attrs: {},
      type: "",
      onclick: null,
      setAttribute(k, v) {
        this.attrs[k] = v;
      },
    }),
    logf: (lv, m) => seen.logs.push([lv, m]),
    requestAnimationFrame: (f) => f(),
    setTimeout: (f, ms) => (seen.timers.push(ms), seen.timers.length),
    clearTimeout: () => {},
  };
  const names = Object.keys(stubs);
  const api = new Function(...names, `${toastCode}; return { toast, toastStay, hideToast };`)(...names.map((k) => stubs[k]));
  return { ...api, seen, node };
};
const tb = toastBox();
check("toast: the stay is 4 s plus 40 ms per character", tb.toastStay("12345"), 4200);
tb.toast("Cut 3 clips");
check("toast: a line that isn't an error times out", json(tb.seen.timers), json([4000 + 40 * "Cut 3 clips".length]));
check("toast: and announces itself politely", tb.node.attrs.role, "status");
tb.seen.timers.length = 0;
tb.seen.kids.length = 0;
tb.toast("delete failed", { kind: "error" });
check("toast: an error never times out", json(tb.seen.timers), json([]));
check("toast: an error announces loudly", tb.node.attrs.role, "alert");
check("toast: an error gets a dismiss control", tb.seen.kids.some((k) => k.cls === "toast-x"), true);
check("toast: an error logs at warn", tb.seen.logs[tb.seen.logs.length - 1][0], "warn");
tb.seen.kids.length = 0;
let acted = 0;
tb.toast("Cut 3 clips", { action: { label: "Open folder", run: () => acted++ } });
const act = tb.seen.kids.find((k) => k.cls === "toast-act");
check("toast: an action button carries its label", act && act.text, "Open folder");
act.onclick();
check("toast: clicking it runs the action", acted, 1);

// Import destinations (#54): one name for every new session, defaulted to the
// last trip used, and a near-match warning when the typed name folds onto an
// existing trip — "Ha Giang" beside "ha-giang" used to silently split a trip.
const impCode = [
  grab(/function foldTripName\(s\) \{[\s\S]*?\n\}/, "foldTripName"),
  grab(/function importDestBadge\(name, trips\) \{[\s\S]*?\n\}/, "importDestBadge"),
  grab(/function importDefaultName\(sessions\) \{[\s\S]*?\n\}/, "importDefaultName"),
].join("\n");
const impApi = (last) => {
  const stubs = {
    plural: (n, w) => `${n} ${w}${n === 1 ? "" : "s"}`,
    isoDay: (sec) => `day-${sec}`,
    lastImportTrip: last,
  };
  const names = Object.keys(stubs);
  return new Function(...names, `${impCode}; return { foldTripName, importDestBadge, importDefaultName };`)(
    ...names.map((k) => stubs[k])
  );
};
const i0 = impApi(null);
check("import: a fresh name is a new trip", i0.importDestBadge("ha-giang", [{ name: "kyoto", masters: 3 }]).add, false);
check("import: an exact name adds to that trip", i0.importDestBadge("kyoto", [{ name: "kyoto", masters: 31 }]).text, "Add to kyoto (31 clips)");
check("import: case and spacing fold onto an existing trip", i0.importDestBadge("Ha Giang", [{ name: "ha-giang", masters: 31 }]).near, "ha-giang");
check("import: underscore folds too", i0.importDestBadge("ha_giang", [{ name: "ha-giang", masters: 31 }]).near, "ha-giang");
check("import: an empty name badges nothing", i0.importDestBadge("  ", []).text, "");
check("import: default is the sole owner", i0.importDefaultName([{ start: 5, owners: ["mu"] }, { start: 9, owners: ["mu"] }]), "mu");
check("import: else the last trip used", impApi("mu-cang-chai").importDefaultName([{ start: 5, owners: [] }]), "mu-cang-chai");
check("import: else the capture date", i0.importDefaultName([{ start: 5, owners: [] }]), "day-5");
check("import: two owners are not one destination", impApi("last").importDefaultName([{ start: 5, owners: ["a"] }, { start: 9, owners: ["b"] }]), "last");

// Review resume (#55): reopening starts at the master that was on screen, not
// clip 1, and the end panel names what the review came to.
const revCode = [
  grab(/function resumeIndex\(clips, last, fallback\) \{[\s\S]*?\n\}/, "resumeIndex"),
  grab(/function markSummary\(marks\) \{[\s\S]*?\n\}/, "markSummary"),
].join("\n");
const rev = new Function(
  "plural",
  `${revCode}; return { resumeIndex, markSummary };`
)((n, w) => `${n} ${w}${n === 1 ? "" : "s"}`);
const clips = [{ master: "a" }, { master: "b" }, { master: "c" }];
check("review: reopening resumes the master left on screen", rev.resumeIndex(clips, "b", 0), 1);
check("review: an unremembered master falls back", rev.resumeIndex(clips, "gone", 2), 2);
check("review: a first open starts at the fallback", rev.resumeIndex(clips, null, 0), 0);
check("review: the end panel counts marks and clips", rev.markSummary([{ master: "a" }, { master: "a" }, { master: "b" }]), "3 marks in 2 clips");
check("review: one mark reads singular", rev.markSummary([{ master: "a" }]), "1 mark in 1 clip");
check("review: no marks say so", rev.markSummary([]), "End of the trip — nothing marked.");

// Deleting a trip must say what it erases and from where, and make you type the
// name when the cloud holds the only copy (#52). An archived trip has no local
// clips, and its confirm used to read "0 clips" over all of its footage. The
// real functions, with the dialog as a fake.
const GIB = 1073741824;
const delCode = [
  grab(/const HTML_ESCAPES = [\s\S]*?\};/, "HTML_ESCAPES"),
  ...["esc", "plural", "fmtBytes", "deleteTripPlan", "deleteSummary", "confirmDanger"].map((n) =>
    grab(new RegExp(`function ${n}\\([^)]*\\) \\{[\\s\\S]*?\\n\\}`), n)
  ),
].join("\n");
const fakeEl = () => {
  const on = {};
  return {
    hidden: false,
    disabled: false,
    value: "",
    open: false,
    focused: false,
    addEventListener: (t, f) => (on[t] ||= []).push(f),
    removeEventListener: (t, f) => (on[t] = (on[t] || []).filter((g) => g !== f)),
    fire(t, ev = {}) {
      for (const f of [...(on[t] || [])]) f(ev);
    },
    focus() {
      this.focused = true;
    },
    showModal() {
      this.open = true;
    },
    close() {
      this.open = false;
      this.fire("close");
    },
  };
};
const tripDelete = () => {
  const els = {};
  const $ = (sel) => (els[sel] ||= fakeEl());
  for (const id of ["#danger-go", "#danger-cancel", "#danger-type", "#danger-type-field"]) $(id);
  const api = new Function("$", "dangerDlg", `${delCode}; return { deleteTripPlan, deleteSummary, confirmDanger };`)(
    $,
    $("#danger-dialog")
  );
  return { ...api, els };
};
const settled = async (p) => {
  let out = "pending";
  p.then((v) => (out = v));
  await new Promise((r) => setImmediate(r));
  return out;
};
const td = tripDelete();

const archived = { masters: 0, bytes: 180 * 1048576, shared: true, cloudClips: 64, cloudBytes: 58 * GIB, onlyClips: 64, onlyBytes: 58 * GIB };
let plan = td.deleteTripPlan("kyoto", archived);
check("archived: names the only copy", plan.body.includes("only copy of 64 clips (58.0 GiB)"), true);
check("archived: says nothing is raw here", plan.body.includes("no raw clips here"), true);
check("archived: says where it is erased from", plan.body.includes("Also erased from the cloud: your 64 clips"), true);
check("archived: never claims 0 clips", plan.body.includes("0 clips"), false);
check("archived: the name must be typed", plan.typed, "kyoto");

const everywhere = { ...archived, masters: 12, onlyClips: 0, onlyBytes: 0, cloudClips: 12 };
plan = td.deleteTripPlan("bali", everywhere);
check("local copies too: cloud line, no only-copy claim", json([plan.body.includes("Also erased from the cloud"), plan.body.includes("only copy")]), json([true, false]));
check("local copies too: no typed name", plan.typed, null);
check("local copies too: counts the local clips", plan.body.includes("12 clips, its cut, and marks"), true);

plan = td.deleteTripPlan("bali", { masters: 3, bytes: GIB, shared: false, cloudClips: 0, cloudBytes: 0, onlyClips: 0, onlyBytes: 0 });
check("unshared: no cloud line", json([plan.body.includes("cloud"), plan.typed]), json([false, null]));
plan = td.deleteTripPlan("a", { ...archived, onlyClips: 1, onlyBytes: GIB, cloudClips: 1 });
check("one clip reads singular", plan.body.includes("only copy of 1 clip (1.0 GiB). It's in the cloud"), true);
plan = td.deleteTripPlan("<b>&x", archived);
check("the name is escaped in the body but typed raw", json([plan.body.includes("<b>"), plan.body.includes("&lt;b&gt;&amp;x"), plan.typed]), json([false, true, "<b>&x"]));

const sum = (r) => td.deleteSummary({ deleted: 0, bytes: 0, inCloud: 0, keptCloud: 0, cloudOk: true, ...r });
check("toast: archived trip leads with the cloud", sum({ bytes: 180 * 1048576, inCloud: 64 }), "Deleted the trip · 180 MiB · 64 clips from the cloud");
check("toast: ordinary delete is unchanged in shape", sum({ deleted: 3, bytes: GIB, inCloud: 3 }), "Deleted 3 clips · 1.0 GiB · 3 clips from the cloud");
check("toast: deleting nothing still says so", sum({}), "Deleted 0 clips");
check("toast: a bare archived trip shows no 0 MiB", sum({ bytes: 2048, inCloud: 3 }), "Deleted the trip · 3 clips from the cloud");
check("body: nor does the confirm", td.deleteTripPlan("k", { ...archived, bytes: 2048 }).body.includes("MiB)."), false);

// The gate itself: disarmed until the exact name is typed, and Enter or a click
// can't get around it.
let dd = tripDelete();
const go = dd.els["#danger-go"];
const box = dd.els["#danger-type"];
const key = (k) => {
  const ev = { key: k, prevented: false, preventDefault() { this.prevented = true; } };
  box.fire("keydown", ev);
  return ev;
};
const typeInto = (v) => {
  box.value = v;
  box.fire("input");
};
let ask = dd.confirmDanger({ body: "x", typeToConfirm: "kyoto" });
check("typed: the field shows, focused, and Delete starts disabled", json([dd.els["#danger-type-field"].hidden, box.focused, go.disabled]), json([false, true, true]));
check("typed: Enter on an empty box is swallowed", key("Enter").prevented, true);
go.fire("click");
check("typed: nor does a click get through", await settled(ask), "pending");
typeInto("kyot");
check("typed: a partial name stays disarmed", go.disabled, true);
typeInto("Kyoto");
check("typed: case matters", go.disabled, true);
typeInto(" kyoto ");
check("typed: the full name arms it", go.disabled, false);
go.fire("click");
check("typed: and then it resolves true", await settled(ask), true);

ask = dd.confirmDanger({ body: "x", typeToConfirm: "kyoto" });
typeInto("kyoto");
key("Enter");
check("typed: Enter confirms once it matches", await settled(ask), true);
ask = dd.confirmDanger({ body: "x", typeToConfirm: "kyoto" });
dd.els["#danger-cancel"].fire("click");
check("typed: Cancel resolves false", await settled(ask), false);

// ...and a plain confirm after a typed one must not inherit its gate.
ask = dd.confirmDanger({ body: "x" });
check("plain: no field, Delete enabled, Cancel focused", json([dd.els["#danger-type-field"].hidden, go.disabled, dd.els["#danger-cancel"].focused]), json([true, false, true]));
go.fire("click");
check("plain: one click confirms", await settled(ask), true);

console.log(fails ? `\n${fails} failed` : `\nall passed`);
process.exit(fails ? 1 : 0);
