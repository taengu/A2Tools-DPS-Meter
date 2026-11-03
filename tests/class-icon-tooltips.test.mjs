import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = (name) => readFileSync(new URL("../" + name, import.meta.url), "utf8").replace(/\r\n/g, "\n");
const i18nSource = source("public/src/js/i18n.js");
const historySource = source("public/src/js/history.js");
const classes = { "검성": "GLADIATOR", "수호성": "TEMPLAR", "궁성": "RANGER", "살성": "ASSASSIN", "마도성": "SORCERER", "치유성": "CLERIC", "정령성": "ELEMENTALIST", "호법성": "CHANTER", "권성": "FIGHTER" };
const locales = ["en", "ko", "ja", "de", "fr", "es", "pt", "ru", "zh-Hant", "zh-Hans"];
const between = (text, start, end) => {
  const from = text.indexOf(start), to = text.indexOf(end, from);
  assert.ok(from >= 0 && to > from, "production renderer boundaries exist");
  return text.slice(from, to);
};
const dataKey = (name) => name.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase());

function dom() {
  const nodes = [];
  class Element {
    constructor(tag = "div") {
      this.tagName = tag.toUpperCase(); this.dataset = {}; this.attributes = new Map();
      this.children = []; this.style = {}; this.alt = ""; this.textContent = ""; this.className = "";
      this.classList = { add() {}, toggle() {}, contains() { return false; } };
      nodes.push(this);
    }
    getAttribute(name) { return name.startsWith("data-") ? this.dataset[dataKey(name)] ?? null : this.attributes.get(name) ?? null; }
    setAttribute(name, value) { if (name.startsWith("data-")) this.dataset[dataKey(name)] = String(value); else this.attributes.set(name, String(value)); }
    hasAttribute(name) { return this.getAttribute(name) !== null; }
    removeAttribute(name) { if (name.startsWith("data-")) delete this.dataset[dataKey(name)]; else this.attributes.delete(name); }
    get title() { return this.getAttribute("title") || ""; }
    set title(value) { this.setAttribute("title", value); }
    appendChild(node) { this.children.push(node); node.parent = this; }
    after(node) { if (this.parent) { this.parent.children.splice(this.parent.children.indexOf(this) + 1, 0, node); node.parent = this.parent; } }
    querySelector(selector) { return this.children.find(n => selector === "img" ? n.tagName === "IMG" : n.className.split(" ").includes(selector.slice(1))) || this.children.map(n => n.querySelector(selector)).find(Boolean) || null; }
    querySelectorAll() { return []; }
    addEventListener() {}
    set innerHTML(text) {
      this.html = text; this.children = [];
      for (const match of text.matchAll(/<img\b[^>]*>/g)) {
        const img = new Element("img");
        img.className = match[0].match(/class="([^"]*)"/)?.[1] || "";
        this.appendChild(img);
      }
    }
    get innerHTML() { return this.html || ""; }
  }
  const document = {
    baseURI: "http://localhost/", readyState: "loading", addEventListener() {},
    documentElement: { setAttribute() {}, classList: { contains() { return false; } } },
    querySelectorAll: (selector) => {
      const attribute = selector.match(/^\[([^=]+)\]$/)?.[1];
      return attribute ? nodes.filter(n => n.hasAttribute(attribute)) : [];
    },
    createElement: tag => new Element(tag), createTextNode: text => Object.assign(new Element("text"), { textContent: text }),
  };
  return { nodes, document, Element };
}
async function setup(language = "en") {
  const d = dom();
  const window = { A2_VIEW: "settings", addEventListener() {} };
  const context = vm.createContext({
    window, document: d.document, URL, TextDecoder, console,
    fetch: async url => {
      const lang = new URL(url).pathname.match(/\/([^/]+)\.json$/)[1];
      const bytes = readFileSync(new URL("../src/data/i18n/ui/" + lang + ".json", import.meta.url));
      return { ok: true, arrayBuffer: async () => Uint8Array.from(bytes).buffer };
    },
  });
  vm.runInContext(i18nSource, context);
  await window.i18n.setLanguage(language, { persist: false });
  return { ...d, window, context, i18n: window.i18n };
}

test("all nine class icons use existing class names in all ten languages", async () => {
  const h = await setup();
  const icons = Object.keys(classes).map(job => {
    const icon = new h.Element("img");
    h.i18n.setClassTooltip(icon, job);
    return [job, icon];
  });
  for (const locale of locales) {
    const dictionary = JSON.parse(source("src/data/i18n/ui/" + locale + ".json"));
    await h.i18n.setLanguage(locale, { persist: false });
    for (const [job, icon] of icons) {
      const expected = dictionary.classes[classes[job]];
      assert.ok(typeof expected === "string" && expected.length > 0, locale + ":" + job);
      assert.equal(icon.title, expected);
      assert.equal(icon.alt, expected);
    }
  }
});

