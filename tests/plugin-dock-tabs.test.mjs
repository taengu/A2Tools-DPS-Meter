import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/pluginDock.js", import.meta.url), "utf8");
function body(name) {
  const start = source.indexOf("  const " + name + " = ");
  const from = source.indexOf("=>", start) + 2, open = source.indexOf("{", from);
  let end = open + 1, depth = 1;
  while (depth) { if (source[end] === "{") depth++; else if (source[end] === "}") depth--; end++; }
  return source.slice(source.indexOf("=", start) + 1, end).trim();
}
function element() {
  const children = new Map();
  return {
    style: { setProperty() {} }, children: [], attributes: {}, listeners: {}, dataset: {},
    classList: { toggle() {} }, setAttribute(k, v) { this.attributes[k] = v; },
    append(x) { this.children.push(x); }, appendChild(x) { this.children.push(x); },
    replaceChildren() { this.children = []; }, remove() {}, scrollIntoView() {},
    querySelector(k) { if (!children.has(k)) children.set(k, element()); return children.get(k); },
    addEventListener(k, fn) { this.listeners[k] = fn; },
  };
}
function setup(states, saved = {}, customInvoke = null) {
  const docked = new Map(), tabsEl = element(), calls = [], errors = [], listeners = {};
  const prefs = { active: null, layout: "tabs", folded: false, order: [], compact: {}, ...saved };
  let stored;
  const context = vm.createContext({
    prefs, docked, tabsEl, stage: element(), layoutBtn: element(), foldBtn: element(),
    template: { content: { firstElementChild: { cloneNode: element } } },
    ordered: () => [...docked.values()].sort((a, b) =>
      prefs.order.indexOf(a.id) - prefs.order.indexOf(b.id)),
    setLabel() {}, savePrefs() { stored = JSON.parse(JSON.stringify(prefs)); },
    setFolded() { prefs.folded = false; }, t: (_key, fallback) => fallback,
    listen: (name, fn) => { listeners[name] = fn; },
    document: { body: element(), createElement: element },
    window: {
      A2_PLUGIN_ICON: () => ({ glyph: null, hue: 20, letter: "C" }),
      createPluginFrame: (options) => ({
        start: () => Promise.resolve(), destroy() {}, setCompact() {},
        fit: options.onFit,
      }),
    },
    showToast: e => errors.push(e),
    invoke: (name, args) => {
      calls.push({ name, args });
      if (customInvoke) return customInvoke(name, args);
      if (name === "dock_select") docked.get(args.id).shown = true;
      return Promise.resolve();
    },
  });
  const functions = ["render", "activate", "makeTab", "makePanel", "applyLayout"]
    .map(name => "const " + name + "=" + body(name) + ";").join("\n");
  const events = source.slice(source.indexOf("  const backTo = "), source.indexOf("  // Plugin events,"));
  vm.runInContext(functions + events + ";globalThis.api={render,applyLayout};", context);
  const apply = rows => context.api.applyLayout({ tabs: rows.map(([id, shown]) => ({ id, shown, name: id, version: "1.0" })) });
  apply(states);
  return { docked, tabsEl, calls, errors, prefs, apply, api: context.api,
    stored: () => stored, event: (name, id) => listeners[name]({ payload: id }) };
}

test("hidden panels retain tabs and the selected configuration without opening a window", () => {
  const s = setup([["cooldowns", true], ["boss-telegraph", false]]);
  const panel = s.docked.get("cooldowns").panel;
  s.apply([["cooldowns", false], ["boss-telegraph", false]]);
  assert.equal(s.tabsEl.children.length, 2);
  assert.equal(s.prefs.active, "cooldowns");
  assert.equal(panel.hidden, false);
  assert.equal(s.docked.get("boss-telegraph").panel.hidden, true);
  assert.equal(s.calls.length, 0);
});
test("with no valid selection, the first tab in the saved order is selected", () => {
  for (const active of [null, "removed-plugin"]) {
    const s = setup([["cooldowns", false], ["boss-telegraph", false]],
      { active, order: ["boss-telegraph", "cooldowns"] });
    assert.equal(s.prefs.active, "boss-telegraph");
    assert.equal(s.tabsEl.children[0].attributes["aria-selected"], "true");
    assert.equal(s.docked.get("boss-telegraph").panel.hidden, false);
    assert.equal(s.calls.length, 0);
  }
});
test("the last selected hidden plugin survives hide, reopen and a new page", async () => {
  const s = setup([["cooldowns", true], ["boss-telegraph", true]]);
  s.docked.get("boss-telegraph").tab.listeners.click();
  await Promise.resolve();
  s.apply([["cooldowns", false], ["boss-telegraph", false]]);
  s.api.render();
  const reopened = setup([["cooldowns", false], ["boss-telegraph", false]], s.stored());
  assert.equal(reopened.prefs.active, "boss-telegraph");
  assert.equal(reopened.docked.get("boss-telegraph").panel.hidden, false);
  assert.equal(reopened.calls.length, 0);
});
test("an alert fit does not erase the previous tab before focus and return", () => {
  const s = setup([["cooldowns", false], ["boss-telegraph", false]], { active: "cooldowns" });
  s.docked.get("boss-telegraph").plugin.fit(true);
  s.event("dock-focus", "boss-telegraph");
  assert.equal(s.prefs.active, "boss-telegraph");
  s.docked.get("boss-telegraph").plugin.fit(false);
  s.event("dock-return", "boss-telegraph");
  assert.equal(s.prefs.active, "cooldowns");
  assert.equal(s.docked.get("cooldowns").panel.hidden, false);
  assert.equal(s.calls.length, 0);
});
test("an alert ending does not override a later manual selection", async () => {
  const s = setup([["cooldowns", false], ["boss-telegraph", false], ["ping", false]]);
  s.event("dock-focus", "boss-telegraph");
  s.docked.get("ping").tab.listeners.click();
  await Promise.resolve();
  s.event("dock-return", "boss-telegraph");
  assert.equal(s.prefs.active, "ping");
});
test("clicking a retained tab records explicit intent without recreating the panel", async () => {
  const s = setup([["cooldowns", false]]);
  const panel = s.docked.get("cooldowns").panel;
  s.docked.get("cooldowns").tab.listeners.click();
  await Promise.resolve();
  assert.equal(s.calls[0].name, "dock_select");
  assert.equal(s.calls[0].args.id, "cooldowns");
  assert.equal(s.prefs.active, "cooldowns");
  assert.equal(s.docked.get("cooldowns").panel, panel);
  assert.deepEqual(s.errors, []);
});
test("stacked mode keeps shown panels plus the selected configuration available", () => {
  const s = setup([["cooldowns", true], ["boss-telegraph", false], ["ping", false]],
    { active: "boss-telegraph", layout: "stack" });
  assert.equal(s.docked.get("cooldowns").panel.hidden, false);
  assert.equal(s.docked.get("boss-telegraph").panel.hidden, false);
  assert.equal(s.docked.get("ping").panel.hidden, true);
});
test("removing the selected plugin falls back to the first remaining tab; empty is harmless", () => {
  const s = setup([["cooldowns", false], ["boss-telegraph", false]], { active: "boss-telegraph" });
  s.apply([["cooldowns", false]]);
  assert.equal(s.prefs.active, "cooldowns");
  assert.equal(s.docked.get("cooldowns").panel.hidden, false);
  s.apply([]);
  assert.equal(s.prefs.active, null);
  assert.equal(s.tabsEl.children.length, 0);
  assert.equal(s.calls.length, 0);
});

