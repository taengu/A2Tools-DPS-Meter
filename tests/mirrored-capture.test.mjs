import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const core = readFileSync(new URL("../public/src/js/core.js", import.meta.url), "utf8");
const bridge = readFileSync(new URL("../public/src/js/tauriBridge.js", import.meta.url), "utf8");
const method = (name) => {
  const start = core.indexOf(`  ${name}(`);
  assert.notEqual(start, -1);
  const end = core.indexOf("\n  }", start) + 4;
  return core.slice(start, end);
};

function form(values) {
  const controls = vm.runInNewContext(`({${["readAutoDetectDevice", "_deviceSettingKey", "_syncCaptureMode"].map(method).join(",")}})`);
  return Object.assign(controls, {
    safeGetSetting: (key) => values[key],
    autoDetectDeviceCheckbox: {}, mirroredCaptureCheckbox: {},
  });
}

test("mirrored capture forces explicit adapter selection without overwriting ordinary preferences", () => {
  const values = { "dpsMeter.autoDetectDevice": "true", "dpsMeter.manualDevice": "VPN" };
  const app = form(values);
  app._syncCaptureMode();
  assert.equal(app.autoDetectDeviceCheckbox.checked, true);
  assert.equal(app.autoDetectDeviceCheckbox.disabled, false);
  assert.equal(app._deviceSettingKey(), "dpsMeter.manualDevice");
  values["dpsMeter.mirroredCapture"] = "true";
  app._syncCaptureMode();
  assert.equal(app.autoDetectDeviceCheckbox.checked, false);
  assert.equal(app.autoDetectDeviceCheckbox.disabled, true);
  assert.equal(app.mirroredCaptureCheckbox.checked, true);
  assert.equal(app._deviceSettingKey(), "dpsMeter.mirroredDevice");
  values["dpsMeter.mirroredCapture"] = "false";
  app._syncCaptureMode();
  assert.equal(app.autoDetectDeviceCheckbox.checked, true);
  assert.equal(app.autoDetectDeviceCheckbox.disabled, false);
  assert.equal(values["dpsMeter.manualDevice"], "VPN");
});

function devices() {
  const start = bridge.indexOf("  let devicesLoading = null;");
  const end = bridge.indexOf("  // A reloaded window", start);
  const calls = [];
  const context = {
    Date, settingsCache: {}, window: {}, localStorage: { getItem: () => null },
    invoke(command, args) {
      let resolve, reject;
      const promise = new Promise((r, e) => { resolve = r; reject = e; });
      calls.push({ command, args, resolve, reject });
      return promise;
    },
  };
  vm.runInNewContext(bridge.slice(start, end) + "globalThis.loadDevices = loadDevices;", context);
  return { context, calls };
}

test("an unnumbered mirror adapter can be listed before a saved-setting request completes", async () => {
  const { context, calls } = devices();
  context.settingsCache["dpsMeter.mirroredCapture"] = "true";
  const pending = context.loadDevices();
  assert.equal(calls[0].command, "get_available_devices");
  assert.equal(calls[0].args.mirrored, true);
  calls[0].resolve(["SPAN [nic-a]"]);
  await pending;
  assert.deepEqual(context.window._cachedDevices, ["SPAN [nic-a]"]);
});

test("the mirror list requires a deliberate choice and preserves the ordinary adapter setting", () => {
  const values = { "dpsMeter.mirroredCapture": "true", "dpsMeter.manualDevice": "VPN" };
  const text = {};
  const menu = { children: [], appendChild(node) { this.children.push(node); },
    classList: { remove() {} }, querySelectorAll() { return this.children; } };
  const context = {
    window: { javaBridge: { getAvailableDevices: () => '["SPAN [nic-a]"]', getConnectionInfo: () => '{"device":"VPN"}' } },
    document: { createElement() {
      const classes = new Set();
      return { dataset: {}, handlers: {}, classList: { add: name => classes.add(name), contains: name => classes.has(name), toggle() {} },
        addEventListener(name, fn) { this.handlers[name] = fn; } };
    } },
  };
  const app = vm.runInNewContext(`({${["_populateDeviceDropdown", "_deviceSettingKey"].map(method).join(",")}})`, context);
  Object.assign(app, {
    deviceDropdownBtn: { querySelector: () => text }, deviceDropdownMenu: menu,
    safeGetSetting: key => values[key], safeSetSetting: (key, value) => { values[key] = value; },
    _deviceSettingKey: () => "dpsMeter.mirroredDevice", safeParseJSON: JSON.parse,
    refreshConnectionInfo() {}, _autoDetectDevice: false,
  });
  app._populateDeviceDropdown(null);
  assert.equal(text.textContent, "Select mirror adapter");
  assert.equal(values["dpsMeter.mirroredDevice"], undefined);
  assert.equal(menu.children[0].classList.contains("isActive"), false);
  menu.children[0].handlers.click();
  assert.equal(values["dpsMeter.mirroredDevice"], "SPAN [nic-a]");
  assert.equal(values["dpsMeter.manualDevice"], "VPN");
});

