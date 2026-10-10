import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const core = readFileSync(new URL("../public/src/js/core.js", import.meta.url), "utf8");
const bridge = readFileSync(new URL("../public/src/js/tauriBridge.js", import.meta.url), "utf8");
const i18n = readFileSync(new URL("../public/src/js/i18n.js", import.meta.url), "utf8");
const settle = () => new Promise((resolve) => setImmediate(resolve));

function element(tag = "div") {
  const classes = new Set();
  const handlers = new Map();
  const properties = new Map();
  const attributes = new Map();
  const node = {
    tagName: tag.toUpperCase(), children: [], dataset: {}, textContent: "", value: "",
    style: {
      setProperty: (key, value) => properties.set(key, String(value)),
      getPropertyValue: (key) => properties.get(key) || "",
      removeProperty: (key) => properties.delete(key),
    },
    classList: {
      add: (...names) => names.forEach((name) => classes.add(name)),
      remove: (...names) => names.forEach((name) => classes.delete(name)),
      contains: (name) => classes.has(name),
      toggle: (name, on = !classes.has(name)) => { on ? classes.add(name) : classes.delete(name); return on; },
    },
    setAttribute: (key, value) => attributes.set(key, String(value)),
    getAttribute: (key) => attributes.get(key) ?? null,
    removeAttribute: (key) => attributes.delete(key),
    getBoundingClientRect: () => ({ left: 0, top: 0, right: 400, bottom: 300, width: 400, height: 300 }),
    appendChild(child) { this.children.push(child); return child; },
    addEventListener(name, handler) {
      const listeners = handlers.get(name) || [];
      listeners.push(handler);
      handlers.set(name, listeners);
    },
    emit(name) {
      const event = { type: name, target: node, stopPropagation() {}, preventDefault() {} };
      if (name === "click") node.onclick?.(event);
      handlers.get(name)?.forEach((handler) => handler(event));
    },
    contains: (child) => child === node || node.children.includes(child),
    querySelector(selector) { return this.querySelectorAll(selector)[0] || null; },
    querySelectorAll(selector) {
      return this.children.filter((child) => selector === 'input[type="range"]'
        ? child.tagName === "INPUT" && child.type === "range"
        : child.classList.contains(selector.slice(1)));
    },
    handlers,
  };
  Object.defineProperties(node, {
    className: { get: () => [...classes].join(" "), set: (value) => { classes.clear(); String(value).split(/\s+/).filter(Boolean).forEach((name) => classes.add(name)); } },
    innerHTML: { get: () => "", set: () => { node.children = []; } },
  });
  return node;
}