test("overlapping alerts return to the configuration even when the first alert ends first", () => {
  const s = setup([["cooldowns", false], ["boss-telegraph", false], ["ping", false]]);
  s.event("dock-focus", "boss-telegraph");
  s.event("dock-focus", "ping");
  s.event("dock-return", "boss-telegraph");
  assert.equal(s.prefs.active, "ping");
  s.event("dock-return", "ping");
  assert.equal(s.prefs.active, "cooldowns");
  assert.equal(s.calls.length, 0);
});

test("explicitly selecting the alert tab keeps that selection when the alert ends", async () => {
  const s = setup([["cooldowns", false], ["boss-telegraph", false]]);
  s.event("dock-focus", "boss-telegraph");
  s.docked.get("boss-telegraph").tab.listeners.click();
  await Promise.resolve();
  s.event("dock-return", "boss-telegraph");
  assert.equal(s.prefs.active, "boss-telegraph");
});

test("an alert on the manually selected tab stays the baseline for a later alert", () => {
  const s = setup([["cooldowns", false], ["boss-telegraph", false]], { active: "cooldowns" });
  s.event("dock-focus", "cooldowns");
  s.event("dock-focus", "boss-telegraph");
  s.event("dock-return", "cooldowns");
  s.event("dock-return", "boss-telegraph");
  assert.equal(s.prefs.active, "cooldowns");
});


test("the latest clicked tab wins even when native acknowledgements finish backwards", async () => {
  const pending = new Map();
  const s = setup([["a", false], ["b", false], ["c", false]], {},
    (_name, args) => new Promise(resolve => pending.set(args.id, resolve)));
  s.docked.get("b").tab.listeners.click();
  s.docked.get("c").tab.listeners.click();
  assert.equal(s.prefs.active, "c", "the click is immediate");
  pending.get("c")(); await Promise.resolve();
  pending.get("b")(); await Promise.resolve();
  assert.equal(s.prefs.active, "c");
  assert.equal(s.stored().active, "c");
});

test("a late acknowledgement cannot erase an alert's return to the chosen tab", async () => {
  let finish;
  const s = setup([["a", false], ["b", false], ["alert", false]], {},
    () => new Promise(resolve => { finish = resolve; }));
  s.docked.get("b").tab.listeners.click();
  s.event("dock-focus", "alert");
  finish(); await Promise.resolve();
  assert.equal(s.prefs.active, "alert");
  s.event("dock-return", "alert");
  assert.equal(s.prefs.active, "b");
});

test("a completed selection cannot restore a plugin removed while waiting", async () => {
  let finish;
  const s = setup([["a", false], ["b", false], ["c", false]], {},
    () => new Promise(resolve => { finish = resolve; }));
  s.docked.get("b").tab.listeners.click();
  s.apply([["a", false], ["c", false]]);
  s.event("dock-focus", "c");
  finish(); await Promise.resolve();
  assert.equal(s.prefs.active, "c");
});

test("an older failed acknowledgement reports its error without changing the latest choice", async () => {
  const pending = new Map();
  const s = setup([["a", false], ["b", false], ["c", false]], {},
    (_name, args) => new Promise((resolve, reject) => pending.set(args.id, { resolve, reject })));
  s.docked.get("b").tab.listeners.click();
  s.docked.get("c").tab.listeners.click();
  pending.get("c").resolve();
  pending.get("b").reject("plugin removed");
  await Promise.resolve();
  assert.equal(s.prefs.active, "c");
  assert.deepEqual(s.errors, ["plugin removed"]);
});
