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

console.log(fails ? `\n${fails} failed` : `\nall passed`);
process.exit(fails ? 1 : 0);