async function pair({ holdLanguage = null } = {}) {
  const values = new Map(Object.entries({
    "dpsMeter.betaUi": "true", "dpsMeter.slimMode": "false", "dpsMeter.theme": "aion2",
    "dpsMeter.playerLimit": "6", "dpsMeter.meterFillOpacity": "80", "dpsMeter.windowOpacity": "40",
    "dpsMeter.language": "en", "dpsMeter.migration.opacityReset1": "done",
    "dpsMeter.mainPlayerNamesBold": "true", "dpsMeter.mainPlayerDpsBold": "true",
    "dpsMeter.defaultMeterMode": "bossTargets", "dpsMeter.trainSelectionMode": "all",
    "dpsMeter.allTargetsWindowMs": "120000", "dpsMeter.targetSelectionWindowMs": "5000",
  }));
  const windows = [];
  const calls = [];
  const broadcasts = [];
  const heldLanguages = [];
  const broadcast = (key, value) => {
    broadcasts.push({ key, value });
    queueMicrotask(() => windows.forEach((surface) => surface.listeners.get("setting-changed")?.({ payload: { key, value } })));
  };
  async function makeWindow(view) {
    const nodes = new Map();
    const listeners = new Map();
    const events = new Map();
    const timers = new Map();
    const domHandlers = new Map();
    const storage = new Map(values);
    let timerId = 0;
    let app;
    let rendered = 0;
    let bossMeasures = 0;
    const body = element();
    const root = element();
    const meter = element();
    const settingsPanel = element();
    const dropdowns = new Map();
    nodes.set(".meter", meter);
    nodes.set(".settingsPanel", settingsPanel);
    nodes.set(".container", element());
    for (const name of ["meterLayout", "theme", "language", "defaultMeterMode", "trainSelectionMode", "allTargetsWindow", "targetWindow", "playerLimit"]) {
      const button = element("button");
      const text = element("span");
      text.className = "settingsDropdownText";
      button.appendChild(text);
      const menu = element();
      const wrapper = element();
      button.className = `${name}DropdownBtn`;
      menu.className = `${name}DropdownMenu`;
      wrapper.appendChild(button);
      wrapper.appendChild(menu);
      nodes.set(`.${name}DropdownBtn`, button);
      nodes.set(`.${name}DropdownMenu`, menu);
      nodes.set(`.${name}DropdownWrapper`, wrapper);
      dropdowns.set(name, { button, text, menu });
    }
    for (const [name, min, max] of [["meterOpacity", 10, 100], ["windowOpacity", 0, 100]]) {
      const input = element("input");
      Object.assign(input, { type: "range", min: String(min), max: String(max) });
      nodes.set(`.${name}Input`, input);
      nodes.set(`.${name}Value`, element("span"));
      settingsPanel.appendChild(input);
    }
    const character = element("input");
    nodes.set(".characterNameInput", character);
    const translated = element("span");
    translated.dataset.i18n = "remote.label";
    const document = {
      readyState: "loading", activeElement: null, body, documentElement: root,
      baseURI: "http://localhost/", head: { appendChild() {} },
      addEventListener: (name, callback) => {
        const callbacks = domHandlers.get(name) || [];
        callbacks.push(callback);
        domHandlers.set(name, callbacks);
      },
      createElement: element,
      querySelector: (selector) => nodes.get(selector) || null,
      querySelectorAll: (selector) => selector === "[data-i18n]" ? [translated]
        : selector === ".settingsDropdownMenu.isOpen"
          ? [...dropdowns.values()].map((dropdown) => dropdown.menu).filter((menu) => menu.classList.contains("isOpen")) : [],
    };
    character.blur = () => { document.activeElement = null; };
    for (const name of ["meterOpacity", "windowOpacity"]) nodes.get(`.${name}Input`).blur = () => { document.activeElement = null; };
    const boss = element("span");
    boss.parentElement = { clientWidth: 270 };
    Object.defineProperty(boss, "scrollWidth", { get: () => parseFloat(boss.style.fontSize || (meter.classList.contains("slim") ? "12" : "15")) * 20 });
    Object.defineProperties(meter, {
      offsetWidth: { get: () => body.classList.contains("legacyUi") ? 420 : 380 },
      offsetHeight: { get: () => (meter.classList.contains("slim") ? 22 : 31) * (app?.playerLimit || 6) + 50 },
      scrollHeight: { get: () => meter.offsetHeight },
    });
    const window = {
      __A2_VIEW__: view, location: { search: "", href: "http://localhost/" },
      __A2_WINDOW_STARTUP__: { loadsHidden: view === "settings", reusesSettings: view === "settings" },
      devicePixelRatio: 1, screen: { availWidth: 1920, availHeight: 1080 },
      addEventListener(name, callback) {
        const callbacks = events.get(name) || [];
        callbacks.push(callback);
        events.set(name, callbacks);
      },
      dispatchEvent: (event) => events.get(event.type)?.forEach((callback) => callback(event)),
      __TAURI__: {
        event: { listen: (name, callback) => { listeners.set(name, callback); return Promise.resolve(() => {}); } },
        opener: { open() {} },
        window: { getCurrentWindow: () => ({ label: view, show() {}, setFocus() {} }) },
        core: { invoke: (command, args) => {
          calls.push({ view, command, args });
          if (command === "update_settings") {
            if (values.get(args.key) !== args.value) { values.set(args.key, args.value); broadcast(args.key, args.value); }
          }
          if (["reset_dps", "restart_target_selection"].includes(command)) assert.fail(`combat reset by ${view}`);
          if (command === "get_settings") return Promise.resolve(Object.fromEntries(values));
          if (command === "tool_window_ready") return Promise.resolve(true);
          if (command === "get_available_devices" || command === "list_monitors") return Promise.resolve([]);
          return Promise.resolve(null);
        } },
      },
    };
    const context = vm.createContext({
      window, document, Event, URL, URLSearchParams, TextDecoder, navigator: { userAgent: "" },
      localStorage: {
        getItem: (key) => storage.get(key) ?? null,
        setItem: (key, value) => storage.set(key, String(value)),
        removeItem: (key) => storage.delete(key), clear: () => storage.clear(),
      },
      console: { log() {}, error() {}, warn() {} },
      setTimeout: () => ++timerId, clearTimeout() {}, requestAnimationFrame() {},
      setInterval: (callback, ms) => { timers.set(++timerId, { callback, ms }); return timerId; },
      clearInterval: (id) => timers.delete(id), MutationObserver: class { observe() {} },
      getComputedStyle: (node) => {
        if (node === boss) { bossMeasures++; return { fontSize: boss.style.fontSize || (meter.classList.contains("slim") ? "12px" : "15px") }; }
        return { getPropertyValue: (key) => root.style.getPropertyValue(key) || ({
          "--meter-fill-opacity": "0.8", "--text-color": "#ffffff", "--player-name-shadow": "none",
          "--row-fill": root.dataset.theme === "frost" ? "#d8ecff" : "#303040",
        }[key] || "") };
      },
      fetch: async (url) => {
        const language = String(url).match(/\/([^/]+)\.json$/)?.[1] || "en";
        const response = { ok: true, arrayBuffer: async () => new TextEncoder().encode(JSON.stringify({ remote: { label: `translated-${language}` } })).buffer };
        if (language === holdLanguage) return new Promise((resolve) => heldLanguages.push(() => resolve(response)));
        return response;
      },
    });
    const surface = { view, window, document, nodes, dropdowns, listeners, timers, body, meter, root, boss, translated, domHandlers, get rendered() { return rendered; }, get bossMeasures() { return bossMeasures; } };
    windows.push(surface);
    vm.runInContext(bridge, context);
    await window.a2SettingsReady;
    vm.runInContext(i18n, context);
    await window.i18n.init();
    vm.runInContext(core, context);
    app = window.dpsApp;
    surface.app = app;
    window._dpsApp = app;
    app.playerLimit = 6;
    app.targetModeBtn = element("button");
    app.meterUI = { updateFromRows: () => rendered++, onResetMeterUi: () => assert.fail("meter reset") };
    if (view === "main") {
      // This fixture owns the form and meter state; combat polling starts in
      // the full main startup, which these setting events must not invoke.
      app.fetchDps = () => {};
      app.elBossName = boss;
      app.setupSettingsPanel();
      app.fetchDps = () => assert.fail("remote frontend fetched combat");
    } else {
      app.start();
      await window.javaBridge.notifyUiReady();
    }
    await settle();
    const snapshot = [{ id: 7, name: "Player", totalDamage: 100, dps: 10 }];
    app.lastSnapshot = snapshot;
    app.lastJson = "combat snapshot";
    app.lastTargetId = 13;
    app.refreshDamageData = app.reinitTargetSelection = () => assert.fail("combat reset");
    app.setupSettingsPanel = () => assert.fail("form rewired");
    surface.assertCombatIntact = () => {
      assert.equal(app.lastSnapshot, snapshot);
      assert.equal(app.lastJson, "combat snapshot");
      assert.equal(app.lastTargetId, 13);
    };
    surface.pick = (name, value) => {
      const item = dropdowns.get(name).menu.children.find((entry) => entry.dataset.value === String(value));
      assert.ok(item, `missing ${name} choice ${value}`);
      item.emit("click");
    };
    surface.edit = (name, value) => { const input = nodes.get(`.${name}Input`); input.value = String(value); input.emit("input"); };
    return surface;
  }
  const main = await makeWindow("main");
  const settings = await makeWindow("settings");
  calls.length = 0;
  broadcasts.length = 0;
  return { main, settings, values, calls, broadcasts, heldLanguages, writes: () => calls.filter((call) => call.command === "update_settings") };
}

