import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

// The DOM-free half of fightCompare.js.
const source = readFileSync(new URL("../public/src/js/fightCompare.js", import.meta.url), "utf8");
const context = vm.createContext({ module: { exports: {} } });
vm.runInContext(source, context);
const { buildCompareModel, cmpHeadToHead, cmpCompact, cmpActiveMs, cmpText } = context.module.exports;

const row = (actorId, code, dmg, time, extra = {}) => ({ actorId, code, name: `S${code}`, dmg, time, crit: 0, ...extra });
// `n` hits evenly from `from` ms, `step` apart.
const times = (n, from = 0, step = 1000) => Array.from({ length: n }, (_, i) => from + i * step);

const fight = (skills, actors, extra = {}) => ({
  id: "f", durationMs: 10_000, actors, details: { battleTime: 10_000, skills, ...extra },
});

test("members are ranked by damage with DPS and share", () => {
  const m = buildCompareModel(fight(
    [row(1, 10, 3000, 10, { crit: 4 }), row(2, 10, 6000, 20, { crit: 5 }), row(2, 11, 1000, 5)],
    [{ actorId: 1, nickname: "A", job: "Cleric" }, { actorId: 2, nickname: "B" }],
  ));
  assert.equal(m.members.map((x) => x.name).join(), "B,A");
  assert.equal(m.members[0].damage, 7000);
  assert.equal(m.members[0].dps, 700);
  assert.equal(m.members[0].hits, 25);
  assert.equal(m.members[0].flags.crit, 5);
  assert.equal(Math.round(m.members[0].share), 70);
  assert.equal(m.totalDamage, 10_000);
});

test("zero-damage rows and a record with no details do not break it", () => {
  assert.equal(buildCompareModel({}).members.length, 0);
  const m = buildCompareModel({ details: { skills: [row(1, 1, 0, 3)] }, actors: [] });
  assert.equal(m.members.length, 0);
});

test("pauses over 2.5 s are idle time, edges included", () => {
  assert.equal(cmpActiveMs([], 10_000), null);
  // Hits every second over the whole fight: no pause counts.
  assert.equal(cmpActiveMs(times(11, 0, 1000), 10_000), 10_000);
  // Hits only in the first 2 s: the 8 s after the last loses 8000 - 2500.
  assert.equal(cmpActiveMs([0, 1000, 2000], 10_000), 10_000 - 5500);
});

test("the factors split the gap exactly: uptime x pace x damage per hit", () => {
  const actors = [{ actorId: 1, nickname: "A", jobId: 11 }, { actorId: 2, nickname: "B", jobId: 11 }];
  // A: 10 hits of 300 over the whole fight. B: 5 hits of 200, all in the first 5 s.
  const m = buildCompareModel(fight([
    row(1, 10, 3000, 10, { hitTimestamps: times(10, 0, 1000) }),
    row(2, 10, 1000, 5, { hitTimestamps: times(5, 0, 1000) }),
  ], actors));
  const h = cmpHeadToHead(m, 1, 2);
  assert.equal(Math.round(h.gapPct), 200);
  assert.equal(h.factors.map((f) => f.key).join(), "uptime,pace,perHit");
  const sumX = h.factors.reduce((n, f) => n + f.x, 0);
  assert.ok(Math.abs(sumX - Math.log(3)) < 1e-9);
  assert.ok(Math.abs(h.factors.reduce((n, f) => n + f.weight, 0) - 1) < 1e-9);
  const perHit = h.factors.find((f) => f.key === "perHit");
  assert.equal(Math.round(perHit.effectPct), 50); // 300 vs 200
  assert.ok(h.factors.find((f) => f.key === "uptime").x > 0); // B stopped early
});

test("without hit times the split is hits x damage per hit", () => {
  const actors = [{ actorId: 1, nickname: "A" }, { actorId: 2, nickname: "B" }];
  const h = cmpHeadToHead(buildCompareModel(fight([row(1, 1, 400, 4), row(2, 2, 100, 2)], actors)), 1, 2);
  assert.equal(h.factors.map((f) => f.key).join(), "hits,perHit");
  assert.equal(h.sameClass, false);
  assert.equal(h.skills.a.length, 1);
  assert.equal(h.skills.b.length, 1);
});

