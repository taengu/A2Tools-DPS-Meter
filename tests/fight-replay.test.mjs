import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

// The cut half of fightReplay.js (no window here, so its controls stay out).
const source = readFileSync(new URL("../public/src/js/fightReplay.js", import.meta.url), "utf8");
const context = vm.createContext({ module: { exports: {} } });
vm.runInContext(source, context);
const { buildReplayModel, cutReplayAt } = context.module.exports;

const skill = (actorId, code, dmg, hitTimestamps, extra = {}) => ({
  actorId, code, name: `S${code}`, time: hitTimestamps.length, dmg,
  multiHitCount: 0, multiHitDamage: 0, crit: 0, parry: 0, back: 0, perfect: 0, double: 0, regen: 0,
  hitTimestamps, ...extra,
});

const record = ({ heals = true } = {}) => ({
  id: "auto_1_2", bossName: "Boss", targetId: 9, startTimeMs: 0, durationMs: 10_000, totalDamage: 2_000,
  details: {
    targetId: 9, battleTime: 10_000, totalTargetDamage: 2_000,
    skills: [
      skill(1, 100, 1_000, [1_000, 2_000, 6_000, 9_000], { crit: 2, back: 4 }),
      // Out of order, as merged rows can be.
      skill(2, 200, 600, [8_000, 3_000, 5_000], { crit: 3 }),
      skill(2, 201, 300, [4_000, 7_000], { isDot: true }),
      // A row with no hit times counts at the end.
      skill(1, 300, 100, [], { time: 1 }),
    ],
    healSkills: [skill(1, 500, 400, heals ? [2_500, 7_500] : [])],
    deaths: [{ actorId: 2, tsMs: 4_000, revivedMs: 6_000 }, { actorId: 1, tsMs: 9_500 }],
  },
  actors: [],
});

const sum = (rows, key) => rows.reduce((s, r) => s + (Number(r[key]) || 0), 0);

test("Others has no invented replay timing and returns only at the full-fight endpoint", () => {
  const rec = { ...record(), worldBossObservedDamage: 10_000 };
  const before = JSON.stringify(rec);
  const model = buildReplayModel(rec);
  const partial = cutReplayAt(model, 5_000);
  assert.equal(partial.worldBossObservedDamage, null);
  assert.equal(partial.totalDamage, 1_050, "partial totals still come only from recorded hits");
  const full = cutReplayAt(model, 10_000);
  assert.equal(full.worldBossObservedDamage, 10_000);
  assert.equal(full.totalDamage, 2_000, "the estimate remains separate from raw replay totals");
  assert.equal(JSON.stringify(rec), before);
  assert.equal(cutReplayAt(model, 5_000).worldBossObservedDamage, null, "seeking back clears it again");
});

test("the end of a replay is the whole fight", () => {
  const rec = record();
  const model = buildReplayModel(rec);
  for (const at of [10_000, 12_000]) {
    const cut = cutReplayAt(model, at);
    assert.equal(cut.atEnd, true);
    assert.equal(cut.totalDamage, 2_000);
    assert.equal(cut.battleTime, 10_000);
    const same = (a, b) => assert.equal(JSON.stringify(a), JSON.stringify(b));
    same(cut.details.skills, rec.details.skills);
    same(cut.details.healSkills, rec.details.healSkills);
    same(cut.details.deaths, rec.details.deaths);
    assert.deepEqual({ ...cut.actorDamage }, { 1: 1_100, 2: 900 });
  }
});

test("halfway, hits are counted to the moment and their damage shared evenly", () => {
  const model = buildReplayModel(record());
  const cut = cutReplayAt(model, 5_000);
  const [a, b, dot] = cut.details.skills;
  assert.equal(cut.details.skills.length, 3, "the untimed row waits for the end");
  assert.deepEqual([a.time, a.dmg, a.crit, a.back], [2, 500, 1, 2]);
  assert.deepEqual([...a.hitTimestamps], [1_000, 2_000]);
  assert.deepEqual([b.time, b.dmg, b.crit], [2, 400, 2]);
  assert.deepEqual([...b.hitTimestamps], [3_000, 5_000]);
  assert.deepEqual([dot.time, dot.dmg], [1, 150]);
  assert.equal(cut.battleTime, 5_000);
  assert.deepEqual({ ...cut.actorDamage }, { 1: 500, 2: 550 });
  // Skill damage is all of the target's here, so the total is the cut's.
  assert.equal(cut.totalDamage, Math.round(2_000 * (1_050 / 2_000)));
  assert.deepEqual([sum(cut.details.healSkills, "dmg"), sum(cut.details.healSkills, "time")], [200, 1]);
  // Down at 4 s and not yet up again.
  assert.deepEqual(cut.details.deaths.map((d) => ({ ...d })), [{ actorId: 2, tsMs: 4_000 }]);
});

test("the cut never runs backwards, and starts empty", () => {
  const model = buildReplayModel(record());
  assert.equal(cutReplayAt(model, 0).details.skills.length, 0);
  assert.equal(cutReplayAt(model, 0).battleTime, 1_000, "DPS is over a second at least");
  let last = -1;
  for (let at = 0; at <= 10_000; at += 250) {
    const dmg = sum(cutReplayAt(model, at).details.skills, "dmg");
    assert.ok(dmg >= last, `damage fell at ${at}`);
    last = dmg;
  }
});

test("healing saved without times stays the whole fight's", () => {
  const model = buildReplayModel(record({ heals: false }));
  assert.equal(model.healTimed, false);
  assert.equal(sum(cutReplayAt(model, 1_000).details.healSkills, "dmg"), 400);
});

test("a fight saved without hit times cannot be replayed", () => {
  const rec = record();
  rec.details.skills.forEach((s) => { s.hitTimestamps = []; });
  assert.equal(buildReplayModel(rec).damageTimed, false);
});