function assertSelected(surface, name, value) {
  const dropdown = surface.dropdowns.get(name);
  const selected = dropdown.menu.children.filter((item) => item.classList.contains("isActive"));
  assert.equal(selected.length, 1);
  assert.equal(selected[0].dataset.value, String(value));
  assert.equal(dropdown.text.textContent, selected[0].textContent);
}

test("both layout dropdown choices (Standard, Slim) round-trip to main classes and geometry without echoes or resets", async () => {
  const fixture = await pair();
  // The Classic skins are retired: Standard ("beta") and Slim ("betaSlim") are all there is.
  assert.deepEqual(fixture.settings.dropdowns.get("meterLayout").menu.children.map((item) => item.dataset.value), ["beta", "betaSlim"]);
  const sequence = ["betaSlim", "beta", "betaSlim", "betaSlim", "beta", "beta", "betaSlim", "beta"];
  for (const value of sequence) {
    fixture.settings.pick("meterLayout", value);
    await settle();
    const beta = value.startsWith("beta");
    const slim = value.endsWith("Slim");
    for (const surface of [fixture.main, fixture.settings]) {
      assert.equal(surface.app.getMeterLayout(), value);
      assert.equal(surface.body.classList.contains("legacyUi"), !beta);
      assert.equal(surface.meter.classList.contains("slim"), slim);
      assertSelected(surface, "meterLayout", value);
      surface.assertCombatIntact();
    }
    const size = fixture.calls.filter((call) => call.command === "resize_window").at(-1);
    assert.equal(size.view, "main");
    assert.equal(size.args.width, beta ? 396 : 436);
    assert.equal(size.args.height, (slim ? 22 : 31) * 6 + 60);
  }
  assert.equal(fixture.writes().length, sequence.length * 2, "only the two originating settings writes per choice");
  assert.ok(fixture.writes().every((call) => call.view === "settings"));
  assert.equal(fixture.settings.bossMeasures, 0, "Settings never measures removed combat markup");
});

