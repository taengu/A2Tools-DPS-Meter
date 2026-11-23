// Details' Stagger section: each stagger window of the fight (a boss's gauge
// from becoming active to its break or its miss), on the fight's time, with
// who took what off it and with which skills. Fed by FightRecord.stagger
// (History) or by get_fight_buffs's `stagger` (a live fight); see
// src-tauri/src/combat/stagger.rs.

// Why a window ended (`End` in stagger.rs), as i18n keys under
// details.stagger.ended.
const STAGGER_END_FALLBACK = {
  break: "Broken after {secs} s",
  reset: "Missed: {missing} of {max} left",
  death: "Defeated before the break: {missing} of {max} left",
  despawn: "Left before the break: {missing} of {max} left",
  fightEnd: "Still open when the fight ended: {missing} of {max} left",
};

// A window's ending: "break", "reset", "death", "despawn" or "fightEnd".
const staggerEndKey = (w) => {
  const ended = String(w?.ended || "");
  return Object.hasOwn(STAGGER_END_FALLBACK, ended) ? ended : "reset";
};

// A window seen whole: who took how much, by skill, with the hit times.
// Actor 0 (no hit seen before the deduction) is its own entry, last.
const summarizeStaggerWindow = (w) => {
  const hits = Array.isArray(w?.hits) ? w.hits : [];
  const max = Number(w?.max) || 0;
  const byActor = new Map();
  for (const h of hits) {
    const actor = Number(h.actor) || 0;
    const value = Number(h.value) || 0;
    const skill = Number(h.skill) || 0;
    let a = byActor.get(actor);
    if (!a) {
      a = { actor, total: 0, ambiguous: 0, hits: [], skills: new Map() };
      byActor.set(actor, a);
    }
    a.total += value;
    if (h.ambiguous) a.ambiguous += 1;
    a.hits.push(h);
    let s = a.skills.get(skill);
    if (!s) {
      s = { skill, total: 0, count: 0, hits: [] };
      a.skills.set(skill, s);
    }
    s.total += value;
    s.count += 1;
    s.hits.push(h);
  }
  const total = hits.reduce((sum, h) => sum + (Number(h.value) || 0), 0);
  const actors = [...byActor.values()]
    .map((a) => ({
      ...a,
      share: max > 0 ? a.total / max : 0,
      skills: [...a.skills.values()].sort((x, y) => y.total - x.total || x.skill - y.skill),
    }))
    .sort((x, y) => (x.actor === 0) - (y.actor === 0) || y.total - x.total || x.actor - y.actor);
  return {
    total,
    actors,
    ambiguous: hits.filter((h) => h.ambiguous).length,
    unknown: byActor.get(0)?.total || 0,
  };
};

// Windows to show: the fight's target first, then the others, each by start.
const orderStaggerWindows = (windows, targetId) => {
  const target = Number(targetId);
  return (Array.isArray(windows) ? windows : [])
    .filter((w) => w && Array.isArray(w.hits))
    .slice()
    .sort((a, b) => (Number(b.entity) === target) - (Number(a.entity) === target)
      || (Number(a.start) || 0) - (Number(b.start) || 0)
      || (Number(a.entity) || 0) - (Number(b.entity) || 0));
};

// Percent of the fight, cut to it.
const staggerPct = (ms, durationMs) => {
  const d = Number(durationMs) || 0;
  if (d <= 0) return 0;
  return Math.round(Math.max(0, Math.min(100, (Number(ms) / d) * 100)) * 1000) / 1000;
};

// A mark's left and width on the lane, following the fight timeline's zoom
// (--ftl-from and --ftl-zoom, set by details.js; buffTimeline.js's
// ftlLeftCss). Plain percent where they are not set.
const staggerLeftCss = (pct) => `calc((${Number(pct) || 0} - var(--ftl-from, 0)) * var(--ftl-zoom, 1) * 1%)`;
const staggerWidthCss = (pct) => `calc(${Number(pct) || 0} * var(--ftl-zoom, 1) * 1%)`;