test("same class: skills line up by name and the gap's skills say why", () => {
  const actors = [{ actorId: 1, nickname: "A", jobId: 11 }, { actorId: 2, nickname: "B", jobId: 11 }];
  const m = buildCompareModel(fight([
    row(1, 10, 1000, 10), row(2, 10, 500, 5), // more uses
    row(1, 11, 900, 3), row(2, 11, 300, 3), // harder hits
    row(1, 12, 800, 2), // B never used it
    row(2, 13, 50, 1), // B ahead here: not a driver of A's lead
  ], actors));
  const h = cmpHeadToHead(m, 1, 2);
  assert.equal(h.sameClass, true);
  assert.equal(h.skills.length, 4);
  const why = Object.fromEntries(h.drivers.map((d) => [d.key, d.cause]));
  assert.equal(why["S10::hit"], "hits");
  assert.equal(why["S11::hit"], "avg");
  assert.equal(why["S12::hit"], "unused");
  assert.equal(why["S13::hit"], undefined);
  // Swapped, B is behind: the same skills, from B's side.
  const back = cmpHeadToHead(m, 2, 1);
  assert.ok(back.gapPct < 0);
  assert.equal(back.drivers.length, 3);
  assert.equal(back.drivers[0].aheadIsA, false);
});

test("deaths add up the time down", () => {
  const actors = [{ actorId: 1, nickname: "A" }];
  const m = buildCompareModel(fight([row(1, 1, 100, 1)], actors, {
    deaths: [{ actorId: 1, tsMs: 2000, revivedMs: 5000 }, { actorId: 1, tsMs: 8000 }],
  }));
  assert.equal(m.members[0].deaths, 2);
  assert.equal(m.members[0].deadMs, 3000 + 2000);
});

test("picking the same player twice compares nothing", () => {
  const m = buildCompareModel(fight([row(1, 1, 100, 1)], [{ actorId: 1, nickname: "A" }]));
  assert.equal(cmpHeadToHead(m, 1, 1), null);
});

test("numbers are shown in the meter's k / m style", () => {
  assert.equal(cmpCompact(950), "950");
  assert.equal(cmpCompact(1234), "1.2k");
  assert.equal(cmpCompact(255_957_427), "256.0m");
  assert.equal(cmpCompact(2_500_000_000), "2.50b");
});


test("sentences fill their placeholders, in English without a translation", () => {
  const tx = cmpText();
  assert.equal(tx.button, "Compare two players");
  assert.equal(tx.ahead({ winner: "A", loser: "B", pct: "12.0%" }), "A out-damages B by 12.0%");
  assert.equal(tx.deaths({ name: "B", count: 2, sec: 30 }), "B died 2x (30s down)");
  assert.equal(tx.unused({ name: "B" }), "B never used it");
  assert.equal(tx.counter({ name: "B", list: "Pace" }), "B is ahead on: Pace");
  assert.equal(tx.mainCause({ list: "Uptime (+5.0%), Pace (+1.0%)" }), "Mainly: Uptime (+5.0%), Pace (+1.0%)");
});

const UI_LANGS = ["en", "ko", "ja", "de", "fr", "es", "pt", "ru", "zh-Hant", "zh-Hans"];
const uiJson = (lang) => JSON.parse(readFileSync(new URL(`../src/data/i18n/ui/${lang}.json`, import.meta.url), "utf8"));
const lookup = (dict) => (key, fallback) => key.split(".").reduce((o, k) => o?.[k], dict) ?? fallback;

test("a translation's sentences fill their placeholders too", () => {
  const translated = vm.createContext({ module: { exports: {} }, window: { i18n: { t: lookup(uiJson("ja")) } } });
  vm.runInContext(source, translated);
  const tx = translated.module.exports.cmpText();
  assert.equal(tx.button, "2人を比較");
  assert.equal(tx.ahead({ winner: "A", loser: "B", pct: "12.0%" }), "A が B を DPS で 12.0% 上回っています");
});