test("a slim layout refits a long boss name after applying the new density", async () => {
  const { main, settings } = await pair();
  main.app.fitBossName();
  assert.equal(main.boss.style.fontSize, "13.5px");
  settings.pick("meterLayout", "betaSlim");
  await settle();
  assert.equal(main.boss.style.fontSize, "", "the old fitted size cannot override the smaller slim CSS size");
  settings.pick("meterLayout", "beta");
  await settle();
  assert.equal(main.boss.style.fontSize, "13.5px");
});

test("theme and player limit clicks update the other window's existing controls and redraw the meter", async () => {
  const fixture = await pair();
  const themeItems = [...fixture.main.dropdowns.get("theme").menu.children];
  const renders = fixture.main.rendered;
  fixture.settings.pick("theme", "frost");
  fixture.settings.pick("playerLimit", "10");
  await settle();
  assert.equal(fixture.main.root.dataset.theme, "frost");
  assert.equal(fixture.main.app.playerLimit, 10);
  assert.ok(fixture.main.rendered > renders);
  assertSelected(fixture.main, "theme", "frost");
  assertSelected(fixture.main, "playerLimit", "10");
  assert.deepEqual(fixture.main.dropdowns.get("theme").menu.children, themeItems, "remote changes do not rebuild menus");
  assert.equal(fixture.main.dropdowns.get("theme").button.style.background, "#d8ecff");
  assert.equal(fixture.writes().length, 2);
  assert.ok(fixture.writes().every((call) => call.view === "settings"));
  fixture.main.assertCombatIntact();
});

test("actual opacity input handlers apply remote CSS, labels and range fill without persisting echoes", async () => {
  const fixture = await pair();
  fixture.settings.edit("meterOpacity", 55);
  fixture.settings.edit("windowOpacity", 20);
  await settle();
  assert.equal(fixture.main.root.style.getPropertyValue("--meter-fill-opacity"), "0.55");
  assert.equal(fixture.main.root.style.getPropertyValue("--window-opacity"), "0.2");
  assert.equal(fixture.main.nodes.get(".meterOpacityInput").value, "55");
  assert.equal(fixture.main.nodes.get(".meterOpacityValue").textContent, "55%");
  assert.equal(fixture.main.nodes.get(".meterOpacityInput").style.getPropertyValue("--range-pct"), "50%");
  assert.equal(fixture.main.nodes.get(".windowOpacityInput").value, "20");
  assert.equal(fixture.main.nodes.get(".windowOpacityValue").textContent, "20%");
  assert.equal(fixture.writes().length, 2);
  fixture.settings.edit("meterOpacity", 55);
  await settle();
  assert.equal(fixture.writes().length, 3);
  assert.equal(fixture.broadcasts.length, 2, "the mock Rust broadcast emits only changed values");
  assert.ok(fixture.writes().every((call) => call.view === "settings"));
  fixture.main.assertCombatIntact();
});

test("remote appearance updates and form refill preserve a focused slider or character draft", async () => {
  const { main, settings, writes } = await pair();
  const input = settings.nodes.get(".meterOpacityInput");
  const label = settings.nodes.get(".meterOpacityValue");
  input.value = "91";
  label.textContent = "91%";
  input.style.setProperty("--range-pct", "90%");
  settings.document.activeElement = input;
  main.window.javaBridge.setSetting("dpsMeter.meterFillOpacity", "55");
  await settle();
  await settings.app.syncSettingsForm();
  assert.equal(input.value, "91");
  assert.equal(label.textContent, "91%");
  assert.equal(input.style.getPropertyValue("--range-pct"), "90%");
  assert.equal(main.root.style.getPropertyValue("--meter-fill-opacity"), "0.55");
  assert.equal(writes().length, 1);
  const character = settings.nodes.get(".characterNameInput");
  character.value = "Typing a name";
  settings.document.activeElement = character;
  main.window.javaBridge.setSetting("dpsMeter.userName", "Saved name");
  await settle();
  await settings.app.syncSettingsForm();
  assert.equal(character.value, "Typing a name");
  assert.equal(settings.app.USER_NAME, "Saved name");
  assert.equal(writes().length, 2);
  main.assertCombatIntact();
});