// m:ss.t from the fight's start.
const formatStaggerTime = (ms) => {
  const v = Math.max(0, Number(ms) || 0);
  const m = Math.floor(v / 60000);
  const s = (v % 60000) / 1000;
  return `${m}:${s.toFixed(1).padStart(4, "0")}`;
};

const createStaggerPanel = ({ root, describeActor = () => null }) => {
  if (!root) return null;
  const i18n = window.i18n;
  const t = (key, fallback) => i18n?.t?.(key, fallback) ?? fallback;
  const fmt = (key, vars, fallback) => (i18n?.format
    ? i18n.format(key, vars, fallback)
    : String(fallback).replace(/\{(\w+)\}/g, (_, k) => String(vars?.[k] ?? "")));

  const body = root.querySelector(".staggerBody");
  const tooltip = document.createElement("div");
  tooltip.className = "buffTooltip staggerTooltip";
  tooltip.hidden = true;
  document.body?.appendChild(tooltip);

  let data = null; // { windows, durationMs, targetId, targetName, older, loading }
  // Open players, by window and actor: kept across a live fight's refreshes.
  const expanded = new Set();

  const actorInfo = (id) => {
    const numeric = Number(id);
    if (!numeric) return { name: t("details.stagger.unknownActor", "Unknown"), job: "", color: "", unknown: true };
    const actor = describeActor(numeric);
    if (actor) return actor;
    return { name: `#${numeric}`, job: "", color: "" };
  };

  const entityName = (w) => {
    const entity = Number(w.entity);
    if (data && entity === Number(data.targetId) && data.targetName) return data.targetName;
    const code = Number(w.mobCode) || 0;
    return (code && i18n?.getNpcName?.(code, "")) || `#${entity}`;
  };

  // A skill code as the hit record has it (12090243, Punishment) names
  // nothing in the table: its family does (12090000). The code the table
  // names, else the family's.
  const skillFamily = (code) => {
    const c = Number(code) || 0;
    const family = [c, c - (c % 10000), c - (c % 1000), c - (c % 100)];
    return family.find((k) => i18n?.getSkillName?.(k, "")) ?? family[1];
  };

  const skillName = (code) => {
    const c = Number(code) || 0;
    if (!c) return t("details.stagger.unknownSkill", "Unknown skill");
    return i18n?.getSkillName?.(skillFamily(c), "") || `#${c}`;
  };

  const skillIcon = (code, job) => {
    const img = document.createElement("img");
    img.className = "buffIcon";
    img.alt = "";
    img.loading = "lazy";
    img.referrerPolicy = "no-referrer";
    const c = Number(code) || 0;
    if (c && window.skillIcons?.applyIconToImage) {
      img.addEventListener("error", () => window.skillIcons?.handleImgError?.(img));
      window.skillIcons.applyIconToImage(img, { code: skillFamily(c), job });
      return img;
    }
    const dot = document.createElement("span");
    dot.className = "buffIcon buffIconDot";
    return dot;
  };

  const note = (text, cls = "") => {
    const el = document.createElement("div");
    el.className = `buffNote ${cls}`.trim();
    el.textContent = text;
    return el;
  };

  const cell = (cls, text) => {
    const el = document.createElement("div");
    el.className = cls;
    if (text !== undefined) el.textContent = text;
    return el;
  };

  // Marks on the fight's time, one per hit; each keeps its hit for the tooltip.
  const ticks = (lane, hits, color, actor) => {
    for (const h of hits) {
      const m = document.createElement("div");
      m.className = `staggerTick${h.ambiguous ? " isAmbiguous" : ""}`;
      m.style.left = staggerLeftCss(staggerPct(h.t, data.durationMs));
      if (color) m.style.background = color;
      m._hit = h;
      m._actor = actor;
      lane.appendChild(m);
    }
  };

  const windowEl = (w, index) => {
    const el = document.createElement("div");
    el.className = "staggerWindow";
    const sum = summarizeStaggerWindow(w);
    const endKey = staggerEndKey(w);
    const tone = endKey === "break" ? "isBreak" : endKey === "death" || endKey === "despawn" ? "isGone" : "isMissed";

    // The window on the fight's time, coloured by how it ended.
    const head = cell(`buffRow staggerHead ${tone}`);
    const label = cell("buffLabel");
    const name = document.createElement("span");
    name.className = "buffName staggerEntity";
    name.textContent = entityName(w);
    label.appendChild(name);
    head.appendChild(label);
    const lane = cell("buffLane");
    const bar = cell(`staggerSpan ${tone}`);
    const from = staggerPct(w.start, data.durationMs);
    const to = staggerPct(w.end, data.durationMs);
    bar.style.left = staggerLeftCss(from);
    bar.style.width = staggerWidthCss(Math.max(0.4, to - from));
    bar._window = w;
    lane.appendChild(bar);
    if (endKey === "break") {
      const mark = cell("staggerBreakMark");
      mark.style.left = staggerLeftCss(to);
      lane.appendChild(mark);
    }
    head.appendChild(lane);
    head.appendChild(cell("buffUptime staggerValue", `${sum.total}/${Number(w.max) || 0}`));
    el.appendChild(head);

    // How it ended, and what to read it with.
    const secs = ((Number(w.end) - Number(w.start)) / 1000).toFixed(1);
    const parts = [fmt(
      `details.stagger.ended.${endKey}`,
      { secs, missing: Number(w.missing) || 0, max: Number(w.max) || 0 },
      STAGGER_END_FALLBACK[endKey],
    )];
    if (w.incomplete) parts.push(t("details.stagger.incomplete", "Its start was not seen: earlier stagger is missing."));
    if (sum.ambiguous) {
      parts.push(fmt("details.stagger.ambiguousCount", { n: sum.ambiguous }, "{n} hit(s) may belong to another player."));
    }
    el.appendChild(note(parts.join(" · "), `staggerStatus ${tone}`));

    for (const a of sum.actors) {
      const info = actorInfo(a.actor);
      const key = `${index}:${w.entity}:${w.start}:${a.actor}`;
      const open = expanded.has(key);
      const row = cell(`buffRow staggerActor${info.unknown ? " isUnknown" : ""}${open ? " isOpen" : ""}`);
      row.tabIndex = 0;
      row.setAttribute("role", "button");
      row.setAttribute("aria-expanded", String(open));
      const lbl = cell("buffLabel staggerActorLabel");
      lbl.style.setProperty("--stagger-share", `${Math.round(Math.min(1, a.share) * 1000) / 10}%`);
      if (info.color) lbl.style.setProperty("--stagger-color", info.color);
      const chevron = document.createElement("span");
      chevron.className = "staggerChevron";
      chevron.textContent = "▶";
      lbl.appendChild(chevron);
      const n = document.createElement("span");
      n.className = "buffName";
      n.textContent = info.name;
      if (info.color) n.style.color = info.color;
      lbl.appendChild(n);
      if (a.ambiguous) {
        const warn = document.createElement("span");
        warn.className = "staggerWarn";
        warn.textContent = "⚠";
        warn.title = t("details.stagger.ambiguous", "May belong to another player: someone else hit at the same time.");
        lbl.appendChild(warn);
      }
      row.appendChild(lbl);
      const l = cell("buffLane");
      ticks(l, a.hits, info.color, a.actor);
      row.appendChild(l);
      const pct = Number(w.max) > 0 ? ` (${Math.round(a.share * 100)}%)` : "";
      row.appendChild(cell("buffUptime staggerValue", String(a.total)));
      row.title = `${info.name}: ${a.total}${pct}`;
      const toggle = () => {
        if (expanded.has(key)) expanded.delete(key);
        else expanded.add(key);
        render();
      };
      row.addEventListener("click", toggle);
      row.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          toggle();
        }
      });
      el.appendChild(row);
      if (!open) continue;
      for (const s of a.skills) {
        const sr = cell("buffRow staggerSkill");
        const sl = cell("buffLabel");
        sl.appendChild(skillIcon(s.skill, info.job));
        const sn = document.createElement("span");
        sn.className = "buffName";
        sn.textContent = s.count > 1 ? `${skillName(s.skill)} ×${s.count}` : skillName(s.skill);
        sl.appendChild(sn);
        sr.appendChild(sl);
        const slane = cell("buffLane");
        ticks(slane, s.hits, info.color, a.actor);
        sr.appendChild(slane);
        sr.appendChild(cell("buffUptime staggerValue", String(s.total)));
        el.appendChild(sr);
      }
    }
    return el;
  };

  // The section shows only for a fight with a stagger window: a fight with
  // no stagger data (an older one, or any log on a2tools.app, whose slices
  // carry no gauges) or none worn down has nothing to say, and every trash
  // fight saying "none" was noise. Its toggle chip goes with it.
  const render = () => {
    if (!body) return;
    hideTooltip();
    body.replaceChildren();
    const windows = data && !data.older ? orderStaggerWindows(data.windows, data.targetId) : [];
    const shown = windows.length > 0 && Number(data?.durationMs) > 0;
    root?.classList?.toggle?.("noStagger", !shown);
    if (!shown) return;
    windows.forEach((w, i) => body.appendChild(windowEl(w, i)));
  };

  // ── Tooltip ──
  const hideTooltip = () => {
    tooltip.hidden = true;
  };
  const line = (text, cls) => {
    const div = document.createElement("div");
    if (cls) div.className = cls;
    div.textContent = text;
    return div;
  };
  const moveTooltip = (event) => {
    if (tooltip.hidden) return;
    const pad = 12;
    const w = tooltip.offsetWidth;
    const h = tooltip.offsetHeight;
    let x = event.clientX + pad;
    let y = event.clientY + pad;
    if (x + w > window.innerWidth - 4) x = Math.max(4, event.clientX - w - pad);
    if (y + h > window.innerHeight - 4) y = Math.max(4, event.clientY - h - pad);
    tooltip.style.left = `${x}px`;
    tooltip.style.top = `${y}px`;
  };
  const showHit = (tick, event) => {
    const h = tick._hit;
    const info = actorInfo(tick._actor);
    const lines = [
      line(skillName(h.skill), "buffTooltipTitle"),
      line(`${info.name} · ${fmt("details.stagger.value", { n: Number(h.value) || 0 }, "{n} stagger")}`),
      line(formatStaggerTime(h.t), "buffTooltipSub"),
    ];
    if (h.ambiguous) {
      lines.push(line(t("details.stagger.ambiguous", "May belong to another player: someone else hit at the same time."), "buffTooltipSub"));
    }
    tooltip.replaceChildren(...lines);
    tooltip.hidden = false;
    moveTooltip(event);
  };
  const showWindow = (bar, event) => {
    const w = bar._window;
    tooltip.replaceChildren(
      line(entityName(w), "buffTooltipTitle"),
      line(`${formatStaggerTime(w.start)} – ${formatStaggerTime(w.end)} (${((w.end - w.start) / 1000).toFixed(1)} s)`),
    );
    tooltip.hidden = false;
    moveTooltip(event);
  };
  body?.addEventListener("mouseover", (event) => {
    const tick = event.target.closest?.(".staggerTick");
    if (tick?._hit) return showHit(tick, event);
    const bar = event.target.closest?.(".staggerSpan");
    if (bar?._window) return showWindow(bar, event);
    hideTooltip();
  });
  body?.addEventListener("mousemove", moveTooltip);
  body?.addEventListener("mouseleave", hideTooltip);

  i18n?.onChange?.(() => render());

  return {
    // `next`: { windows, durationMs, targetId, targetName, older, loading }
    setData(next) {
      data = next;
      render();
    },
    render,
    clear() {
      data = null;
      expanded.clear();
      render();
    },
  };
};

window.createStaggerPanel = createStaggerPanel;