test("an earlier device response cannot replace the list for a newly selected capture mode", async () => {
  const { context, calls } = devices();
  const ordinary = context.loadDevices();
  context.settingsCache["dpsMeter.mirroredCapture"] = "true";
  const mirrored = context.loadDevices();
  assert.equal(calls.length, 2);
  assert.equal(calls[0].args.mirrored, false);
  calls[1].resolve(["SPAN [nic-a]"]);
  await mirrored;
  calls[0].resolve(["Ethernet"]);
  await ordinary;
  assert.deepEqual(context.window._cachedDevices, ["SPAN [nic-a]"]);
});

test("a rejected mirror discovery cannot reuse ordinary cached adapter labels", async () => {
  const { context, calls } = devices();
  const ordinary = context.loadDevices();
  calls[0].resolve(["Ethernet"]);
  await ordinary;
  assert.deepEqual(context.window._cachedDevices, ["Ethernet"]);
  context.settingsCache["dpsMeter.mirroredCapture"] = "true";
  const mirrored = context.loadDevices();
  assert.equal(context.window._cachedDevices.length, 0, "a new mode must not offer the previous mode's labels");
  calls[1].reject(new Error("device discovery unavailable"));
  await assert.rejects(mirrored, /device discovery unavailable/);
  assert.equal(context.window._cachedDevices.length, 0);
});

test("an empty mirror list clears ordinary adapter choices and a later list re-enables selection", () => {
  const values = { "dpsMeter.manualDevice": "Ethernet" };
  let raw = '["Ethernet"]';
  const text = {};
  const menu = { children: [], appendChild(node) { this.children.push(node); },
    classList: { remove() {} }, querySelectorAll() { return this.children; } };
  Object.defineProperty(menu, "innerHTML", { set() { this.children = []; } });
  const context = {
    window: { javaBridge: { getAvailableDevices: () => raw, getConnectionInfo: () => "{}" } },
    document: { createElement() { return { dataset: {}, classList: { add() {}, toggle() {} }, addEventListener() {} }; } },
  };
  const app = vm.runInNewContext(`({${["_populateDeviceDropdown", "_deviceSettingKey"].map(method).join(",")}})`, context);
  Object.assign(app, { deviceDropdownBtn: { querySelector: () => text }, deviceDropdownMenu: menu,
    safeGetSetting: key => values[key], safeParseJSON: JSON.parse, _autoDetectDevice: false });
  app._populateDeviceDropdown("Ethernet");
  assert.equal(menu.children.length, 1);
  assert.equal(text.textContent, "Ethernet");
  values["dpsMeter.mirroredCapture"] = "true";
  raw = "[]";
  app._populateDeviceDropdown(null);
  assert.equal(menu.children.length, 0, "old ordinary choices must not remain selectable");
  assert.equal(text.textContent, "Select mirror adapter");
  assert.equal(app.deviceDropdownBtn.disabled, true);
  raw = '["SPAN [nic-a]"]';
  app._populateDeviceDropdown(null);
  assert.equal(menu.children.length, 1);
  assert.equal(menu.children[0].textContent, "SPAN [nic-a]");
  assert.equal(app.deviceDropdownBtn.disabled, false);
  assert.equal(values["dpsMeter.manualDevice"], "Ethernet");
  assert.equal(values["dpsMeter.mirroredDevice"], undefined);
});

