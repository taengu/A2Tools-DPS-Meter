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
  return { app, rendered, logs, window };
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

const bridgeSource = readFileSync(new URL("../public/src/js/tauriBridge.js", import.meta.url), "utf8");
const battleDetailMethod = bridgeSource.slice(bridgeSource.indexOf("    async getBattleDetail(actorId) {"), bridgeSource.indexOf("\n    getVersion()"));

function bridge(snapshot, responses) {
  const calls = [];
  const context = vm.createContext({
    cachedDpsJson: JSON.stringify(snapshot), lastSkillDetailsIssue: "", window: {},
    invoke: async (command, args) => {
      assert.equal(command, "get_skill_details");
      calls.push(args);
      return responses[args.targetId];
    },
  });
  const api = vm.runInContext(`({${battleDetailMethod}})`, context);
  return { api, calls };
}

test("hover queries the retained fight when the active target is zero", async () => {
  const { api, calls } = bridge({ targetId: 0, detailTargetIds: [42] }, {
    42: { skills: [{ code: 11010000, actorId: 1, dmg: 500, time: 1 }] },
  });
  const { app, rendered } = setup((id) => api.getBattleDetail(id));
  app.applyHoverTooltip({ id: 1 }, { forceRefresh: true });
  await tick();
  assert.equal(calls[0].targetId, 42);
  assert.equal(calls[0].actorIds[0], 1);
  assert.equal(rendered.at(-1).skills[0].dmg, 500);
});

test("multi-target hover combines repeated skills and keeps damage-over-time separate", async () => {
  const skill = { code: 11010000, actorId: 1, dmg: 500, time: 1, minDmg: 500, maxDmg: 500, hitTimestamps: [0], specs: [true] };
  const { api, calls } = bridge({ targetId: 0, detailTargetIds: [42, 43, 42], battleTime: 2000 }, {
    42: { startTime: 1000, totalTargetDamage: 500, skills: [skill] },
    43: { startTime: 2000, totalTargetDamage: 1000, skills: [{ ...skill, dmg: 700, minDmg: 700, maxDmg: 700 }, { ...skill, dmg: 300, isDot: true }] },
  });
  const detail = JSON.parse(await api.getBattleDetail(1));
  assert.equal(calls.length, 2);
  assert.equal(detail.skills.length, 2);
  assert.equal(detail.skills[0].dmg, 1200);
  assert.equal(detail.skills[0].time, 2);
  assert.equal(detail.skills[0].minDmg, 500);
  assert.equal(detail.skills[0].maxDmg, 700);
  assert.deepEqual(detail.skills[0].hitTimestamps, [0, 1000]);
  assert.equal(detail.totalTargetDamage, 1500);
  assert.equal(detail.battleTime, 2000);
});

test("reset snapshots do not query an old target and legacy snapshots still work", async () => {
  const reset = bridge({ targetId: 0, detailTargetIds: [] }, {});
  assert.equal(await reset.api.getBattleDetail(1), null);
  assert.equal(reset.calls.length, 0);
  const legacy = bridge({ targetId: 42 }, { 42: { skills: [] } });
  await legacy.api.getBattleDetail(1);
  assert.equal(legacy.calls[0].targetId, 42);
});


test("tooltip follows pointer coordinates relative to its container and flips at screen edges", () => {
  const { app, window } = setup();
  window.screen = { availLeft: -1280, availTop: 0, availWidth: 1280, availHeight: 720 };
  window.screenX = -700;
  window.screenY = 100;
  let updates = 0;
  window.javaBridge.updateOverlaySize = () => updates++;
  app.hoverTooltipEl = {
    style: {}, offsetWidth: 240, offsetHeight: 180,
    offsetParent: { getBoundingClientRect: () => ({ left: 10, top: 5 }) },
  };
  app.hoverMousePos = { x: 100, y: 80 };
  app.positionHoverTooltip();
  assert.equal(app.hoverTooltipEl.style.left, "102px");
  assert.equal(app.hoverTooltipEl.style.top, "87px");
  app.hoverMousePos = { x: 550, y: 550 };
  app.positionHoverTooltip();
  assert.equal(app.hoverTooltipEl.style.left, "288px");
  assert.equal(app.hoverTooltipEl.style.top, "353px");
  assert.equal(updates, 2);
});

test("moving over the same row repositions the tooltip without fetching skills again", () => {
  const { app } = setup();
  app.pinnedDetailsRowId = null;
  app.shouldSuppressRowInteractions = () => false;
  app.hoverTooltipEl = { classList: { contains: () => true } };
  let positions = 0;
  app.positionHoverTooltip = () => positions++;
  app.applyHoverTooltip = () => assert.fail("unexpected refetch");
  app.openHoverDetailsRow({ id: 1 }, { clientX: 120, clientY: 80 });
  assert.equal(app.hoverMousePos.x, 120);
  assert.equal(app.hoverMousePos.y, 80);
  assert.equal(positions, 1);
});

const sizingSource = bridgeSource.slice(bridgeSource.indexOf("  const updateWindowSize = () => {"), bridgeSource.indexOf("  // Watch all class changes"));
test("overlay reserves the tooltip's actual width and height and shrinks on close", () => {
  let tooltip = { getBoundingClientRect: () => ({ right: 610, bottom: 360 }) };
  let fullPanel = false;
  const sizes = [];
  const context = vm.createContext({
    resizeActive: false, lastSizeKey: "", PANEL_WIDTH: 1200, PANEL_HEIGHT: 800, PROMO_WIDTH: 600, PROMO_HEIGHT: 400,
    spaceRightBelow: () => ({ w: 900, h: 700 }),
    window: { A2_VIEW: "main", devicePixelRatio: 1.5, javaBridge: {} },
    document: {
      body: { classList: { contains: () => false } },
      querySelector: (selector) => selector === ".meter" ? { offsetWidth: 380, offsetHeight: 200, scrollHeight: 200 }
        : selector === ".hoverDetailsTooltip.isVisible" ? tooltip
        : selector === ".settingsPanel.isOpen" && fullPanel ? {} : null,
    },
    invoke: (command, args) => { sizes.push(args); return Promise.resolve(); },
  });
  vm.runInContext(sizingSource, context);
  vm.runInContext("updateWindowSize()", context);
  assert.equal(sizes[0].width, 618);
  assert.equal(sizes[0].height, 368);
  assert.equal(sizes[0].scale, 1.5);
  tooltip = null;
  vm.runInContext("updateWindowSize()", context);
  assert.equal(sizes[1].width, 396);
  assert.equal(sizes[1].height, 210);
  fullPanel = true;
  vm.runInContext("updateWindowSize()", context);
  assert.equal(sizes[2].width, 1200);
  assert.equal(sizes[2].height, 800);
});


test("hover translations preserve the existing details tooltip text", () => {
  for (const locale of ["en", "ru"]) {
    const dictionary = JSON.parse(readFileSync(new URL(`../src/data/i18n/ui/${locale}.json`, import.meta.url), "utf8"));
    assert.equal(typeof dictionary.details.tooltip, "string");
    const { app } = setup();
    app.i18n = { t: (key, fallback) => key.split(".").reduce((value, part) => value?.[part], dictionary) ?? fallback };
    app.hoverTooltipEl = { style: {}, classList: { add() {} }, offsetWidth: 100, offsetHeight: 100 };
    for (const state of ["loading", "empty", "error"]) {
      Object.getPrototypeOf(app).renderHoverTooltip.call(app, { skills: [], state }, { id: 1 }, {});
      assert.ok(app.hoverTooltipEl.innerHTML.includes(dictionary.details.hoverTooltip[state]));
    }
  }
});
