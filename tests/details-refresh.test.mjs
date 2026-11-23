import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/details.js", import.meta.url), "utf8");
const section = (start, end) => {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from);
  assert.ok(from >= 0 && to > from, `missing Details source section: ${start}`);
  return source.slice(from, to);
};
const refreshSource = [
  section("  const applyTargetSelection = async", "  const combinePerActorStats"),
  section("  // A synthetic null row", "  const render = (details, row) => {"),
  section("  const isOpen = ()", "  detailsClose?.addEventListener"),
  section("  const refresh = async", "  const isPinned = ()"),
].join("\n");
const tick = () => new Promise((resolve) => setImmediate(resolve));

function detailsView({ targetCount = 20, panelOpen = true } = {}) {
  const targets = Array.from({ length: targetCount }, (_, i) => ({ targetId: i + 1, totalDamage: 10 }));
  const requests = [];
  const rendered = [];
  const logs = [];
  const timers = new Map();
  const classes = new Set(panelOpen ? ["open"] : []);
  let timerId = 0;
  let active = 0;
  let peak = 0;
  let contextReads = 0;
  const context = vm.createContext({
    getDetails: (row, options) => {
      let resolve;
      let reject;
      const promise = new Promise((ok, fail) => { resolve = ok; reject = fail; });
      peak = Math.max(peak, ++active);
      const request = {
        row, options, settled: false,
        resolve: (version = 1) => {
          assert.equal(request.settled, false);
          request.settled = true;
          resolve({ targetId: options?.targetId, attackers: options?.attackerIds, version });
        },
        reject: (error) => {
          assert.equal(request.settled, false);
          request.settled = true;
          reject(error);
        },
      };
      requests.push(request);
      return promise.then((value) => { active--; return value; }, (error) => { active--; throw error; });
    },
    window: { javaBridge: { logToDebug: (message) => logs.push(message) } },
    detailsPanel: {
      classList: { contains: (name) => classes.has(name), add: (name) => classes.add(name), remove: (name) => classes.delete(name) },
      style: { removeProperty() {} },
    },
    openSeq: 1, autoRefreshTimer: null, openedRowId: 7, pinnedRowId: null, onPinnedRowChange: null,
    lastRow: { id: 7 }, lastDetails: { old: true }, lastUnfilteredDetails: null,
    activeCompactMode: false, detailsContext: {}, detailsTargets: targets, detailsActors: new Map(),
    selectedTargetId: null, selectedAttackerIds: [7], selectedAttackerLabel: "Actor 7", COMPACT_MAX_SKILLS: 5,
    detailsMode: "dmg", lastMeasuredNameWidth: 0, fightStartMs: 0, fightBossName: "", fightDungeonId: 0, historyRecord: null,
    buffTimeline: null, staggerPanel: null, fightCompare: null, liveBuffs: null,
    getSelectableTargets: () => targets,
    getTargetById: (id) => targets.find((target) => target.targetId === id),
    getTargetLabel: (target) => `Target ${target.targetId}`,
    getDungeonId: () => null,
    getCachedDetails: () => null,
    buildCombinedDetails: (list, total) => ({ list, total }),
    resolveActorLabel: (id) => `Actor ${id}`,
    resolveRowLabel: (row) => row.name || `Actor ${row.id}`,
    labelText: (_, fallback) => fallback,
    targetMatchesSelectedAttackers: () => true,
    loadDetailsContext: () => { contextReads++; },
    syncSelectedAttackersFromLabel() {}, syncSortButtons() {}, syncModeButtons() {}, updateHeaderText() {}, updateGridColumns() {},
    restoreLayout() {}, resetFtlZoom() {},
    render: (details, row) => {
      rendered.push({ details, row });
      context.lastDetails = details;
      context.lastRow = row;
    },
    statSlots: [{ valueEl: { textContent: "1" }, statEl: { style: {} } }],
    skillSlots: [{ rowEl: { style: {} }, rowFillEl: { style: {} } }],
    setInterval: (callback, ms) => { timers.set(++timerId, { callback, ms }); return timerId; },
    clearInterval: (id) => timers.delete(id),
    requestAnimationFrame: (callback) => callback(),
  });
  vm.runInContext(refreshSource, context);
  const api = vm.runInContext("({ refresh, open, close, selectTarget: applyTargetSelection, selectAttacker: applyAttackerSelection })", context);
  const pending = () => requests.filter((request) => !request.settled);
  const complete = async (count, version = 1, beforeBatch = () => {}) => {
    let completed = 0;
    await tick();
    while (completed < count) {
      const batch = pending().slice(0, count - completed);
      assert.ok(batch.length, `expected ${count - completed} more Details requests`);
      beforeBatch();
      batch.forEach((request) => request.resolve(version));
      completed += batch.length;
      await tick();
    }
  };
  const flushCancelled = async () => {
    for (let i = 0; i < 3; i++) await tick();
  };
  const timerTick = () => {
    assert.equal(timers.size, 1, "one timer belongs to the open panel");
    [...timers.values()][0].callback();
  };
  return {
    api, context, requests, rendered, logs, timers, pending, complete, flushCancelled, timerTick,
    get peak() { return peak; },
    get contextReads() { return contextReads; },
    queueLength: () => vm.runInContext("detailsRequestQueue.length", context),
  };
}

