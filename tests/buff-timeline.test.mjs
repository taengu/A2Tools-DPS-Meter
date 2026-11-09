import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/buffTimeline.js", import.meta.url), "utf8");
const context = vm.createContext({ window: {} });
vm.runInContext(source, context);
const get = (name) => vm.runInContext(name, context);
// Values made in the vm's realm compare by structure only after a round trip.
const plain = (value) => JSON.parse(JSON.stringify(value));

test("segments read back from the saved string, skipping anything malformed", () => {
  const parse = get("parseBuffSegments");
  assert.deepEqual(plain(parse("-500,1000,1,0;1000,4000,2,1")), [
    { start: -500, end: 1000, stacks: 1, how: 0 },
    { start: 1000, end: 4000, stacks: 2, how: 1 },
  ]);
  assert.deepEqual(plain(parse("1,2,3;x,1,1,1;5,4,1,1;7,9,1,2")), [{ start: 7, end: 9, stacks: 1, how: 2 }]);
  assert.deepEqual(plain(parse("")), []);
  assert.deepEqual(plain(parse(undefined)), []);
});

test("pieces split by a stack change join into one application", () => {
  const join = get("joinBuffApplications");
  const { apps, appOf } = join([
    { start: 0, end: 1000, stacks: 1, how: 0 },
    { start: 1000, end: 3000, stacks: 3, how: 8 },
    { start: 5000, end: 6000, stacks: 1, how: 1 },
  ]);
  assert.deepEqual(plain(apps), [
    { start: 0, end: 3000, how: 8, maxStacks: 3 },
    { start: 5000, end: 6000, how: 1, maxStacks: 1 },
  ]);
  assert.deepEqual(plain(appOf), [0, 0, 1]);
});

test("bars are placed in percent of the fight and cut to it", () => {
  const layout = get("layoutBuffSegments");
  const bars = layout([
    { start: -2000, end: 1000, stacks: 1, how: 0 },
    { start: 1000, end: 2500, stacks: 2, how: 1 },
    { start: 9000, end: 10000, stacks: 1, how: 10 },
    { start: 12000, end: 13000, stacks: 1, how: 1 },
  ], 10000);
  assert.deepEqual(plain(bars.map((b) => [b.left, b.width, b.stacks])), [[0, 10, 1], [10, 15, 2], [90, 10, 1]]);
  // The first two are one application, from before the pull.
  assert.deepEqual(plain(bars[0].app), { start: -2000, end: 2500, how: 1, maxStacks: 2 });
  assert.equal(bars[0].app, bars[1].app);
  assert.deepEqual(plain(layout([{ start: 0, end: 1, stacks: 1, how: 1 }], 0)), []);
});

test("uptime and fight times read the way the section shows them", () => {
  const uptime = get("formatBuffUptime");
  assert.equal(uptime(30000, 60000), "50%");
  assert.equal(uptime(60000, 60000), "100%");
  assert.equal(uptime(70000, 60000), "100%");
  assert.equal(uptime(100, 60000), "<1%");
  assert.equal(uptime(0, 60000), "0%");
  assert.equal(uptime(10, 0), "-");
  const time = get("formatBuffTime");
  assert.equal(time(65300), "1:05");
  assert.equal(time(65300, { tenths: true }), "1:05.3");
  assert.equal(time(-4200, { tenths: true }), "-0:04.2");
  assert.equal(time(0), "0:00");
});

test("the time axis takes a round step", () => {
  const ticks = get("buffAxisTicks");
  assert.deepEqual(plain(ticks(60000).map((t) => t.ms / 1000)), [0, 10, 20, 30, 40, 50, 60]);
  assert.deepEqual(plain(ticks(600000).map((t) => t.ms / 1000)), [0, 120, 240, 360, 480, 600]);
  assert.deepEqual(plain(ticks(0)), []);
});

test("rows: the player's buffs, their passives and the target's debuffs, filtered by caster", () => {
  const select = get("selectBuffRows");
  const PLAYER = 10;
  const HEALER = 20;
  const BOSS = 900;
  const tracks = [
    { on: PLAYER, id: 1, by: HEALER, segs: "5000,9000,1,1", up: 4000 },
    { on: PLAYER, id: 2, by: PLAYER, segs: "1000,30000,1,10", up: 29000 },
    { on: PLAYER, id: 3, by: BOSS, segs: "2000,3000,1,1", up: 1000 },
    { on: PLAYER, id: 4, by: PLAYER, passive: true, up: 60000 },
    { on: HEALER, id: 5, by: HEALER, segs: "0,1000,1,1", up: 1000 },
    { on: BOSS, id: 6, by: PLAYER, segs: "0,5000,3,4", up: 5000 },
    { on: BOSS, id: 7, by: BOSS, segs: "0,60000,1,10", up: 60000 },
  ];
  const base = { targetId: BOSS, playerId: PLAYER, actorIds: [PLAYER, HEALER] };
  const ids = (rows) => plain(rows.map((r) => r.id));

  let rows = select(tracks, base);
  assert.deepEqual(ids(rows.player), [2, 1, 3], "by uptime");
  assert.deepEqual(ids(rows.passives), [], "passives hidden by default");
  assert.equal(rows.passiveCount, 1);
  assert.deepEqual(ids(rows.target), [7, 6]);

  rows = select(tracks, { ...base, sort: "first", hidePassives: false });
  assert.deepEqual(ids(rows.player), [2, 3, 1], "by first applied");
  assert.deepEqual(ids(rows.passives), [4]);

  rows = select(tracks, { ...base, caster: "party" });
  assert.deepEqual(ids(rows.player), [2, 1]);
  assert.deepEqual(ids(rows.target), [6]);

  rows = select(tracks, { ...base, caster: "self" });
  assert.deepEqual(ids(rows.player), [2]);
  assert.deepEqual(ids(rows.target), [6]);

  // No player chosen: only the target's.
  rows = select(tracks, { ...base, playerId: null });
  assert.deepEqual(ids(rows.player), []);
  assert.deepEqual(ids(rows.target), [7, 6]);
});