test("mode and time-window dropdowns synchronize frontend selection without replaying backend commands", async () => {
  const fixture = await pair();
  fixture.settings.pick("defaultMeterMode", "trainTargets");
  fixture.settings.pick("trainSelectionMode", "highestDamage");
  fixture.settings.pick("allTargetsWindow", "60000");
  await settle();
  assert.equal(fixture.main.app.targetSelection, "trainTargets");
  assert.equal(fixture.main.app.targetModeBtn.textContent, "TRAIN");
  assert.equal(fixture.main.app.targetModeBtn.classList.contains("isTrainTargets"), true);
  assert.equal(fixture.main.app.trainSelectionMode, "highestDamage");
  fixture.main.app.lastTargetMode = "trainTargets";
  const options = fixture.main.app.getDefaultDetailsOpenOptions();
  assert.equal(options.defaultTargetAll, false);
  assert.equal(options.defaultTargetId, 13);
  assert.equal(fixture.main.app.settingsSelections.allTargetsWindowMs, "60000");
  for (const [name, value] of [["defaultMeterMode", "trainTargets"], ["trainSelectionMode", "highestDamage"], ["allTargetsWindow", "60000"]]) assertSelected(fixture.main, name, value);
  assert.equal(fixture.writes().length, 3);
  assert.deepEqual(fixture.calls.filter((call) => call.command === "set_target_mode").map((call) => call.view), ["settings"]);
  fixture.main.assertCombatIntact();
});

test("a language click updates the other window's actual i18n without a second settings write", async () => {
  const fixture = await pair();
  fixture.settings.pick("language", "ru");
  await settle();
  for (const surface of [fixture.main, fixture.settings]) {
    assert.equal(surface.app.i18n.getLanguage(), "ru");
    assert.equal(surface.root.getAttribute("lang"), "ru");
    assert.equal(surface.translated.textContent, "translated-ru");
    assertSelected(surface, "language", "ru");
    surface.assertCombatIntact();
  }
  assert.equal(fixture.writes().length, 1);
  assert.equal(fixture.writes()[0].view, "settings");
  assert.deepEqual(fixture.calls.filter((call) => call.command === "set_language").map((call) => call.view), ["settings"]);
});

test("a late dictionary response cannot restore an older remotely selected language", async () => {
  const fixture = await pair({ holdLanguage: "ru" });
  fixture.settings.pick("language", "ru");
  await settle();
  fixture.settings.pick("language", "de");
  await settle();
  fixture.heldLanguages.forEach((release) => release());
  await settle();
  for (const surface of [fixture.main, fixture.settings]) {
    assert.equal(surface.app.i18n.getLanguage(), "de");
    assert.equal(surface.root.getAttribute("lang"), "de");
    assert.equal(surface.translated.textContent, "translated-de");
    assertSelected(surface, "language", "de");
  }
  assert.equal(fixture.writes().length, 2);
});

test("hidden settings stays paused and reopen refills newer values without writes or duplicate handlers", async () => {
  const fixture = await pair();
  const input = fixture.settings.nodes.get(".windowOpacityInput");
  const handlers = [...input.handlers.entries()].map(([name, list]) => [name, list.length]);
  const outsideHandlers = fixture.settings.domHandlers.get("click").length;
  fixture.settings.listeners.get("settings-hidden")();
  assert.equal(fixture.settings.timers.size, 0);
  fixture.main.window.javaBridge.setSetting("dpsMeter.theme", "frost");
  fixture.main.window.javaBridge.setSetting("dpsMeter.playerLimit", "12");
  await settle();
  assert.equal(fixture.settings.timers.size, 0);
  fixture.values.set("dpsMeter.windowOpacity", "33");
  fixture.settings.listeners.get("settings-shown")();
  fixture.settings.listeners.get("settings-shown")();
  await settle();
  assert.equal(fixture.settings.timers.size, 1);
  assert.equal(input.value, "33");
  assert.equal(fixture.settings.nodes.get(".windowOpacityValue").textContent, "33%");
  assert.equal(fixture.settings.app.playerLimit, 12);
  assertSelected(fixture.settings, "theme", "frost");
  assertSelected(fixture.settings, "playerLimit", "12");
  assert.equal(fixture.writes().length, 2, "only the two edits while hidden, no refill persistence");
  assert.ok(fixture.writes().every((call) => call.view === "main"));
  assert.deepEqual([...input.handlers.entries()].map(([name, list]) => [name, list.length]), handlers);
  assert.equal(fixture.settings.domHandlers.get("click").length, outsideHandlers);
  fixture.main.assertCombatIntact();
});