test("overlapping automatic refreshes render the active result and coalesce one newer snapshot", async () => {
  const view = detailsView();
  const first = view.api.refresh();
  await tick();
  const generation = view.context.openSeq;
  const followers = Array.from({ length: 12 }, () => view.api.refresh());
  assert.equal(view.context.openSeq, generation, "timer ticks do not cancel the active selection");
  assert.equal(view.requests.length, 4);
  assert.equal(view.queueLength(), 36, "ticks do not add duplicate target loads");
  assert.equal(view.contextReads, 1);
  await view.complete(40, 1);
  await Promise.all([first, ...followers]);
  assert.equal(view.rendered.length, 1, "the slow result still reaches the panel");
  assert.equal(view.contextReads, 2, "one pending snapshot starts after the first finishes");
  assert.equal(view.pending().length, 4);
  await view.complete(40, 2);
  assert.equal(view.rendered.length, 2);
  assert.ok(view.rendered[1].details.list.every((details) => details.version === 2));
  assert.equal(view.requests.length, 80);
  assert.equal(view.peak, 4);
});

test("the actual live timer makes progress when every target batch takes more ticks", async () => {
  const view = detailsView({ panelOpen: false });
  const opening = view.api.open({ id: 7 }, { defaultTargetAll: true });
  assert.equal(view.timers.size, 1, "the timer starts before the first response");
  assert.equal([...view.timers.values()][0].ms, 2000);
  const generation = view.context.openSeq;
  await view.complete(40, 1, () => {
    for (let i = 0; i < 3; i++) view.timerTick();
    assert.equal(view.context.openSeq, generation);
  });
  await opening;
  assert.equal(view.rendered.length, 1);
  assert.equal(view.contextReads, 2);
  await view.complete(40, 2, () => view.timerTick());
  assert.equal(view.rendered.length, 2, "continued timer pressure cannot starve rendering");
  assert.equal(view.contextReads, 3);
  assert.equal(view.peak, 4);
  view.api.close();
  view.pending().forEach((request) => request.resolve(3));
  await view.flushCancelled();
  assert.equal(view.rendered.length, 2, "a pending snapshot cannot paint after close");
  assert.equal(view.pending().length, 0);
  assert.equal(view.timers.size, 0);
});

test("an old error cannot clear the new target's pending refresh or start a ghost load", async () => {
  const view = detailsView();
  const old = view.api.refresh();
  await tick();
  view.api.refresh();
  const oldRequests = [...view.pending()];
  const manual = view.api.selectTarget(2);
  const generation = view.context.openSeq;
  const followup = view.api.refresh();
  oldRequests[0].reject(new Error("old target failed"));
  oldRequests.slice(1).forEach((request) => request.resolve(1));
  await old;
  await view.flushCancelled();
  assert.equal(view.context.openSeq, generation);
  assert.equal(view.contextReads, 1, "the old finally does not refresh the new selection");
  assert.equal(view.rendered.length, 0);
  assert.equal(view.logs.length, 0, "a stale error is silent");
  assert.equal(view.context.statSlots[0].valueEl.textContent, "1");
  assert.equal(view.pending().length, 2);
  assert.ok(view.pending().every((request) => request.options.targetId === 2));
  await view.complete(2, 2);
  await Promise.all([manual, followup]);
  assert.equal(view.rendered[0].details.targetId, 2);
  assert.equal(view.contextReads, 2, "the new selection keeps its pending refresh");
  await view.complete(2, 3);
  assert.equal(view.rendered.at(-1).details.version, 3);
  assert.equal(view.requests.length, 8, "old in-flight calls, the manual load and one followup only");
  assert.equal(view.peak, 4);
});