test("every UI language has every compare string, with the same placeholders", () => {
  const en = uiJson("en").details.compare;
  const holes = (v) => (v.match(/\{\w+\}/g) || []).sort().join();
  for (const lang of UI_LANGS) {
    const c = uiJson(lang).details?.compare;
    assert.ok(c, `${lang}: details.compare`);
    assert.deepEqual(Object.keys(c).sort(), Object.keys(en).sort(), lang);
    for (const k of Object.keys(en)) assert.equal(holes(c[k]), holes(en[k]), `${lang} ${k}`);
  }
  // The English in the code is the English in en.json.
  const tx = cmpText();
  for (const [k, v] of Object.entries(en)) {
    if (typeof tx[k] === "string") assert.equal(tx[k], v, k);
    else assert.equal(typeof tx[k], "function", k);
  }
});

test("no hard-coded CJK punctuation, arrows or em dashes in the view", () => {
  const code = source.replace(/^\s*\/\/.*$/gm, "");
  assert.doesNotMatch(code, /[—、。（）⇄]/);
});

test("skill variants with one name are one row, so a driver is not 'never used'", () => {
  const actors = [{ actorId: 1, nickname: "A", jobId: 11 }, { actorId: 2, nickname: "B", jobId: 11 }];
  // The same skill under variant codes (A and B picked different ones).
  const m = buildCompareModel(fight([
    row(1, 1001, 3000, 10, { name: "Slash" }),
    row(2, 1002, 1000, 10, { name: "Slash" }),
    row(1, 1003, 500, 5, { name: "Slash" }),
  ], actors));
  const a = m.members.find((x) => x.actorId === 1);
  assert.equal(a.skills.size, 1);
  assert.equal(a.skills.get("Slash::hit").hits, 15);
  const h = cmpHeadToHead(m, 1, 2);
  assert.equal(h.skills.length, 1);
  assert.equal(h.drivers.length, 1);
  assert.notEqual(h.drivers[0].cause, "unused");
});

test("a DoT and its direct hit stay two rows", () => {
  const m = buildCompareModel(fight([
    row(1, 10, 1000, 5, { name: "Burn" }),
    row(1, 10, 400, 20, { name: "Burn", isDot: true }),
  ], [{ actorId: 1, nickname: "A" }]));
  assert.deepEqual([...m.members[0].skills.keys()].sort(), ["Burn::dot", "Burn::hit"]);
});

test("DoT ticks stay out of uptime, pace and per hit, and in the damage", () => {
  const actors = [{ actorId: 1, nickname: "A", jobId: 11 }, { actorId: 2, nickname: "B", jobId: 11 }];
  // The same direct hits for both; A's DoT also ticks every second through
  // the second half, while A itself does nothing.
  const m = buildCompareModel(fight([
    row(1, 10, 1000, 5, { hitTimestamps: times(5, 0, 1000) }),
    row(1, 20, 1000, 5, { isDot: true, hitTimestamps: times(5, 5000, 1000) }),
    row(2, 10, 1000, 5, { hitTimestamps: times(5, 0, 1000) }),
  ], actors));
  const a = m.members.find((x) => x.actorId === 1);
  assert.equal(a.damage, 2000);
  assert.equal(a.hits, 10);
  assert.equal(a.directHits, 5);
  const h = cmpHeadToHead(m, 1, 2);
  const by = Object.fromEntries(h.factors.map((f) => [f.key, f]));
  assert.equal(h.factors.map((f) => f.key).join(), "uptime,pace,perHit,dots");
  assert.equal(by.uptime.x, 0);
  assert.equal(by.pace.x, 0);
  assert.equal(by.perHit.x, 0);
  assert.equal(Math.round(by.dots.a), 50);
  assert.ok(Math.abs(by.dots.x - Math.log(2)) < 1e-9);
  // Still an exact split of the whole gap.
  assert.ok(Math.abs(h.factors.reduce((n, f) => n + f.x, 0) - Math.log(2)) < 1e-9);
});

test("without DoTs there is no DoT factor", () => {
  const actors = [{ actorId: 1, nickname: "A" }, { actorId: 2, nickname: "B" }];
  const h = cmpHeadToHead(buildCompareModel(fight([row(1, 1, 400, 4), row(2, 2, 100, 2)], actors)), 1, 2);
  assert.ok(!h.factors.some((f) => f.key === "dots"));
});

// The view, on a small stand-in DOM.

