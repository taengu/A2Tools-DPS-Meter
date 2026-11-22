import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

// The DOM-free half of fightCompare.js.
const source = readFileSync(new URL("../public/src/js/fightCompare.js", import.meta.url), "utf8");
const context = vm.createContext({ module: { exports: {} } });
vm.runInContext(source, context);
const { buildCompareModel, cmpHeadToHead, cmpCompact, cmpActiveMs, cmpText } = context.module.exports;

const row = (actorId, code, dmg, time, extra = {}) => ({ actorId, code, name: `S${code}`, dmg, time, crit: 0, ...extra });
// `n` hits evenly from `from` ms, `step` apart.
const times = (n, from = 0, step = 1000) => Array.from({ length: n }, (_, i) => from + i * step);

const fight = (skills, actors, extra = {}) => ({
  id: "f", durationMs: 10_000, actors, details: { battleTime: 10_000, skills, ...extra },
});

test("members are ranked by damage with DPS and share", () => {
  const m = buildCompareModel(fight(
    [row(1, 10, 3000, 10, { crit: 4 }), row(2, 10, 6000, 20, { crit: 5 }), row(2, 11, 1000, 5)],
    [{ actorId: 1, nickname: "A", job: "Cleric" }, { actorId: 2, nickname: "B" }],
  ));
  assert.equal(m.members.map((x) => x.name).join(), "B,A");
  assert.equal(m.members[0].damage, 7000);
  assert.equal(m.members[0].dps, 700);
  assert.equal(m.members[0].hits, 25);
  assert.equal(m.members[0].flags.crit, 5);
  assert.equal(Math.round(m.members[0].share), 70);
  assert.equal(m.totalDamage, 10_000);
});

test("zero-damage rows and a record with no details do not break it", () => {
  assert.equal(buildCompareModel({}).members.length, 0);
  const m = buildCompareModel({ details: { skills: [row(1, 1, 0, 3)] }, actors: [] });
  assert.equal(m.members.length, 0);
});

test("pauses over 2.5 s are idle time, edges included", () => {
  assert.equal(cmpActiveMs([], 10_000), null);
  // Hits every second over the whole fight: no pause counts.
  assert.equal(cmpActiveMs(times(11, 0, 1000), 10_000), 10_000);
  // Hits only in the first 2 s: the 8 s after the last loses 8000 - 2500.
  assert.equal(cmpActiveMs([0, 1000, 2000], 10_000), 10_000 - 5500);
});

test("the factors split the gap exactly: uptime x pace x damage per hit", () => {
  const actors = [{ actorId: 1, nickname: "A", jobId: 11 }, { actorId: 2, nickname: "B", jobId: 11 }];
  // A: 10 hits of 300 over the whole fight. B: 5 hits of 200, all in the first 5 s.
  const m = buildCompareModel(fight([
    row(1, 10, 3000, 10, { hitTimestamps: times(10, 0, 1000) }),
    row(2, 10, 1000, 5, { hitTimestamps: times(5, 0, 1000) }),
  ], actors));
  const h = cmpHeadToHead(m, 1, 2);
  assert.equal(Math.round(h.gapPct), 200);
  assert.equal(h.factors.map((f) => f.key).join(), "uptime,pace,perHit");
  const sumX = h.factors.reduce((n, f) => n + f.x, 0);
  assert.ok(Math.abs(sumX - Math.log(3)) < 1e-9);
  assert.ok(Math.abs(h.factors.reduce((n, f) => n + f.weight, 0) - 1) < 1e-9);
  const perHit = h.factors.find((f) => f.key === "perHit");
  assert.equal(Math.round(perHit.effectPct), 50); // 300 vs 200
  assert.ok(h.factors.find((f) => f.key === "uptime").x > 0); // B stopped early
});

test("without hit times the split is hits x damage per hit", () => {
  const actors = [{ actorId: 1, nickname: "A" }, { actorId: 2, nickname: "B" }];
  const h = cmpHeadToHead(buildCompareModel(fight([row(1, 1, 400, 4), row(2, 2, 100, 2)], actors)), 1, 2);
  assert.equal(h.factors.map((f) => f.key).join(), "hits,perHit");
  assert.equal(h.sameClass, false);
  assert.equal(h.skills.a.length, 1);
  assert.equal(h.skills.b.length, 1);
});

test("same class: skills line up by code and the gap's skills say why", () => {
  const actors = [{ actorId: 1, nickname: "A", jobId: 11 }, { actorId: 2, nickname: "B", jobId: 11 }];
  const m = buildCompareModel(fight([
    row(1, 10, 1000, 10), row(2, 10, 500, 5), // more uses
    row(1, 11, 900, 3), row(2, 11, 300, 3), // harder hits
    row(1, 12, 800, 2), // B never used it
    row(2, 13, 50, 1), // B ahead here: not a driver of A's lead
  ], actors));
  const h = cmpHeadToHead(m, 1, 2);
  assert.equal(h.sameClass, true);
  assert.equal(h.skills.length, 4);
  const why = Object.fromEntries(h.drivers.map((d) => [d.key, d.cause]));
  assert.equal(why["10"], "hits");
  assert.equal(why["11"], "avg");
  assert.equal(why["12"], "unused");
  assert.equal(why["13"], undefined);
  // Swapped, B is behind: the same skills, from B's side.
  const back = cmpHeadToHead(m, 2, 1);
  assert.ok(back.gapPct < 0);
  assert.equal(back.drivers.length, 3);
  assert.equal(back.drivers[0].aheadIsA, false);
});

test("deaths add up the time down", () => {
  const actors = [{ actorId: 1, nickname: "A" }];
  const m = buildCompareModel(fight([row(1, 1, 100, 1)], actors, {
    deaths: [{ actorId: 1, tsMs: 2000, revivedMs: 5000 }, { actorId: 1, tsMs: 8000 }],
  }));
  assert.equal(m.members[0].deaths, 2);
  assert.equal(m.members[0].deadMs, 3000 + 2000);
});

test("picking the same player twice compares nothing", () => {
  const m = buildCompareModel(fight([row(1, 1, 100, 1)], [{ actorId: 1, nickname: "A" }]));
  assert.equal(cmpHeadToHead(m, 1, 1), null);
});

test("numbers are shown in the meter's k / m style", () => {
  assert.equal(cmpCompact(950), "950");
  assert.equal(cmpCompact(1234), "1.2k");
  assert.equal(cmpCompact(255_957_427), "256.0m");
  assert.equal(cmpCompact(2_500_000_000), "2.50b");
});

test("sentences fill their placeholders, in English without a translation", () => {
  const tx = cmpText();
  assert.equal(tx.button, "Compare");
  assert.equal(tx.ahead("A", "B", "12.0%"), "A out-damages B by 12.0%");
  assert.equal(tx.deaths("B", 2, 30), "B died 2x (30s down)");
  assert.equal(tx.unused("B"), "B never used it");
  assert.equal(tx.counter("B"), "B is ahead on");
});

test("a translation's sentences fill their placeholders too", () => {
  const ja = JSON.parse(readFileSync(new URL("../src/data/i18n/ui/ja.json", import.meta.url), "utf8"));
  const t = (key, fallback) => key.split(".").reduce((o, k) => o?.[k], ja) ?? fallback;
  const translated = vm.createContext({ module: { exports: {} }, window: { i18n: { t } } });
  vm.runInContext(source, translated);
  const tx = translated.module.exports.cmpText();
  assert.equal(tx.button, "比較");
  assert.equal(tx.ahead("A", "B", "12.0%"), "A が B を DPS で 12.0% 上回っています");
});
