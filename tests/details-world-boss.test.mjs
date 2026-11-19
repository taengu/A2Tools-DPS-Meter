import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/details.js", import.meta.url), "utf8");
const section = (start, end) => {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from);
  assert.ok(from >= 0 && to > from);
  return source.slice(from, to);
};
const target = (observed = 10_000) => ({ targetId: 9, totalDamage: 2_000,
  worldBossObservedDamage: observed, battleTime: 10_000, actorDamage: { 1: 1_100, 2: 900 } });

function barStats(targets, { mode = "dmg", selected = 9 } = {}) {
  const context = vm.createContext({ detailsTargets: targets, selectedTargetId: selected, detailsMode: mode,
    detectedJobByActorId: new Map(), detailsActors: new Map(), lastDetails: { battleTime: 10_000 },
    lastUnfilteredDetails: { healSkills: [{ actorId: 1, dmg: 100 }, { actorId: 2, dmg: 300 }] },
    getTargetById: (id) => targets.find((t) => t.targetId === id) });
  vm.runInContext(source, context);
  vm.runInContext(section("  const buildPartyBarStats =", "  const reducedMotion ="), context);
  return vm.runInContext("buildPartyBarStats()", context);
}

test("full-fight bars include unattributed Others without duplicating recorded strangers", () => {
  const fight = target();
  const before = JSON.stringify(fight);
  const result = barStats([fight]);
  const others = result.stats.find((s) => s.isOthers);
  assert.equal(others.totalDmg, 8_000);
  assert.equal(others.contributionPct, 80);
  assert.equal(others.actorId, undefined, "the estimate invents no player identity");
  assert.equal(result.stats.find((s) => s.actorId === 1).contributionPct, 11);
  assert.equal(result.stats.reduce((sum, s) => sum + s.totalDmg, 0), 10_000);
  assert.equal(result.stats.reduce((sum, s) => sum + s.contributionPct, 0), 100);
  assert.equal(JSON.stringify(fight), before, "recorded actor totals stay untouched");
});

test("old fights, invalid estimates and healing retain their recorded totals", () => {
  for (const observed of [undefined, null, -1, NaN, Infinity, 1_000]) {
    const result = barStats([{ ...target(), worldBossObservedDamage: observed }]);
    assert.equal(result.stats.length, 2);
    assert.equal(result.stats[0].totalDmg, 1_100);
    assert.ok(Math.abs(result.stats[0].contributionPct - 55) < 1e-9);
  }
  const heal = barStats([target()], { mode: "heal" });
  assert.equal(heal.stats.reduce((sum, s) => sum + s.totalDmg, 0), 400);
  assert.equal(heal.stats[0].contributionPct, 75);
  assert.ok(heal.stats.every((s) => !s.isOthers));
});

test("a party fight needs no local damage row, and aggregates respect target selection", () => {
  const first = target();
  first.actorDamage = {};
  assert.equal(barStats([first]).stats[0].totalDmg, 10_000);
  const second = { targetId: 10, totalDamage: 300, actorDamage: { 2: 300 }, battleTime: 10_000 };
  const combined = barStats([target(), second], { selected: null });
  assert.equal(combined.stats.reduce((sum, s) => sum + s.totalDmg, 0), 10_300);
  const selected = barStats([target(), second], { selected: 10 });
  assert.equal(selected.stats.length, 1);
  assert.equal(selected.stats[0].contributionPct, 100);
});

const element = () => ({ style: {}, children: [], textContent: "",
  set innerHTML(value) { this.children = []; }, appendChild(child) { this.children.push(child); } });
const statKeys = ["totalDamage", "contribution", "avgDps", "critRate"];
const valueOf = (key, data) => String({ totalDamage: data.totalDmg, contribution: data.contributionPct,
  avgDps: data.totalDmg / 10, critRate: data.totalCritPct }[key]);

