import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

function app(userName) {
  const window = { addEventListener() {} };
  const context = vm.createContext({ window, console, document: { readyState: "loading", addEventListener() {} } });
  vm.runInContext(readFileSync(new URL("../public/src/js/core.js", import.meta.url), "utf8"), context);
  const dps = vm.runInContext("Object.create(DpsApp.prototype)", context);
  dps.USER_NAME = userName;
  return dps;
}

const row = (nickname, dps, job = "Assassin") => ({ nickname, job, dps, amount: dps * 10, damageContribution: 0 });

test("your row is yours by the backend's id while it has no name", () => {
  // Strangers outdamaging you at the dummies, and your own row still "#2737".
  const payload = JSON.stringify({
    localPlayerId: 2737,
    map: {
      2737: row("2737", 10),
      101: row("Aa", 90),
      102: row("Bb", 80),
    },
  });
  const { rows } = app("Mine").buildRowsFromPayload(payload);
  const mine = rows.find((r) => r.id === "2737");
  assert.equal(mine.isUser, true);
  assert.equal(mine.isIdentifying, true);
  assert.deepEqual(Array.from(rows.filter((r) => r.isUser), (r) => r.id), ["2737"]);
});

test("by name as before, and nobody's without either", () => {
  const map = { 5: row("Mine", 10), 6: row("Other", 20) };
  const named = app("Mine").buildRowsFromMapObject(map);
  assert.deepEqual(Array.from(named.filter((r) => r.isUser), (r) => r.id), ["5"]);
  const unknown = app("").buildRowsFromMapObject(map, null);
  assert.equal(unknown.some((r) => r.isUser), false);
});

test("one metric sorts, others ride along as columns; stagger always offered", () => {
  const dps = app("Mine");
  dps.updateDisplayToggleLabel = () => {};
  dps.extraMetrics = [];
  assert.deepEqual(Array.from(dps.displayModes()), ["totalDamage", "damageReceived", "hps", "totalHeal", "stagger"]);
  const rows = dps.buildRowsFromMapObject({ 5: { ...row("Mine", 10), stagger: 35 }, 6: row("Other", 20) });
  assert.deepEqual(Array.from(rows, (r) => r.stagger).sort(), [0, 35]);
  dps.setDisplayMode("stagger");
  assert.equal(dps.displayMode, "stagger");
  const mine = rows.find((r) => r.id === "5");
  const metric = dps.getMetricForRow(mine);
  assert.deepEqual([metric.value, metric.text, metric.extras.length], [35, "35", 0]);
  // Extras keep the menu's order and never repeat the primary.
  dps.dpsFormatter = new Intl.NumberFormat("en-US");
  dps.extraMetrics = ["stagger", "totalDamage", "dps"];
  dps.setDisplayMode("stagger");
  assert.deepEqual(Array.from(dps.extraMetrics), ["dps", "totalDamage"]);
  assert.equal(dps.getMetricForRow(mine).extras.length, 2);
  // Healing is always offered (its Settings switch is gone).
  dps.extraMetrics = ["hps"];
  dps.setDisplayMode("totalHeal");
  assert.equal(dps.displayMode, "totalHeal");
  assert.deepEqual(Array.from(dps.extraMetrics), ["hps"]);
  // The old "both" no longer exists as a mode.
  dps.setDisplayMode("both");
  assert.equal(dps.displayMode, "dps");
});

test("damage taken is available as a live row metric", () => {
  const dps = app("Mine");
  const [mine] = dps.buildRowsFromMapObject({ 5: { ...row("Mine", 10), damageReceived: 12345 } });
  assert.equal(mine.damageReceived, 12345);
  dps.formatAbbreviatedNumber = (value) => String(value);
  const metric = dps.metricFor(mine, "damageReceived");
  assert.deepEqual([metric.value, metric.text], [12345, "12345"]);
});

test("in Stagger mode a row's tooltip lists its stagger by skill, most first", () => {
  const dps = app("Mine");
  const names = { 12090000: "Punishment", 12040000: "Pummel", 12120000: "Taunt" };
  dps.i18n = { getSkillName: (code, fallback) => names[code] ?? fallback, t: (_, fallback) => fallback };
  const [mine] = dps.buildRowsFromMapObject({
    5: { ...row("Mine", 10), stagger: 70, staggerSkills: { 12040000: 15, 12090000: 35, 12120000: 20 } },
  });
  const details = dps.staggerTooltipDetails(mine);
  assert.equal(details.mode, "stagger");
  assert.deepEqual(Array.from(details.skills, (s) => [s.name, s.dmg, s.code]), [
    ["Punishment", 35, 12090000],
    ["Taunt", 20, 12120000],
    ["Pummel", 15, 12040000],
  ]);
});
