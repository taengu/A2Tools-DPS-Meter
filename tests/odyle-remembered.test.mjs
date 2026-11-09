import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/core.js", import.meta.url), "utf8");

function element() {
  const classes = new Set();
  return {
    textContent: "",
    classList: {
      add: (c) => classes.add(c),
      remove: (c) => classes.delete(c),
      toggle: (c, on) => (on ? classes.add(c) : classes.delete(c)),
      contains: (c) => classes.has(c),
    },
  };
}

// The meter window's Odyle display, with a localStorage that outlives it like
// the real one does across a meter restart.
function meter(storage) {
  const context = vm.createContext({
    console: { log() {} },
    localStorage: {
      getItem: (k) => (storage.has(k) ? storage.get(k) : null),
      setItem: (k, v) => storage.set(k, String(v)),
      removeItem: (k) => storage.delete(k),
    },
    window: { addEventListener() {} },
    document: {
      readyState: "loading", addEventListener() {},
      documentElement: { classList: { toggle() {} } }, body: { classList: { toggle() {} } },
    },
    requestAnimationFrame: () => 1, cancelAnimationFrame() {},
  });
  vm.runInContext(source, context);
  const app = vm.runInContext("new DpsApp()", context);
  const parts = { ".odyleBasic": element(), ".odyleCharged": element() };
  app.odyleEl = { ...element(), querySelector: (s) => parts[s] };
  app.showOdyle = true;
  const shown = () => ({
    visible: app.odyleEl.classList.contains("isVisible"),
    remembered: app.odyleEl.classList.contains("isRemembered"),
    text: parts[".odyleBasic"].textContent + parts[".odyleCharged"].textContent,
  });
  return { app, shown };
}

test("a restarted meter shows the character's last energy, dimmed, until the game sends it", () => {
  const storage = new Map();
  const first = meter(storage);
  first.app.USER_NAME = "Nullception";
  first.app.USER_SERVER = 2001;
  first.app.updateOdyle({ basic: 30, charged: 840, name: "Nullception", server: 2001 });
  first.app.updateOdyle({ basic: 30, charged: 880, name: "Nullception", server: 2001 });
  assert.deepEqual(first.shown(), { visible: true, remembered: false, text: "30(+880)" });

  const second = meter(storage);
  second.app.USER_NAME = "Nullception";
  second.app.USER_SERVER = 2001;
  second.app.updateOdyle();
  assert.deepEqual(second.shown(), { visible: true, remembered: true, text: "30(+880)" });

  second.app.updateOdyle({ basic: 45, charged: 880, name: "Nullception", server: 2001 });
  assert.deepEqual(second.shown(), { visible: true, remembered: false, text: "45(+880)" });
});

// A login's list comes before the record naming who logged in, so the
// backend sends it without a name: it is shown, but not kept for anyone.
test("energy without an owner is shown but not kept, and another character's isn't shown", () => {
  const storage = new Map();
  const first = meter(storage);
  first.app.USER_NAME = "Nullception";
  first.app.USER_SERVER = 2001;
  first.app.updateOdyle({ basic: 30, charged: 840, name: "Nullception", server: 2001 });
  first.app.updateOdyle({ basic: 12, charged: 0, name: null });
  assert.deepEqual(first.shown(), { visible: true, remembered: false, text: "12" });

  const second = meter(storage);
  second.app.USER_NAME = "Nullception";
  second.app.USER_SERVER = 2001;
  second.app.updateOdyle();
  assert.deepEqual(second.shown(), { visible: true, remembered: true, text: "30(+840)" });

  second.app.USER_NAME = "Alt";
  second.app.updateOdyle();
  assert.equal(second.shown().visible, false);
});

// One name can be a character on several servers: each keeps its own energy,
// and with no server known yet none is shown (#190).
test("a character of the same name on another server shows its own energy, or none", () => {
  const storage = new Map();
  const first = meter(storage);
  first.app.USER_NAME = "Denatril";
  first.app.USER_SERVER = 1304;
  first.app.updateOdyle({ basic: 15, charged: 695, name: "Denatril", server: 1304 });

  const second = meter(storage);
  second.app.USER_NAME = "Denatril";
  second.app.USER_SERVER = 1306;
  second.app.updateOdyle();
  assert.equal(second.shown().visible, false);
  second.app.USER_SERVER = 0;
  second.app.updateOdyle();
  assert.equal(second.shown().visible, false);
  second.app.USER_SERVER = 1304;
  second.app.updateOdyle();
  assert.deepEqual(second.shown(), { visible: true, remembered: true, text: "15(+695)" });
});

test("nothing remembered shows nothing, and the setting off hides it", () => {
  const m = meter(new Map());
  m.app.USER_NAME = "Nullception";
  m.app.USER_SERVER = 2001;
  m.app.updateOdyle();
  assert.equal(m.shown().visible, false);
  m.app.updateOdyle({ basic: 30, charged: 840, name: "Nullception", server: 2001 });
  m.app.showOdyle = false;
  m.app.updateOdyle();
  assert.equal(m.shown().visible, false);
});