test("changing the attacker supersedes an active tick and renders only the latest player", async () => {
  const view = detailsView();
  const old = view.api.refresh();
  await tick();
  const oldRequests = [...view.pending()];
  const manual = view.api.selectAttacker(8);
  oldRequests.forEach((request) => request.resolve(1));
  await old;
  await view.flushCancelled();
  assert.equal(view.rendered.length, 0);
  await view.complete(40, 2);
  await manual;
  assert.equal(view.rendered.length, 1);
  assert.ok(view.rendered[0].details.list.every((details) => details.attackers?.[0] === 8));
  assert.ok(view.context.lastUnfilteredDetails.list.every((details) => details.attackers === null));
  assert.equal(view.requests.length, 44, "the stale queued calls never reach the backend");
  assert.equal(view.logs.length, 0);
});

test("close and reopening another row protect its active load, pending tick and single timer", async () => {
  const view = detailsView();
  const old = view.api.refresh();
  await tick();
  view.api.refresh();
  const oldRequests = [...view.pending()];
  view.api.close();
  await view.api.refresh();
  assert.equal(view.requests.length, 4, "a closed panel does not load");
  const opening = view.api.open({ id: 9 }, { defaultTargetId: 2 });
  const generation = view.context.openSeq;
  view.timerTick();
  oldRequests[0].reject(new Error("closed row failed"));
  oldRequests.slice(1).forEach((request) => request.resolve(1));
  await old;
  await view.flushCancelled();
  assert.equal(view.rendered.length, 0);
  assert.equal(view.logs.length, 0);
  assert.equal(view.contextReads, 2, "the old finally cannot start an extra refresh");
  assert.ok(view.pending().every((request) => request.row.id === 9 && request.options.targetId === 2));
  await view.complete(2, 2);
  await opening;
  assert.equal(view.context.openSeq, generation, "the automatic followup keeps the new generation");
  assert.equal(view.rendered[0].row.id, 9);
  assert.equal(view.timers.size, 1);
  assert.equal(view.contextReads, 3, "the new row retains its coalesced tick");
  await view.complete(2, 3);
  assert.equal(view.requests.length, 8);
  assert.equal(view.rendered.at(-1).row.id, 9);
  assert.equal(view.rendered.at(-1).details.version, 3);
  view.api.close();
});

test("a manual selection before the first open response retains the live timer", async () => {
  const view = detailsView({ panelOpen: false });
  const opening = view.api.open({ id: 7 }, { defaultTargetAll: true });
  await tick();
  const initial = [...view.pending()];
  const manual = view.api.selectTarget(2);
  initial.forEach((request) => request.resolve(1));
  await opening;
  await view.flushCancelled();
  await view.complete(2, 2);
  await manual;
  assert.equal(view.rendered.length, 1);
  assert.equal(view.rendered[0].details.targetId, 2);
  assert.equal(view.timers.size, 1);
  view.timerTick();
  await view.complete(2, 3);
  assert.equal(view.rendered.length, 2, "the manually selected view continues to update");
  assert.equal(view.rendered[1].details.version, 3);
  assert.equal(view.peak, 4);
  view.api.close();
});

test("a failed automatic snapshot releases its load and accepts the next live result", async () => {
  const view = detailsView({ targetCount: 1 });
  const failed = view.api.refresh();
  await tick();
  view.pending()[0].reject(new Error("current snapshot failed"));
  view.pending()[0].resolve(1);
  await failed;
  await view.flushCancelled();
  assert.equal(view.logs.length, 1);
  assert.equal(view.rendered.length, 0);
  const next = view.api.refresh();
  await view.complete(2, 2);
  await next;
  assert.equal(view.rendered.length, 1);
  assert.equal(view.rendered[0].details.list[0].version, 2);
});
