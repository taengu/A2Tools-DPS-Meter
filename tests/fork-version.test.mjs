import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("fork package, installer and lockfiles identify the same release", () => {
  const app = JSON.parse(read("src-tauri/tauri.conf.json"));
  const pkg = JSON.parse(read("package.json"));
  const npm = JSON.parse(read("package-lock.json"));
  assert.equal(app.version, "2.0.86+1");
  assert.equal(pkg.version, app.version);
  assert.equal(npm.version, app.version);
  assert.equal(npm.packages[""].version, app.version);
  assert.equal(app.bundle.windows.wix.version, "2.0.86.1");
  assert.match(app.productName, /Talal Fork/);
  assert.equal(app.bundle.publisher, "Talal Alyaseen");
  assert.match(read("src-tauri/Cargo.toml"), /^version = "2\.0\.86\+1"$/m);
  for (const path of ["src-tauri/Cargo.lock", "log-service/Cargo.lock", "tools/slice-v3/Cargo.lock"]) {
    assert.match(read(path), /name = "a2tools-dps-meter"\r?\nversion = "2\.0\.86\+1"/);
  }
});

test("fork update status reaches Settings without advertising an upstream installer", async () => {
  const timers = [];
  const window = { __TAURI__: { core: { invoke: async (command) => {
    assert.equal(command, "check_for_update");
    return { state: "fork", current: "2.0.86.1 (Talal Alyaseen fork)", message: "Use the fork releases." };
  } } } };
  vm.runInNewContext(read("public/src/js/checkRelease.js"), {
    window, console, setTimeout: (fn) => timers.push(fn), setInterval() {},
  });
  let offered = false;
  window.ReleaseChecker.start(() => { offered = true; });
  await timers[0]();
  await Promise.resolve();
  assert.equal(offered, false);
  const result = await window.ReleaseChecker.checkNow();
  assert.equal(result.state, "fork");
  assert.equal(result.message, "Use the fork releases.");
  assert.equal(result.latest, undefined);
  assert.equal(await window.ReleaseChecker.offer(), false);
});