test("buffs the game never shows are left out (Dodge's 3015)", () => {
  const select = get("selectBuffRows");
  const tracks = [
    { on: 10, id: 3015, by: 10, skill: 18000100, segs: "8700,11700,1,1", up: 3000 },
    { on: 10, id: 20112011, by: 10, segs: "0,30000,1,1", up: 30000 },
  ];
  const base = { targetId: 900, playerId: 10, actorIds: [10] };
  assert.deepEqual(plain(select(tracks, base).player.map((r) => r.id)), [20112011, 3015]);
  const hidden = new Set(["3015"]);
  assert.deepEqual(plain(select(tracks, { ...base, hidden }).player.map((r) => r.id)), [20112011]);
  const table = JSON.parse(readFileSync(new URL("../src/data/abnormals.json", import.meta.url), "utf8"));
  assert.ok(table.hidden.includes(3015), "the shipped table hides it");
  assert.ok(!table.hidden.includes(20112011), "the mana potion shows");
});

test("every UI language has the Buffs strings, with the same placeholders", () => {
  const read = (lang) => JSON.parse(readFileSync(new URL(`../src/data/i18n/ui/${lang}.json`, import.meta.url), "utf8"));
  const flatten = (obj, prefix = "") => Object.entries(obj).flatMap(([k, v]) =>
    v && typeof v === "object" ? flatten(v, `${prefix}${k}.`) : [[`${prefix}${k}`, v]]);
  const en = Object.fromEntries(flatten(read("en").details.buffs));
  const placeholders = (s) => [...String(s).matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
  // Every reason a segment can end with has a label.
  const endKeys = plain(get("BUFF_END_KEYS"));
  for (const key of endKeys) assert.ok(en[`end.${key}`], key);
  for (const lang of ["de", "en", "es", "fr", "ja", "ko", "pt", "ru", "zh-Hans", "zh-Hant"]) {
    const strings = Object.fromEntries(flatten(read(lang).details.buffs));
    assert.deepEqual(Object.keys(strings).sort(), Object.keys(en).sort(), lang);
    for (const [key, value] of Object.entries(strings)) {
      assert.ok(typeof value === "string" && value.trim(), `${lang} ${key}`);
      assert.deepEqual(placeholders(value), placeholders(en[key]), `${lang} ${key}`);
    }
  }
});

test("a buff's icon is its own, then its skill's, then a dot", () => {
  const iconSource = readFileSync(new URL("../public/src/js/skillIcons.js", import.meta.url), "utf8");
  const iconMap = readFileSync(new URL("../src/data/skill_icons.json", import.meta.url), "utf8");
  const window = { javaBridge: { readResource: () => iconMap } };
  vm.runInNewContext(iconSource, { window, console });
  const candidates = window.skillIcons.getAbnormalIconCandidates("ICON_Abnormal_DotHp", 18770000);
  assert.equal(candidates[0], "https://assets.playnccdn.com/static-aion2-gamedata/resources/ICON_Abnormal_DotHp.png");
  assert.match(candidates[1], /ICON_CH_SKILL/);
  assert.match(candidates.at(-1), /^data:image\/svg\+xml,.*circle/);
  const bare = window.skillIcons.getAbnormalIconCandidates(undefined, 0);
  assert.equal(bare.length, 1);
  assert.match(bare[0], /circle/);
  // A name that is not a plain file name is not put into a URL.
  assert.equal(window.skillIcons.getAbnormalIconCandidates("../x", 0).length, 1);
});

test("one row per buff name and caster: same-name ids merge, on wherever either was", () => {
  const merge = get("mergeSameNameTracks");
  const tracks = [
    { id: 181600101, by: 7, on: 7, skill: 18160010, up: 6000, segs: "0,4000,1,0;4000,6000,2,3" },
    { id: 181600111, by: 7, on: 7, skill: 18160011, up: 3000, segs: "5000,8000,1,1" },
    { id: 999, by: 7, on: 7, up: 1000, segs: "0,1000,1,1" },
    { id: 181600111, by: 8, on: 7, up: 1000, segs: "0,1000,1,1" },
  ];
  const names = { 181600101: "Sprint Mantra", 181600111: "Sprint Mantra", 999: "Other" };
  const rows = plain(merge(tracks, (t) => names[t.id], 10000));
  assert.equal(rows.length, 3, "the two Sprint Mantras from caster 7 merge; caster 8's stays apart");
  const sprint = rows.find((r) => r.mergedIds);
  assert.equal(sprint.id, 181600101, "keeps the id of the one on longest");
  assert.deepEqual(sprint.mergedIds, [181600101, 181600111]);
  assert.deepEqual(sprint._segments, [
    { start: 0, end: 4000, stacks: 1, how: 0 },
    { start: 4000, end: 6000, stacks: 2, how: 0 },
    { start: 6000, end: 8000, stacks: 1, how: 1 },
  ]);
  assert.equal(sprint.up, 8000);
});
