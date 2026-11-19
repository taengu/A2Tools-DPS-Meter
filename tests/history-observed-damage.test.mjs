import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/history.js", import.meta.url), "utf8");
const context = vm.createContext({});
vm.runInContext(`${source}\nthis.helpers = { historyTotalDamage, historyNearbyDps, historyDamageShare };`, context);
const { historyTotalDamage, historyNearbyDps, historyDamageShare } = context.helpers;

test("History includes observed Others without adding the recorded strangers twice", () => {
  const fight = { totalDamage: 1500, myDamage: 1000, durationMs: 6000, worldBossObservedDamage: 500500 };
  const before = structuredClone(fight);
  assert.equal(historyTotalDamage(fight), 500500);
  assert.equal(historyNearbyDps(fight), 500500 / 6);
  assert.equal(historyDamageShare(fight), 0);
  assert.deepEqual(fight, before, "raw and personal damage remain unchanged");
});

test("Nearby sorting uses the aggregate while older fights retain recorded totals", () => {
  const recorded = { totalDamage: 10000, myDamage: 5000, durationMs: 1000 };
  const observed = { totalDamage: 1000, durationMs: 1000, worldBossObservedDamage: 20000 };
  assert.equal(historyTotalDamage(recorded), 10000);
  assert.equal(historyDamageShare(recorded), 50);
  assert.deepEqual([recorded, observed].sort((a, b) => historyNearbyDps(b) - historyNearbyDps(a)), [observed, recorded]);
});

test("a low or invalid HP estimate never removes recorded damage", () => {
  for (const value of [undefined, null, -100, 0, 50, NaN, Infinity, "invalid"]) {
    const fight = { totalDamage: 100, myDamage: 25, durationMs: 1000, worldBossObservedDamage: value };
    assert.equal(historyTotalDamage(fight), 100);
    assert.equal(historyDamageShare(fight), 25);
  }
  assert.equal(historyNearbyDps({ totalDamage: 100, durationMs: 0, worldBossObservedDamage: 200 }), 0);
});
