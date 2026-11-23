import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

// The fight timeline's zoom maths live in buffTimeline.js (window.ftlZoom),
// beside the buff bars that follow them; details.js keeps the view.
const source = readFileSync(new URL("../public/src/js/buffTimeline.js", import.meta.url), "utf8");
const context = vm.createContext({ window: {} });
vm.runInContext(source, context);
const zoom = context.window.ftlZoom;
const plain = (value) => JSON.parse(JSON.stringify(value));

const FIGHT = 600000; // a 10-minute fight (Ludra)

test("the view stays inside the fight, no narrower than ten seconds", () => {
  assert.deepEqual(plain(zoom.clampView({ from: -5000, to: 20000 }, FIGHT)), { from: 0, to: 25000 });
  assert.deepEqual(plain(zoom.clampView({ from: 590000, to: 620000 }, FIGHT)), { from: 570000, to: 600000 });
  assert.deepEqual(plain(zoom.clampView({ from: 100000, to: 101000 }, FIGHT)), { from: 100000, to: 110000 });
  assert.deepEqual(plain(zoom.clampView(null, FIGHT)), { from: 0, to: FIGHT });
  // A fight shorter than the least span: all of it.
  assert.deepEqual(plain(zoom.clampView({ from: 2000, to: 3000 }, 8000)), { from: 0, to: 8000 });
  assert.deepEqual(plain(zoom.clampView({ from: 0, to: 10 }, 0)), { from: 0, to: 0 });
});

test("zooming keeps the moment under the pointer where it is", () => {
  const full = { from: 0, to: FIGHT };
  const v = zoom.zoomView(full, 4, 300000, FIGHT);
  assert.deepEqual(plain(v), { from: 225000, to: 375000 });
  // A moment a quarter in stays a quarter in.
  const w = zoom.zoomView(full, 2, 150000, FIGHT);
  assert.equal((150000 - w.from) / (w.to - w.from), 0.25);
  // Without a moment, about the middle; zooming out again comes back to Fit.
  assert.deepEqual(plain(zoom.zoomView(full, 2, null, FIGHT)), { from: 150000, to: 450000 });
  assert.ok(zoom.isFit(zoom.zoomView(v, 0.1, 300000, FIGHT), FIGHT));
  // No closer than ten seconds: 60x for ten minutes.
  const close = zoom.zoomView(full, 1000, 120000, FIGHT);
  assert.equal(close.to - close.from, 10000);
  assert.equal(zoom.maxZoom(FIGHT), 60);
  assert.equal(zoom.zoomLevel(close, FIGHT), 60);
  assert.equal(zoom.maxZoom(8000), 1);
});

test("moving the view keeps its span and stops at the fight's ends", () => {
  const v = { from: 100000, to: 160000 };
  assert.deepEqual(plain(zoom.panView(v, 30000, FIGHT)), { from: 130000, to: 190000 });
  assert.deepEqual(plain(zoom.panView(v, -500000, FIGHT)), { from: 0, to: 60000 });
  assert.deepEqual(plain(zoom.panView(v, 900000, FIGHT)), { from: 540000, to: 600000 });
});

test("a replay's moment leaving the view brings the view to it", () => {
  const v = { from: 100000, to: 160000 };
  assert.deepEqual(plain(zoom.followView(v, 130000, FIGHT)), v);
  // Past the right edge: the moment a tenth in, the rest ahead of it.
  assert.deepEqual(plain(zoom.followView(v, 170000, FIGHT)), { from: 164000, to: 224000 });
  assert.deepEqual(plain(zoom.followView(v, 20000, FIGHT)), { from: 14000, to: 74000 });
  assert.deepEqual(plain(zoom.followView(v, 599000, FIGHT)), { from: 540000, to: 600000 });
});

test("axis ticks take a round step for the span shown, down to a second", () => {
  assert.equal(zoom.tickStepMs(FIGHT, 10), 60000);
  assert.equal(zoom.tickStepMs(FIGHT, 14), 60000);
  assert.equal(zoom.tickStepMs(150000, 10), 15000);
  assert.equal(zoom.tickStepMs(30000, 10), 5000);
  assert.equal(zoom.tickStepMs(10000, 10), 1000);
  assert.equal(zoom.tickStepMs(10000, 4), 5000);

  // Fit: every minute, and the end labelled (9:00, 20 s from it, goes).
  const fit = zoom.viewTicks({ from: 0, to: 560000 }, 560000, 10);
  assert.deepEqual(plain(fit.map((t) => t.ms)), [0, 60000, 120000, 180000, 240000, 300000, 360000, 420000, 480000, 560000]);
  assert.equal(fit[fit.length - 1].isEnd, true);
  assert.equal(fit[fit.length - 1].pct, 100);

  // Zoomed to ten seconds in the middle: every second, at their place in the
  // lanes; no end mark, the end is not in view.
  const near = zoom.viewTicks({ from: 123500, to: 133500 }, FIGHT, 10);
  assert.deepEqual(plain(near.map((t) => t.ms)), [124000, 125000, 126000, 127000, 128000, 129000, 130000, 131000, 132000, 133000]);
  assert.equal(near[0].pct, 5);
  assert.ok(near.every((t) => !t.isEnd));
  assert.deepEqual(plain(zoom.viewTicks({ from: 0, to: 0 }, FIGHT)), []);
});

test("lane marks follow the zoom by two style properties, plain percent without them", () => {
  assert.equal(zoom.leftCss(12.5), "calc((12.5 - var(--ftl-from, 0)) * var(--ftl-zoom, 1) * 1%)");
  assert.equal(zoom.widthCss(3), "calc(3 * var(--ftl-zoom, 1) * 1%)");
  assert.equal(zoom.leftCss(undefined), "calc((0 - var(--ftl-from, 0)) * var(--ftl-zoom, 1) * 1%)");
  // The stagger lanes use the same expression.
  const stagger = readFileSync(new URL("../public/src/js/staggerPanel.js", import.meta.url), "utf8");
  assert.match(stagger, /var\(--ftl-from, 0\)\) \* var\(--ftl-zoom, 1\) \* 1%/);
});

test("the zoom controls are in the timeline's bar, with strings in every UI language", () => {
  const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
  const bar = html.slice(html.indexOf('<div class="fightTimelineBar">'), html.indexOf('<div class="fightTimelineScroll">'));
  for (const k of ["out", "in", "fit"]) assert.match(bar, new RegExp(`data-ftl-zoom="${k}"`));
  assert.match(html, /class="fightTimelineZoomBar"/);
  const keys = [...html.matchAll(/data-i18n(?:-title|-aria-label)?="(details\.zoom\.\w+)"/g)].map((m) => m[1]);
  assert.ok(keys.length >= 6);

  const read = (lang) => JSON.parse(readFileSync(new URL(`../src/data/i18n/ui/${lang}.json`, import.meta.url), "utf8"));
  const en = read("en").details.zoom;
  for (const key of keys) assert.ok(en[key.split(".").pop()], key);
  for (const lang of ["de", "en", "es", "fr", "ja", "ko", "pt", "ru", "zh-Hans", "zh-Hant"]) {
    const strings = read(lang).details.zoom;
    assert.deepEqual(Object.keys(strings).sort(), Object.keys(en).sort(), lang);
    for (const [key, value] of Object.entries(strings)) assert.ok(typeof value === "string" && value.trim(), `${lang} ${key}`);
  }
});