class FakeClassList {
  constructor() { this.set = new Set(); }
  add(c) { this.set.add(c); }
  remove(c) { this.set.delete(c); }
  contains(c) { return this.set.has(c); }
  toggle(c, on) {
    const v = on === undefined ? !this.set.has(c) : !!on;
    if (v) this.set.add(c); else this.set.delete(c);
    return v;
  }
}
class FakeEl {
  constructor(cls = "") {
    this.className = cls;
    this.classList = new FakeClassList();
    this.dataset = {};
    this.style = {};
    this.children = [];
    this.attrs = {};
    this.handlers = {};
    this.innerHTML = "";
    this.title = "";
    this.scrollTop = 0;
  }
  prepend(el) { this.children.unshift(el); }
  querySelector(sel) {
    const want = sel.replace(/^\./, "");
    for (const c of this.children) {
      if (c.className === want) return c;
      const deep = c.querySelector(sel);
      if (deep) return deep;
    }
    return null;
  }
  setAttribute(k, v) { this.attrs[k] = String(v); }
  addEventListener(type, fn) { this.handlers[type] = fn; }
  click() { this.handlers.click?.({ target: this }); }
}

const attachView = (dict = {}) => {
  const panel = new FakeEl("detailsPanel");
  const controls = new FakeEl("detailsHeaderControls");
  const body = new FakeEl("detailsBody");
  panel.children.push(controls, body);
  const ctx = vm.createContext({
    module: { exports: {} },
    window: { i18n: { t: lookup(dict) } },
    document: { createElement: () => new FakeEl() },
  });
  vm.runInContext(source, ctx);
  let rec = null;
  const api = ctx.module.exports.attachCompare({ panel, record: () => rec });
  return { api, panel, btn: controls.children[0], view: body.children[0], setRecord: (r) => { rec = r; } };
};

const saved = () => ({
  ...fight([row(1, 10, 3000, 10), row(2, 10, 1000, 5)],
    [{ actorId: 1, nickname: "A<b>", jobId: 11 }, { actorId: 2, nickname: "B", jobId: 11 }]),
  localActorId: 1,
});

test("the button shows only for a saved fight, and reset closes the comparison", () => {
  const v = attachView();
  assert.equal(v.btn.style.display, "none");
  // No saved fight: clicking opens nothing.
  v.btn.click();
  assert.equal(v.panel.classList.contains("fightCompareOpen"), false);

  v.setRecord(saved());
  v.api.reset();
  assert.equal(v.btn.style.display, "");
  assert.equal(v.btn.title, "Compare two players");
  assert.equal(v.btn.attrs["aria-label"], "Compare two players");
  v.btn.click();
  assert.equal(v.panel.classList.contains("fightCompareOpen"), true);
  assert.ok(v.view.innerHTML.includes("A&lt;b&gt;"));

  // Details closes (details.js clears the record, then resets): nothing of
  // the comparison stays, so a live Details is not blanked.
  v.setRecord(null);
  v.api.reset();
  assert.equal(v.panel.classList.contains("fightCompareOpen"), false);
  assert.equal(v.btn.classList.contains("isActive"), false);
  assert.equal(v.btn.style.display, "none");
  assert.equal(v.view.innerHTML, "");
  assert.equal(v.api.isOpen(), false);
});

test("a record gone without a reset still closes the comparison on the next render", () => {
  const v = attachView();
  v.setRecord(saved());
  v.api.reset();
  v.btn.click();
  assert.equal(v.api.isOpen(), true);
  v.setRecord(null);
  v.api.refresh();
  assert.equal(v.panel.classList.contains("fightCompareOpen"), false);
  assert.equal(v.btn.style.display, "none");
});

test("translated strings are escaped before they reach innerHTML", () => {
  const v = attachView({ details: { compare: { party: "<img src=x onerror=alert(1)>", ahead: "<i>{winner}</i> {loser} {pct}" } } });
  v.setRecord(saved());
  v.api.reset();
  v.btn.click();
  assert.ok(!v.view.innerHTML.includes("<img"));
  assert.ok(v.view.innerHTML.includes("&lt;img src=x onerror=alert(1)&gt;"));
  assert.ok(v.view.innerHTML.includes("&lt;i&gt;A&lt;b&gt;&lt;/i&gt;"));
});
