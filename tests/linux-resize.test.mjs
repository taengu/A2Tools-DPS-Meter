import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../public/src/js/tauriBridge.js", import.meta.url), "utf8");
const startup = source.slice(source.indexOf("  const { invoke }"), source.indexOf("  // Three windows"));
const overlay = source.slice(source.indexOf("  // ===== Overlay resize handle"), source.indexOf("  // Startup diagnostics"));
const tool = source.slice(source.indexOf("  // ===== Tool windows: drag by the header"), source.indexOf("  // Pre-fetch device list"));
const tick = () => new Promise((resolve) => setImmediate(resolve));

function setup({ userAgent = "Linux", supported = false, view = "main", detect } = {}) {
  const calls = [];
  const nativeResizes = [];
  const events = new Map();
  const classes = new Set();
  const document = {
    readyState: "complete",
    body: { classList: { add: (value) => classes.add(value) } },
    documentElement: { classList: { add() {}, remove() {}, contains: (name) => name === "linux" && /Linux/.test(userAgent) } },
    head: { appendChild() {} },
    createElement: () => ({}),
    addEventListener(name, handler) {
      const handlers = events.get(name) || [];
      handlers.push(handler);
      events.set(name, handlers);
    },
  };
  const window = {
    A2_VIEW: view, devicePixelRatio: 1,
    __TAURI__: {
      core: { invoke: (command, args) => {
        // Every page asks which optional parts the build has; not a window call.
        if (command === "build_features") return Promise.resolve({ online: true });
        calls.push({ command, args });
        return command === "compositor_resize_supported" && detect
          ? detect : Promise.resolve(command === "compositor_resize_supported" ? supported : null);
      } },
      event: { listen() {} }, opener: { open() {} },
      window: { getCurrentWindow: () => ({ startResizeDragging: (direction) => calls.push({ command: "startResizeDragging", direction }) }) },
    },
  };
  const context = vm.createContext({
    window, document, navigator: { userAgent }, console, Node: { TEXT_NODE: 3 },
    resizeActive: false, nativeResize: null, primaryHeld: true, lastSizeKey: "",
    spaceRightBelow: () => ({ w: 1920, h: 1080 }),
    overlayPadding: () => ({ w: 16, h: 10 }),
    startNativeResize: (...args) => nativeResizes.push(args),
    MouseEvent: class { constructor(type, event) { Object.assign(this, event); this.type = type; } },
  });
  vm.runInContext(startup, context);
  vm.runInContext(tool, context);
  vm.runInContext(overlay, context);
  const emit = (name, event) => (events.get(name) || []).forEach((handler) => handler(event));
  return { calls, nativeResizes, classes, emit, context };
}

function pressHandle() {
  return {
    button: 0,
    target: { closest: (selector) => selector === ".resizeHandle" },
    preventDefault() { this.prevented = true; },
    stopImmediatePropagation() { this.stopped = true; },
  };
}

test("Windows skips Linux capability detection and preserves an immediate resize press", async () => {
  const app = setup({ userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" });
  assert.equal(app.calls.length, 0);
  const press = pressHandle();
  app.emit("mousedown", press);
  assert.equal(press.prevented, undefined);
  assert.equal(press.stopped, undefined);
  assert.equal(app.calls[0].command, "resize_window");
  assert.equal(app.calls[0].args.width, 1920);
  await tick();
  assert.equal(app.nativeResizes.length, 0);
});

test("Windows tool windows drag from the header by the page, not by app-region", () => {
  // -webkit-app-region made WebView2 move a helper window on every layout
  // change, and that froze the whole app for seconds at a time.
  const app = setup({ userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)", view: "settings" });
  const header = (inButton) => ({
    button: 0, clientX: 200, clientY: 20,
    target: { closest: (selector) => selector.includes(".settingsHeader") || (inButton && selector.includes("button")) },
    preventDefault() { this.prevented = true; },
    stopImmediatePropagation() { this.stopped = true; },
  });
  const press = header(false);
  app.emit("mousedown", press);
  assert.equal(app.calls.at(-1)?.command, "start_tool_drag");
  assert.equal(press.prevented, true);
  const before = app.calls.length;
  const onButton = header(true);
  app.emit("mousedown", onButton);
  assert.equal(app.calls.length, before, "a header button is clicked, not dragged");
  assert.equal(onButton.prevented, undefined);
});

test("other Linux desktops keep viewport resizing without GNOME styling", async () => {
  const app = setup({ supported: false });
  await tick();
  const press = pressHandle();
  app.emit("mousedown", press);
  assert.equal(press.stopped, undefined);
  assert.equal(app.calls.at(-1).command, "resize_window");
  assert.equal(app.nativeResizes.length, 0);
  assert.equal(app.classes.has("linuxOverlay"), false);
});

test("other Linux desktops keep the begin_tool_resize edge path", async () => {
  const app = setup({ supported: false, view: "details" });
  await tick();
  app.emit("mousedown", {
    ...pressHandle(), clientX: 1, clientY: 1,
    target: { closest: () => false },
  });
  await tick();
  assert.equal(app.calls.at(-2).command, "begin_tool_resize");
  assert.equal(app.calls.at(-2).args.minWidth, 520);
  assert.equal(app.calls.at(-1).command, "startResizeDragging");
  assert.equal(app.calls.at(-1).direction, "NorthWest");
  assert.equal(app.nativeResizes.length, 0);
});

test("GNOME uses compositor resizing for the overlay and tool edges", async () => {
  const overlayApp = setup({ supported: true });
  await tick();
  const press = pressHandle();
  overlayApp.emit("mousedown", press);
  await tick();
  assert.equal(press.stopped, true);
  assert.deepEqual(overlayApp.nativeResizes[0], ["SouthEast", 316, 40]);
  assert.equal(overlayApp.classes.has("linuxOverlay"), true);
  assert.equal(overlayApp.calls.some((call) => call.command === "resize_window"), false);

  const toolApp = setup({ supported: true, view: "details" });
  await tick();
  toolApp.emit("mousedown", { ...pressHandle(), clientX: 1, clientY: 1, target: { closest: () => false } });
  await tick();
  assert.deepEqual(toolApp.nativeResizes[0], ["NorthWest", 520, 360]);
  assert.equal(toolApp.calls.some((call) => call.command === "begin_tool_resize"), false);
});

test("a released early press does not start resizing after Linux detection settles", async () => {
  let resolve;
  const app = setup({ detect: new Promise((done) => { resolve = done; }) });
  app.emit("mousedown", pressHandle());
  vm.runInContext("primaryHeld = false", app.context);
  resolve(true);
  await tick();
  assert.equal(app.nativeResizes.length, 0);
});

test("an early press on another Linux desktop replays the original viewport path", async () => {
  let resolve;
  const app = setup({ detect: new Promise((done) => { resolve = done; }) });
  const press = pressHandle();
  press.target.dispatchEvent = (event) => app.emit(event.type, event);
  app.emit("mousedown", press);
  assert.equal(press.stopped, true);
  resolve(false);
  await tick();
  assert.equal(app.calls.at(-1).command, "resize_window");
  assert.equal(app.nativeResizes.length, 0);
});

test("failed Linux capability detection falls back to the existing resize path", async () => {
  const app = setup({ detect: Promise.reject(new Error("IPC failed")) });
  await tick();
  const press = pressHandle();
  app.emit("mousedown", press);
  assert.equal(press.stopped, undefined);
  assert.equal(app.calls.at(-1).command, "resize_window");
  assert.equal(app.classes.has("linuxOverlay"), false);
});