test("History member icons retain one custom tooltip and refresh their accessible names", async () => {
  const h = await setup();
  Object.assign(h.context, { fight: { memberJobs: ["호법성", "마도성"] }, i18n: h.i18n });
  const block = between(historySource, '    const iconsEl = document.createElement("div");', "    infoEl.appendChild(nameEl);");
  const container = vm.runInContext("(function(){" + block + "return iconsEl;})()", h.context);
  assert.equal(container.children[0].getAttribute("data-tip"), "Chanter");
  await h.i18n.setLanguage("ko", { persist: false });
  for (let i = 0; i < container.children.length; i++) {
    const wrap = container.children[i], img = wrap.querySelector("img");
    assert.equal(wrap.getAttribute("data-tip"), ["호법성", "마도성"][i]);
    assert.equal(img.alt, wrap.getAttribute("data-tip"));
    assert.equal(img.title, "");
    assert.equal(wrap.title, "");
  }
});

test("History selected filter, filter options and personal-best icons get native class tooltips", async () => {
  const h = await setup();
  const filterPlayerLabel = new h.Element(), filterPlayerMenu = new h.Element();
  Object.assign(h.context, {
    i18n: h.i18n, filterPlayer: "", filterPlayerLabel, filterPlayerMenu,
    CLASS_ICON_JOBS: new Set(Object.keys(classes)), getJobLabel: h.i18n.getClassName,
    setClassDropdownOpen() {}, renderList() {}, allFights: [], t: h.i18n.t,
  });
  const select = between(historySource, "  const setClassFilter = ", "\n  let allFights");
  vm.runInContext(select + '\nsetClassFilter("치유성");', h.context);
  assert.equal(filterPlayerLabel.querySelector("img").title, "Cleric");
  const options = between(historySource, "      const jobs = [...new Set(fights.flatMap", "      // Sync label");
  h.context.fights = [{ jobs: ["정령성", "권성"] }];
  vm.runInContext(options, h.context);
  assert.deepEqual(filterPlayerMenu.children.map(n => n.querySelector("img").title).sort(), ["Brawler", "Spiritmaster"]);
  const best = between(historySource, "  const classIcon = (job) => {", "\n  const buildBestRow");
  const img = vm.runInContext(best + '\nclassIcon("궁성")[0]', h.context);
  assert.equal(img.title, "Ranger");
  await h.i18n.setLanguage("ko", { persist: false });
  assert.equal(img.title, "궁성");
  assert.equal(filterPlayerLabel.querySelector("img").title, "치유성");
});

test("Details party-bar icons are labeled by the actual renderer", async () => {
  const h = await setup("fr");
  const contentEl = new h.Element();
  Object.assign(h.context, { job: "수호성", contentEl });
  const block = between(source("public/src/js/details.js"), '      if (job) {\n        const iconEl = document.createElement("img");', '      const nameEl = document.createElement("span");');
  vm.runInContext(block, h.context);
  const expected = JSON.parse(source("src/data/i18n/ui/fr.json")).classes.TEMPLAR;
  assert.equal(contentEl.querySelector("img").title, expected);
});

test("the existing noninteractive hover card names the class beside its icon", async () => {
  const h = await setup();
  vm.runInContext(source("public/src/js/core.js"), h.context);
  const app = vm.runInContext("Object.create(DpsApp.prototype)", h.context);
  app.hoverTooltipEl = new h.Element();
  app.dpsFormatter = new Intl.NumberFormat("en-US");
  app.i18n = h.i18n; app.scheduleHoverTooltipPosition = () => {}; app.watchHoverPointer = () => {};
  app.renderHoverTooltip({ skills: [], state: "empty" }, { job: "호법성", name: "Player", dps: 10, totalDamage: 100 }, {});
  const icon = app.hoverTooltipEl.querySelector(".hoverDetailsTooltipClassIcon");
  assert.equal(icon.alt, "Chanter");
  const label = icon.parent.children[icon.parent.children.indexOf(icon) + 1];
  assert.equal(label.textContent, "Chanter");
  await h.i18n.setLanguage("ko", { persist: false });
  assert.equal(label.textContent, "호법성");
  const css = source("src/styles.css");
  assert.match(css, /\.hoverDetailsTooltip\s*\{[^}]*pointer-events:\s*none;/);
});

test("all five Settings preview class icons use localized title bindings", async () => {
  const h = await setup();
  const tags = [...source("index.html").matchAll(/<img class="classIconImg"[^>]+>/g)];
  assert.equal(tags.length, 5);
  const previews = tags.map(([tag]) => {
    const key = tag.match(/data-i18n-title="([^"]+)"/)?.[1];
    assert.ok(key?.startsWith("classes."));
    const icon = new h.Element("img"); icon.dataset.i18nTitle = key; return icon;
  });
  for (const locale of locales) {
    await h.i18n.setLanguage(locale, { persist: false });
    const dictionary = JSON.parse(source("src/data/i18n/ui/" + locale + ".json"));
    for (const icon of previews) assert.equal(icon.title, dictionary.classes[icon.dataset.i18nTitle.slice(8)]);
  }
});
