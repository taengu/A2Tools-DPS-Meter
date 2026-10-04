import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/core.js", import.meta.url), "utf8");
const tick = () => new Promise((resolve) => setImmediate(resolve));

function setup(getBattleDetail) {
  const logs = [];
  const window = { addEventListener() {}, dpsData: { getBattleDetail }, javaBridge: { logToDebug: (s) => logs.push(s) } };
  const context = vm.createContext({ window, console, document: { readyState: "loading", addEventListener() {} } });
  vm.runInContext(source, context);
  const app = vm.runInContext("Object.create(DpsApp.prototype)", context);
  app.dpsFormatter = new Intl.NumberFormat("en-US");
  app.elList = { querySelector: () => ({}) };
  app.hoveredDetailsRowId = 1;
  app.hoverTooltipCacheByRowId = new Map();
  app.hoverTooltipPendingRowIds = new Set();
  app.hoverTooltipRequestSeqByRowId = new Map();
  const rendered = [];
  app.renderHoverTooltip = (details) => rendered.push(details);
  return { app, rendered, logs };
}

test("hover replaces loading with the player's highest-damage skills", async () => {
  const { app, rendered } = setup(async () => JSON.stringify({
    battleTime: 10000,
    skills: Array.from({ length: 7 }, (_, i) => ({ code: 18010000 + i * 10000, name: `Skill ${i}`, dmg: 100 * (i + 1), time: 1, actorId: 1 })),
  }));
  app.applyHoverTooltip({ id: 1 }, { forceRefresh: true });
  assert.equal(rendered[0].state, "loading");
  await tick();
  assert.equal(rendered.at(-1).skills.length, 5);
  assert.equal(rendered.at(-1).skills[0].dmg, 700);
  assert.notEqual(rendered.at(-1).state, "loading");
});

test("an empty response does not leave a perpetual loading state", async () => {
  const { app, rendered } = setup(async () => null);
  app.applyHoverTooltip({ id: 1 }, { forceRefresh: true });
  await tick();
  assert.equal(rendered.at(-1).skills.length, 0);
  assert.notEqual(rendered.at(-1).state, "loading");
});

test("request failures are visible and logged instead of masquerading as loading", async () => {
  const { app, rendered, logs } = setup(async () => { throw new Error("IPC failed"); });
  app.applyHoverTooltip({ id: 1 }, { forceRefresh: true });
  await tick();
  assert.equal(rendered.at(-1).state, "error");
  assert.match(logs[0], /IPC failed/);
});

test("rendered tooltip text distinguishes loading, empty data and errors", () => {
  const { app } = setup();
  app.hoverTooltipEl = { style: {}, classList: { add() {} }, offsetWidth: 100, offsetHeight: 100 };
  const render = Object.getPrototypeOf(app).renderHoverTooltip;
  const row = { id: 1, name: "Test", dps: 100, totalDamage: 1000 };
  for (const [state, text] of [["loading", "Loading..."], ["empty", "No skill data for this fight"], ["error", "Could not load skills"]]) {
    render.call(app, { skills: [], state }, row, {});
    assert.ok(app.hoverTooltipEl.innerHTML.includes(text));
    if (state !== "loading") assert.ok(!app.hoverTooltipEl.innerHTML.includes("Loading..."));
  }
});