test("switching capture modes clears old choices while discovery is still pending", async () => {
  const values = { "dpsMeter.manualDevice": "Ethernet" };
  let raw = '["Ethernet"]';
  let resolve;
  const pending = new Promise(r => { resolve = r; });
  const text = {};
  const menu = { children: [], appendChild(node) { this.children.push(node); },
    classList: { remove() {} }, querySelectorAll() { return this.children; } };
  Object.defineProperty(menu, "innerHTML", { set() { this.children = []; } });
  const context = {
    window: { javaBridge: { getAvailableDevices: () => raw, getConnectionInfo: () => "{}", loadAvailableDevices: () => pending } },
    document: { createElement() { return { dataset: {}, handlers: {}, classList: { add() {}, toggle() {} },
      addEventListener(name, fn) { this.handlers[name] = fn; } }; } },
  };
  const app = vm.runInNewContext(`({${["_populateDeviceDropdown", "_loadDeviceDropdown", "_deviceSettingKey", "_updateDeviceDropdownState"].map(method).join(",")}})`, context);
  Object.assign(app, { deviceDropdownBtn: { querySelector: () => text }, deviceDropdownMenu: menu,
    safeGetSetting: key => values[key], safeSetSetting: (key, value) => { values[key] = value; },
    safeParseJSON: JSON.parse, _autoDetectDevice: false, refreshConnectionInfo() {} });
  app._populateDeviceDropdown("Ethernet");
  const oldItem = menu.children[0];
  values["dpsMeter.mirroredCapture"] = "true";
  app._loadDeviceDropdown();
  assert.equal(menu.children.length, 0);
  assert.equal(app.deviceDropdownBtn.disabled, true);
  assert.equal(text.textContent, "Select mirror adapter");
  oldItem.handlers.click();
  assert.equal(values["dpsMeter.mirroredDevice"], undefined);
  raw = '["SPAN [nic-a]"]';
  resolve();
  await pending;
  assert.equal(menu.children.length, 1);
  assert.equal(menu.children[0].textContent, "SPAN [nic-a]");
  assert.equal(app.deviceDropdownBtn.disabled, false);
  menu.children[0].handlers.click();
  assert.equal(values["dpsMeter.mirroredDevice"], "SPAN [nic-a]");
  assert.equal(values["dpsMeter.manualDevice"], "Ethernet");
});

test("mirror status and game-provided names do not require or adopt a local window title", () => {
  for (const mirrored of [false, true]) {
    for (const title of ["", "AION2 Another character"]) {
      const context = { window: { javaBridge: { getAion2WindowTitle: () => title } } };
      const app = vm.runInNewContext(`({${["checkAion2WindowTitle", "refreshSettingsStatus"].map(method).join(",")}})`, context);
      let synced = 0;
      let parsedTitle = 0;
      Object.assign(app, { aionRunning: false,
        safeGetSetting: () => String(mirrored),
        syncCharacterNameFromGame() { synced++; },
        parseCharacterNameFromWindowTitle() { parsedTitle++; return ""; },
        refreshConnectionInfo() {}, updateConnectionStatusUi() {} });
      app.checkAion2WindowTitle();
      assert.equal(app.aionRunning, mirrored || !!title);
      assert.equal(synced, mirrored || title ? 1 : 0);
      assert.equal(parsedTitle, !mirrored && title ? 1 : 0, "a monitoring PC's local title must not name the remote character");
      app.refreshSettingsStatus();
      assert.equal(app.aionRunning, mirrored || !!title);
      assert.equal(synced, mirrored || title ? 2 : 0);
    }
  }
});

test("a mirror PC waits for traffic when the native port is null and shows the detected port later", () => {
  let port = null;
  const context = { document: { activeElement: null }, window: { javaBridge: {
    getAion2WindowTitle: () => null,
    getConnectionInfo: () => JSON.stringify({ port }),
  } } };
  const app = vm.runInNewContext(`({${["refreshSettingsStatus", "refreshConnectionInfo"].map(method).join(",")}})`, context);
  Object.assign(app, { safeGetSetting: () => "true", safeParseJSON: JSON.parse,
    lockedIp: {}, lockedPort: { classList: { remove() {} } },
    i18n: { t: (_key, fallback) => fallback }, syncCharacterNameFromGame() {},
    updateConnectionStatusUi() {}, refreshSettingsPanelIfOpen() {} });
  app.refreshSettingsStatus();
  assert.equal(app.aionRunning, true, "mirrored capture remains eligible without a local game");
  assert.equal(app.isDetectingPort, true);
  assert.equal(app.lockedPort.textContent, "Connecting...");
  port = 13328;
  app.refreshSettingsStatus();
  assert.equal(app.isDetectingPort, false);
  assert.equal(app.lockedPort.textContent, "13328");
});
