import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("application and installer versions retain the upstream plain version", () => {
  const app = JSON.parse(read("src-tauri/tauri.conf.json"));
  const pkg = JSON.parse(read("package.json"));
  const npm = JSON.parse(read("package-lock.json"));
  assert.match(app.version, /^\d+\.\d+\.\d+$/);
  assert.equal(pkg.version, app.version);
  assert.equal(npm.version, app.version);
  assert.equal(npm.packages[""].version, app.version);
  assert.equal(app.bundle.windows.wix.version, undefined);
  assert.equal(app.productName, "A2Tools DPS Meter");
  assert.equal(app.app.windows[0].title, app.productName);
  assert.equal(app.identifier, "com.a2tools.dps-meter");
  assert.equal(app.bundle.publisher, undefined);
  assert.equal(app.bundle.windows.wix.upgradeCode, undefined);
  const manifest = read("src-tauri/Cargo.toml");
  assert.equal(/^version = "([^"]+)"$/m.exec(manifest)?.[1], app.version);
  for (const path of ["src-tauri/Cargo.lock", "log-service/Cargo.lock", "tools/slice-v3/Cargo.lock"]) {
    assert.equal(/name = "a2tools-dps-meter"\r?\nversion = "([^"]+)"/.exec(read(path))?.[1], app.version);
  }
});

test("a newer official release can still be offered and accepted", { timeout: 1000 }, async () => {
  const timers = [];
  const requests = [];
  const window = { __TAURI__: { core: { invoke: async (command, args) => {
    assert.equal(command, "check_for_update");
    requests.push(args.ask);
    return { state: args.ask ? "installing" : "offer", latest: "2.0.87" };
  } } } };
  vm.runInNewContext(read("public/src/js/checkRelease.js"), {
    window, console, setTimeout: (fn) => timers.push(fn), setInterval() {},
  });
  const offered = [];
  let announce;
  const available = new Promise((resolve) => { announce = resolve; });
  window.ReleaseChecker.start((version) => { offered.push(version); announce(); });
  timers[0]();
  await available;
  assert.deepEqual(offered, ["v2.0.87"]);
  assert.equal(await window.ReleaseChecker.offer(), true);
  assert.deepEqual(requests, [false, true]);
  assert.equal(await window.ReleaseChecker.offer(), false);
});
