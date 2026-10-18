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

console.log(fails ? `\n${fails} failed` : `\nall passed`);
process.exit(fails ? 1 : 0);
