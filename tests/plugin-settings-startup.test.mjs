import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/pluginSettings.js", import.meta.url), "utf8");
const settle = () => new Promise((resolve) => setImmediate(resolve));
const AUTO_UPDATE = "dpsMeter.pluginAutoUpdate";
const SEEN = "dpsMeter.pluginsSeen";

function node() {
  const classes = new Set(), listeners = new Map();
  return {
    checked: false, children: [], dataset: {}, hidden: false,
    style: { setProperty() {} },
    classList: {
      add: (...names) => names.forEach((name) => classes.add(name)),
      remove: (...names) => names.forEach((name) => classes.delete(name)),
      contains: (name) => classes.has(name),
      toggle: (name, on) => on ? classes.add(name) : classes.delete(name),
    },
    append(...children) { this.children.push(...children); },
    prepend(...children) { this.children.unshift(...children); },
    replaceChildren(...children) { this.children = children; },
    get childElementCount() { return this.children.length; },
    setAttribute() {}, removeAttribute() {},
    addEventListener: (name, callback) => listeners.set(name, callback),
    emit: (name) => listeners.get(name)?.(),
  };
}

function setup({ cached = {}, snapshot, readyState = "complete", view = "settings", legacy = false } = {}) {
  const nodes = new Map(), dom = new Map(), calls = [], writes = [];
  let values = new Map(Object.entries(cached));
  let release;
  const waiting = new Promise((resolve) => { release = resolve; });
  const ready = waiting.then(() => { if (snapshot) values = new Map(Object.entries(snapshot)); });
  const document = {
    readyState,
    querySelector(selector) { if (!nodes.has(selector)) nodes.set(selector, node()); return nodes.get(selector); },
    querySelectorAll: () => [],
    createElement: node,
    addEventListener: (name, callback) => dom.set(name, callback),
  };
  const window = {
    __A2_VIEW__: view,
    a2SettingsReady: legacy ? undefined : ready,
    javaBridge: {
      getSetting: (key) => values.get(key) ?? null,
      setSetting: (key, value) => { values.set(key, value); writes.push({ key, value }); },
    },
    A2_PLUGIN_ICON: () => ({ letter: "T", hue: 120 }),
    __TAURI__: {
      core: { invoke: (command) => {
        calls.push(command);
        if (command === "plugin_list") return Promise.resolve({
          online: false, devMode: false, disabled: [],
          plugins: [{ id: "current", name: "Current", version: "1.2.0", reviewed: true, permissions: [] }],
        });
        return Promise.resolve([]);
      } },
      window: { getCurrentWindow: () => ({ listen() {} }) },
    },
  };
  vm.runInNewContext(source, { window, document, console });
  return { nodes, document, calls, writes, release, dom };
}

test("plugin controls and seen versions wait for the backend snapshot instead of cached defaults", async () => {
  const page = setup({
    cached: { [AUTO_UPDATE]: "false" },
    snapshot: { [AUTO_UPDATE]: "true", [SEEN]: JSON.stringify({ current: "1.0.0", retained: "3.0.0" }) },
  });
  await settle();
  assert.equal(page.calls.length, 0, "plugin initialization overtook stored settings");
  assert.equal(page.writes.length, 0, "initialization overwrote saved version history");
  page.release();
  await settle();
  assert.equal(page.nodes.get(".pluginAutoUpdateCheckbox").checked, true);
  assert.equal(page.calls.filter((name) => name === "plugin_list").length, 1);
  const seen = page.writes.find(({ key }) => key === SEEN);
  assert.ok(seen);
  assert.deepEqual(JSON.parse(seen.value), { current: "1.2.0", retained: "3.0.0" });
});

test("a failed native settings read can retain the cached preferences when readiness settles", async () => {
  // The bridge catches a failed get_settings call and settles readiness while
  // retaining its local cache; this fixture models that public contract.
  const page = setup({ cached: { [AUTO_UPDATE]: "true", [SEEN]: JSON.stringify({ retained: "3.0.0" }) } });
  page.release();
  await settle();
  assert.equal(page.nodes.get(".pluginAutoUpdateCheckbox").checked, true);
  assert.equal(JSON.parse(page.writes.find(({ key }) => key === SEEN).value).retained, "3.0.0");
});

test("settled settings do not initialize plugin controls before the document is ready", async () => {
  const page = setup({ readyState: "loading", snapshot: { [AUTO_UPDATE]: "true" } });
  page.release();
  await settle();
  assert.equal(page.calls.length, 0);
  assert.equal(page.nodes.has(".pluginAutoUpdateCheckbox"), false);
  page.dom.get("DOMContentLoaded")();
  await settle();
  assert.equal(page.nodes.get(".pluginAutoUpdateCheckbox").checked, true);
  assert.equal(page.calls.filter((name) => name === "plugin_list").length, 1);
});

test("legacy hosts without a readiness promise still initialize and persist an explicit checkbox edit", async () => {
  const page = setup({ legacy: true, cached: { [AUTO_UPDATE]: "true" } });
  await settle();
  const checkbox = page.nodes.get(".pluginAutoUpdateCheckbox");
  assert.equal(checkbox.checked, true);
  checkbox.checked = false;
  checkbox.emit("change");
  assert.equal(page.writes.find(({ key }) => key === AUTO_UPDATE).value, "false");
});

test("other meter windows do not initialize the plugin settings page", async () => {
  const page = setup({ view: "main", legacy: true });
  await settle();
  assert.equal(page.nodes.size, 0);
  assert.equal(page.calls.length, 0);
  assert.equal(page.writes.length, 0);
});