function statsView(selected, observed = 10_000, mode = "dmg") {
  const whole = { totalDmg: 2_000, contributionPct: 100, totalCritPct: 20 };
  const player = { totalDmg: 1_100, contributionPct: 55, totalCritPct: 25 };
  const slots = statKeys.map(() => ({ statEl: element(), labelEl: element(), valueEl: element() }));
  const defs = statKeys.map((key) => ({ key: `details.stats.${key}`, getValue: (data) => valueOf(key, data) }));
  const context = vm.createContext({ detailsMode: mode, selectedAttackerIds: selected ? [1] : null,
    selectedTargetId: 9, detailsTargets: [target(observed)], lastUnfilteredDetails: whole,
    getJobColor: () => "", getActorJob: () => "", STATUS: defs, HEAL_STATUS: defs, statSlots: slots,
    COMPACT_STAT_KEYS: new Set(), labelText: (_, fallback) => fallback,
    document: { createElement: element }, resolveStatValue: (key, data) => valueOf(key.split(".").at(-1), data) });
  vm.runInContext(source, context);
  vm.runInContext(section("  const renderStats =", "  // Build party bar stats"), context);
  const data = selected ? player : whole;
  const before = JSON.stringify([player, whole]);
  vm.runInContext("renderStats", context)(data);
  assert.equal(JSON.stringify([player, whole]), before, "displaying Others does not rewrite skill totals");
  return slots.map((slot) => selected ? slot.valueEl.children.map((child) => child.textContent) : slot.valueEl.textContent);
}

test("the selected player's numerator stays recorded while whole-fight stats include Others", () => {
  const values = statsView(true);
  assert.deepEqual(values[0], ["1100", " / ", "10000"]);
  assert.deepEqual(values[1], ["11", " / ", "100"]);
  assert.deepEqual(values[2], ["110", " / ", "1000"]);
  assert.deepEqual(values[3], ["25", " / ", "20"], "critical rates still describe recorded hits");
  assert.deepEqual(statsView(false), ["10000", "100", "1000", "20"]);
  assert.deepEqual(statsView(false, null), ["2000", "100", "200", "20"]);
  assert.deepEqual(statsView(false, 10_000, "heal"), ["2000", "100", "200", "20"]);
});

test("clicking Others does not select a fake actor or reset the player's selection", async () => {
  let click;
  let redraws = 0;
  class Element { closest() { return this; } }
  const context = vm.createContext({ Element,
    detailsPartyListEl: { addEventListener: (_, fn) => { click = fn; }, contains: () => true },
    selectedAttackerIds: [1], buildPartyBarStats: () => { redraws++; }, refreshDetailsView: () => { redraws++; } });
  vm.runInContext(section('  detailsPartyListEl?.addEventListener("click"', "  // Hit results"), context);
  const others = new Element();
  others.dataset = { others: "1" };
  await click({ target: others });
  assert.deepEqual(context.selectedAttackerIds, [1]);
  assert.equal(redraws, 0);
});

test("Others renders beside eight real players without a local highlight, icon or invented peak", () => {
  const node = () => ({ ...element(), dataset: {},
    style: { setProperty() {} }, classList: { toggle() {} } });
  const list = node();
  const context = vm.createContext({ detailsPartyListEl: list, detailsPanel: { classList: { contains: () => false } },
    fillBarsFromZero: false, reducedMotion: () => true, showPeak: true, selectedAttackerIds: null,
    lastDetails: null, lastUnfilteredDetails: null, lastBarRatio: new Map(), barSteps: null,
    window: { _dpsApp: { localPlayerId: null } }, document: { createElement: node },
    detailsActors: new Map(), getActorJob: () => "", getJobColor: () => "", cjkRegex: /[\u3400-\u9FFF]/,
    labelText: (_, fallback) => fallback, resolveActorLabel: (id) => `Player ${id}`,
    hitSpans: () => new Map(), hitsOf: () => [], peakOf: () => 0, partyStatText: () => "", getPartyStat: () => "off",
    formatDamageCompact: (n) => String(n) });
  vm.runInContext(section("  const renderPartyBars =", "  // One click listener"), context);
  const stats = [{ isOthers: true, totalDmg: 100, contributionPct: 50 },
    ...Array.from({ length: 9 }, (_, i) => ({ actorId: i + 1, totalDmg: 9 - i, contributionPct: 1 }))];
  vm.runInContext("renderPartyBars", context)(stats, 10_000);
  assert.equal(list.children.length, 10, "All, eight players and the aggregate");
  const others = list.children.find((bar) => bar.dataset.others);
  assert.ok(others);
  assert.ok(!others.className.includes("isUser"), "unknown local identity is not Others");
  assert.equal(others.dataset.actorId, undefined);
  assert.equal(others.style.cursor, "default");
  const content = others.children[1];
  assert.ok(content.children.every((child) => child.className !== "detailsPartyBarIcon"));
  const dps = content.children.find((child) => child.className === "detailsPartyBarDps");
  assert.equal(dps.children.length, 0, "no peak from nonexistent hits");
  assert.equal(dps.title, undefined, "no invented active-time tooltip");
});
