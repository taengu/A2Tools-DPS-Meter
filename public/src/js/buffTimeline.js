// Details' Buffs section: one row per buff or debuff on the selected player,
// then the debuffs on the target, each a bar per time it was on over the
// fight's time, with its uptime. Fed by FightRecord.buffs (History) or by
// get_fight_buffs (a live fight); see src-tauri/src/combat/fight_buffs.rs.

// How a segment ended (`end_code` in fight_buffs.rs), as i18n keys under
// details.buffs.end.
const BUFF_END_KEYS = [
  "stacks", "expired", "takenOff", "removed", "recast", "gone",
  "mapLoad", "notListed", "pushedOut", "unseen", "stillOn", "replaced",
];
const BUFF_END_FALLBACK = {
  stacks: "Stacks changed",
  expired: "Expired",
  takenOff: "Taken off by a skill",
  removed: "Removed",
  recast: "Applied again by another caster",
  gone: "Target died or left",
  mapLoad: "Map changed",
  notListed: "Ended unseen",
  pushedOut: "Pushed out by a newer stack",
  unseen: "Ran out (no end seen)",
  stillOn: "Still on at the end of the fight",
  replaced: "Replaced by a new level",
};

// "start,end,stacks,how;..." into segments; anything malformed is skipped.
const parseBuffSegments = (segs) => {
  if (typeof segs !== "string" || !segs) return [];
  const out = [];
  for (const part of segs.split(";")) {
    const f = part.split(",").map(Number);
    if (f.length !== 4 || f.some((n) => !Number.isFinite(n))) continue;
    const [start, end, stacks, how] = f;
    if (end < start) continue;
    out.push({ start, end, stacks, how });
  }
  return out;
};

// Pieces of constant stacks joined back into the times the buff was on: a
// piece that ended in a stack change (how 0) runs on into the next. Returns
// the applications and, per piece, the index of its application.
const joinBuffApplications = (segments) => {
  const apps = [];
  const appOf = [];
  let current = null;
  for (const s of segments) {
    if (current && current.how === 0 && current.end === s.start) {
      current.end = s.end;
      current.how = s.how;
      current.maxStacks = Math.max(current.maxStacks, s.stacks);
    } else {
      current = { start: s.start, end: s.end, how: s.how, maxStacks: s.stacks };
      apps.push(current);
    }
    appOf.push(apps.length - 1);
  }
  return { apps, appOf };
};

// Bars for a lane: left and width in percent of the fight, cut to it. Each
// keeps its piece and the application it belongs to, for the tooltip.
const layoutBuffSegments = (segments, durationMs) => {
  const duration = Number(durationMs) || 0;
  if (duration <= 0 || !Array.isArray(segments)) return [];
  const { apps, appOf } = joinBuffApplications(segments);
  const round = (v) => Math.round(v * 1000) / 1000;
  const out = [];
  segments.forEach((s, i) => {
    const from = Math.max(0, s.start);
    const to = Math.min(duration, s.end);
    if (to <= from) return;
    out.push({
      left: round((from / duration) * 100),
      width: round(((to - from) / duration) * 100),
      stacks: s.stacks,
      how: s.how,
      piece: s,
      app: apps[appOf[i]],
    });
  });
  return out;
};

// "87%"; "<1%" for a buff that was on, but briefly.
const formatBuffUptime = (upMs, durationMs) => {
  const duration = Number(durationMs) || 0;
  const up = Math.max(0, Number(upMs) || 0);
  if (duration <= 0) return "-";
  const pct = Math.min(100, (up / duration) * 100);
  if (up > 0 && pct < 1) return "<1%";
  return `${Math.round(pct)}%`;
};

// Fight time as m:ss, or m:ss.t with tenths; a time before the pull is
// negative.
const formatBuffTime = (ms, { tenths = false } = {}) => {
  const value = Number(ms) || 0;
  const sign = value < 0 ? "-" : "";
  const abs = Math.abs(value);
  const totalTenths = Math.floor(abs / 100);
  const minutes = Math.floor(totalTenths / 600);
  const seconds = Math.floor((totalTenths % 600) / 10);
  const base = `${sign}${minutes}:${String(seconds).padStart(2, "0")}`;
  return tenths ? `${base}.${totalTenths % 10}` : base;
};

// Ticks for the time axis: a round step giving at most `maxTicks` labels.
const buffAxisTicks = (durationMs, maxTicks = 8) => {
  const duration = Number(durationMs) || 0;
  if (duration <= 0) return [];
  const steps = [5, 10, 15, 30, 60, 120, 300, 600].map((s) => s * 1000);
  const step = steps.find((s) => duration / s <= maxTicks) || steps[steps.length - 1];
  const ticks = [];
  for (let t = 0; t <= duration; t += step) ticks.push({ ms: t, pct: (t / duration) * 100 });
  return ticks;
};

const firstBuffStart = (track) => {
  const segs = track._segments || parseBuffSegments(track.segs);
  return segs.length ? segs[0].start : Infinity;
};

// The rows to show: the player's timed buffs, their passives, and what was
// on the target. `caster` is "all", "party" (cast by one of the fight's
// players) or "self" (cast by the selected player).
const selectBuffRows = (tracks, {
  targetId = null,
  playerId = null,
  actorIds = [],
  caster = "all",
  hidePassives = true,
  sort = "uptime",
} = {}) => {
  const party = new Set([...actorIds].map(Number));
  const target = Number(targetId);
  const player = playerId === null || playerId === undefined ? null : Number(playerId);
  const keep = (t) => {
    const by = Number(t.by);
    if (caster === "party") return party.has(by);
    if (caster === "self") return player !== null && by === player;
    return true;
  };
  const list = (Array.isArray(tracks) ? tracks : []).map((t) => ({ ...t, _segments: parseBuffSegments(t.segs) }));
  const compare = sort === "first"
    ? (a, b) => firstBuffStart(a) - firstBuffStart(b) || (b.up || 0) - (a.up || 0) || a.id - b.id
    : (a, b) => (b.up || 0) - (a.up || 0) || firstBuffStart(a) - firstBuffStart(b) || a.id - b.id;
  const onPlayer = player === null ? [] : list.filter((t) => Number(t.on) === player && Number(t.on) !== target && keep(t));
  return {
    player: onPlayer.filter((t) => !t.passive).sort(compare),
    passives: hidePassives ? [] : onPlayer.filter((t) => t.passive).sort((a, b) => a.id - b.id),
    passiveCount: onPlayer.filter((t) => t.passive).length,
    target: list.filter((t) => Number(t.on) === target && !t.passive && keep(t)).sort(compare),
  };
};

// Class name (English or Korean, as actors carry them) to its i18n key.
const BUFF_CLASS_KEYS = {
  Gladiator: "GLADIATOR", 검성: "GLADIATOR",
  Templar: "TEMPLAR", 수호성: "TEMPLAR",
  Ranger: "RANGER", 궁성: "RANGER",
  Assassin: "ASSASSIN", 살성: "ASSASSIN",
  Sorcerer: "SORCERER", 마도성: "SORCERER",
  Cleric: "CLERIC", 치유성: "CLERIC",
  Spiritmaster: "ELEMENTALIST", Elementalist: "ELEMENTALIST", 정령성: "ELEMENTALIST",
  Chanter: "CHANTER", 호법성: "CHANTER",
  Brawler: "FIGHTER", Fighter: "FIGHTER", 권성: "FIGHTER",
};

const createBuffTimeline = ({ root, describeActor = () => null }) => {
  if (!root) return null;
  const i18n = window.i18n;
  const t = (key, fallback) => i18n?.t?.(key, fallback) ?? fallback;
  const fmt = (key, vars, fallback) => i18n?.format?.(key, vars, fallback) ?? fallback;

  const toolbar = root.querySelector(".buffToolbar");
  const body = root.querySelector(".buffBody");
  const tooltip = root.querySelector(".buffTooltip");
  // Fixed to the window, not clipped by the collapsible section.
  if (tooltip && document.body && tooltip.parentElement !== document.body) document.body.appendChild(tooltip);

  const options = { caster: "all", hidePassives: true, sort: "uptime" };
  try {
    const saved = JSON.parse(localStorage.getItem("dpsMeter.buffOptions") || "null");
    if (saved && typeof saved === "object") {
      if (["all", "party", "self"].includes(saved.caster)) options.caster = saved.caster;
      if (typeof saved.hidePassives === "boolean") options.hidePassives = saved.hidePassives;
      if (["uptime", "first"].includes(saved.sort)) options.sort = saved.sort;
    }
  } catch {
    // defaults
  }

  let data = null; // { tracks, durationMs, targetId, targetName, playerId, actorIds, older, loading }
  let abnormalNames = null; // { names, native }
  let abnormalIcons = null; // id -> icon file name
  let namesLanguage = null;
  let namesPending = false;
  let iconsPending = false;

  const loadNames = () => {
    const lang = i18n?.getLanguage?.() || "en";
    if (namesPending || (abnormalNames && namesLanguage === lang)) return;
    if (!i18n?.loadAbnormalNames) return;
    namesPending = true;
    i18n.loadAbnormalNames().then((loaded) => {
      abnormalNames = loaded || { names: {}, native: true };
      namesLanguage = lang;
    }).catch(() => {
      abnormalNames = { names: {}, native: true };
      namesLanguage = lang;
    }).finally(() => {
      namesPending = false;
      render();
    });
  };

  const loadIcons = () => {
    if (abnormalIcons || iconsPending) return;
    iconsPending = true;
    const fromBridge = () => {
      try {
        return JSON.parse(window.javaBridge?.readResource?.("/data/abnormals.json") || "null");
      } catch {
        return null;
      }
    };
    fetch(new URL("./data/abnormals.json", document.baseURI).toString())
      .then((r) => (r.ok ? r.json() : fromBridge()))
      .catch(fromBridge)
      .then((table) => {
        const out = {};
        const list = table?.abnormals || {};
        for (const id of Object.keys(list)) {
          if (list[id]?.icon) out[id] = list[id].icon;
        }
        abnormalIcons = out;
      })
      .finally(() => {
        iconsPending = false;
        render();
      });
  };

  const buffName = (track) => {
    const id = String(track.id);
    const names = abnormalNames?.names || {};
    // No buff names in this language (Chinese): the skill that applied it,
    // in the language, before the English buff name.
    if (abnormalNames && !abnormalNames.native && track.skill) {
      const skillName = i18n?.getSkillName?.(track.skill, "");
      if (skillName) return skillName;
    }
    const name = names[id];
    if (typeof name === "string" && name.trim()) return name;
    const skillName = track.skill ? i18n?.getSkillName?.(track.skill, "") : "";
    return skillName || familyName(track.skill) || `#${id}`;
  };

  // A skill code the table lacks (a summon's aura, 17150001) is one of a
  // skill's codes: the table names its siblings (17150000 to 17150240 are
  // all "Divine Aura"). The nearest code below it, then above, in the same
  // first six digits; otherwise nothing.
  const familyName = (skill) => {
    const code = Number(skill);
    if (!(code > 0)) return "";
    const base = Math.floor(code / 100) * 100;
    for (let d = 1; d < 100; d += 1) {
      for (const c of [code - d, code + d]) {
        if (c < base || c >= base + 100) continue;
        const n = i18n?.getSkillName?.(c, "");
        if (n) return n;
      }
    }
    // Wider: the skill's first five digits (17150001 -> 1715000x..).
    const wide = Math.floor(code / 1000) * 1000;
    for (let c = wide; c < wide + 1000; c += 10) {
      const n = i18n?.getSkillName?.(c, "");
      if (n) return n;
    }
    return "";
  };

  const casterInfo = (id) => {
    const numeric = Number(id);
    if (!numeric) return { name: t("details.buffs.unknownCaster", "Unknown"), job: "", color: "" };
    if (data && numeric === Number(data.targetId)) {
      return { name: data.targetName || `#${numeric}`, job: "", color: "", isTarget: true };
    }
    const actor = describeActor(numeric);
    if (actor) return actor;
    return { name: `#${numeric}`, job: "", color: "" };
  };

  const classLabel = (job) => {
    const key = BUFF_CLASS_KEYS[job];
    return key ? t(`classes.${key}`, job) : job || "";
  };

  const persist = () => {
    try {
      localStorage.setItem("dpsMeter.buffOptions", JSON.stringify(options));
    } catch {
      // not kept
    }
  };

  const syncToolbar = () => {
    toolbar?.querySelectorAll?.("[data-buff-caster]")?.forEach((btn) => {
      btn.classList.toggle("isActive", btn.dataset.buffCaster === options.caster);
    });
    toolbar?.querySelectorAll?.("[data-buff-sort]")?.forEach((btn) => {
      btn.classList.toggle("isActive", btn.dataset.buffSort === options.sort);
    });
    const passives = toolbar?.querySelector?.(".buffPassivesBtn");
    if (passives) {
      passives.classList.toggle("isActive", !options.hidePassives);
      passives.setAttribute("aria-pressed", String(!options.hidePassives));
    }
  };

  toolbar?.addEventListener("click", (event) => {
    const btn = event.target.closest?.("button");
    if (!btn) return;
    if (btn.dataset.buffCaster) options.caster = btn.dataset.buffCaster;
    else if (btn.dataset.buffSort) options.sort = btn.dataset.buffSort;
    else if (btn.classList.contains("buffPassivesBtn")) options.hidePassives = !options.hidePassives;
    else return;
    persist();
    syncToolbar();
    render();
  });

  const note = (text) => {
    const el = document.createElement("div");
    el.className = "buffNote";
    el.textContent = text;
    return el;
  };

  // Icons are kept across renders (a live fight renders every 2 s), so an
  // image is not loaded again; a second row with the same icon gets a copy.
  const iconEls = new Map();
  let iconsUsed = new Set();
  const iconFor = (track) => {
    // Keyed by what it is drawn from: once the icon table loads, the key
    // changes and the buff's own icon replaces its skill's.
    const icon = abnormalIcons?.[String(track.id)] || "";
    const key = `${icon}:${track.skill || 0}:${abnormalIcons ? 1 : 0}`;
    let el = iconEls.get(key);
    if (!el) {
      if (iconEls.size > 600) iconEls.clear();
      el = createIcon(icon, track.skill);
      iconEls.set(key, el);
    }
    // A second row gets an icon of its own: a copy taken while the first
    // was still loading kept its placeholder. The image is cached by then.
    if (iconsUsed.has(key)) return createIcon(icon, track.skill);
    iconsUsed.add(key);
    return el;
  };

  const createIcon = (icon, skill) => {
    const img = document.createElement("img");
    img.className = "buffIcon";
    img.alt = "";
    img.loading = "lazy";
    img.referrerPolicy = "no-referrer";
    const candidates = window.skillIcons?.getAbnormalIconCandidates?.(icon, skill) || [];
    if (candidates.length && window.skillIcons?.applyIconToImage) {
      img.addEventListener("error", () => window.skillIcons?.handleImgError?.(img));
      window.skillIcons.applyIconToImage(img, { candidates });
      return img;
    }
    const dot = document.createElement("span");
    dot.className = "buffIcon buffIconDot";
    return dot;
  };

  // The bar's color says who cast it: the player themselves, a party
  // member (their class color), or anyone else (the boss, a stranger).
  // Your own buffs too are in your class colour: a generic "self" blue made
  // nearly every row of a player's own buffs the same blue.
  const barColor = (track) => {
    const by = Number(track.by);
    const caster = casterInfo(by);
    if (caster.isTarget) return "var(--buff-hostile)";
    if (caster.color) return caster.color;
    return data && by && by === Number(data.playerId) ? "var(--buff-self)" : "var(--buff-other)";
  };

  const row = (track, rowIndex) => {
    const el = document.createElement("div");
    el.className = "buffRow";
    el.dataset.row = String(rowIndex);

    // The name is in its bar's colour: who cast it (you, a party member in
    // their class colour, or the boss). Their name is in the tooltip; beside
    // the buff's own name it read as part of it.
    const color = barColor(track);
    const label = document.createElement("div");
    label.className = "buffLabel";
    label.appendChild(iconFor(track));
    const name = document.createElement("span");
    name.className = "buffName";
    name.textContent = buffName(track);
    name.style.color = color;
    label.appendChild(name);
    if (track.summon) {
      const tag = document.createElement("span");
      tag.className = "buffTag";
      tag.textContent = t("details.buffs.summonTag", "Summon");
      label.appendChild(tag);
    }
    el.appendChild(label);

    const lane = document.createElement("div");
    lane.className = "buffLane";
    const bars = layoutBuffSegments(track._segments, data.durationMs);
    const maxStacks = Math.max(1, ...bars.map((b) => b.stacks));
    bars.forEach((bar, barIndex) => {
      const b = document.createElement("div");
      b.className = "buffBar";
      b.style.left = `${bar.left}%`;
      b.style.width = `${bar.width}%`;
      b.style.background = color;
      // More stacks, more solid.
      b.style.opacity = String(maxStacks > 1 ? 0.45 + 0.55 * (bar.stacks / maxStacks) : 0.85);
      b.dataset.bar = String(barIndex);
      // The count only where it fits; narrower bars say it by their shade.
      if (bar.stacks > 1 && bar.width >= 2.5) {
        const n = document.createElement("span");
        n.className = "buffStacks";
        n.textContent = String(bar.stacks);
        b.appendChild(n);
      }
      lane.appendChild(b);
    });
    el.appendChild(lane);
    el._bars = bars;
    el._track = track;

    const up = document.createElement("div");
    up.className = "buffUptime";
    up.textContent = formatBuffUptime(track.up, data.durationMs);
    el.appendChild(up);
    return el;
  };

  const group = (title, tracks, startIndex, rows) => {
    const g = document.createElement("div");
    g.className = "buffGroup";
    const header = document.createElement("div");
    header.className = "buffGroupHeader";
    const titleEl = document.createElement("span");
    titleEl.textContent = title;
    header.appendChild(titleEl);
    const count = document.createElement("span");
    count.className = "buffGroupCount";
    count.textContent = String(tracks.length);
    header.appendChild(count);
    g.appendChild(header);
    tracks.forEach((track, i) => {
      const r = row(track, startIndex + i);
      rows.push(r);
      g.appendChild(r);
    });
    return g;
  };

  const axis = () => {
    const el = document.createElement("div");
    el.className = "buffRow buffAxis";
    el.appendChild(document.createElement("div"));
    const lane = document.createElement("div");
    lane.className = "buffAxisLane";
    for (const tick of buffAxisTicks(data.durationMs)) {
      const span = document.createElement("span");
      span.style.left = `${tick.pct}%`;
      span.textContent = formatBuffTime(tick.ms);
      lane.appendChild(span);
    }
    el.appendChild(lane);
    const up = document.createElement("div");
    up.className = "buffUptime";
    up.textContent = t("details.buffs.uptime", "Up");
    el.appendChild(up);
    return el;
  };

  let rowEls = [];
  const render = () => {
    if (!body) return;
    hideTooltip();
    body.replaceChildren();
    rowEls = [];
    iconsUsed = new Set();
    syncToolbar();
    if (!data) return;
    if (data.older) {
      body.appendChild(note(t("details.buffs.older", "No buff data for this fight (recorded by an older version).")));
      return;
    }
    if (data.loading && !data.tracks) {
      body.appendChild(note(t("details.buffs.loading", "Loading...")));
      return;
    }
    loadNames();
    loadIcons();
    const tracks = Array.isArray(data.tracks) ? data.tracks : [];
    if (!tracks.length || !(Number(data.durationMs) > 0)) {
      body.appendChild(note(t("details.buffs.none", "No buffs or debuffs were seen in this fight.")));
      return;
    }
    const rows = selectBuffRows(tracks, { ...data, ...options });
    body.appendChild(axis());
    if (data.playerId === null || data.playerId === undefined) {
      body.appendChild(note(t("details.buffs.selectPlayer", "Select a player above to see their buffs.")));
    } else {
      const playerName = casterInfo(data.playerId).name;
      const g = group(playerName, rows.player, 0, rowEls);
      if (!rows.player.length) g.appendChild(note(t("details.buffs.noneShown", "None with these filters.")));
      if (rows.passives.length) {
        const p = document.createElement("div");
        p.className = "buffPassives";
        const label = document.createElement("span");
        label.className = "buffPassivesLabel";
        label.textContent = t("details.buffs.passives", "Passives");
        p.appendChild(label);
        const names = document.createElement("span");
        names.textContent = rows.passives.map(buffName).join(", ");
        p.appendChild(names);
        g.appendChild(p);
      }
      body.appendChild(g);
    }
    const tg = group(t("details.buffs.onTarget", "Debuffs on target"), rows.target, rowEls.length, rowEls);
    if (!rows.target.length) tg.appendChild(note(t("details.buffs.noneShown", "None with these filters.")));
    body.appendChild(tg);
  };

  // ── Tooltip ──
  const hideTooltip = () => {
    if (tooltip) tooltip.hidden = true;
  };
  const line = (text, cls) => {
    const div = document.createElement("div");
    if (cls) div.className = cls;
    div.textContent = text;
    return div;
  };
  const showTooltip = (rowEl, barIndex, event) => {
    if (!tooltip) return;
    const bar = rowEl?._bars?.[barIndex];
    const track = rowEl?._track;
    if (!bar || !track) return hideTooltip();
    const app = bar.app || bar.piece;
    const caster = casterInfo(track.by);
    const job = classLabel(caster.job);
    const endKey = BUFF_END_KEYS[app.how] || "removed";
    const lines = [
      line(buffName(track), "buffTooltipTitle"),
      line(fmt("details.buffs.castBy", { name: job ? `${caster.name} (${job})` : caster.name }, `Cast by ${caster.name}`)),
      line(`${formatBuffTime(app.start, { tenths: true })} – ${formatBuffTime(app.end, { tenths: true })} (${((app.end - app.start) / 1000).toFixed(1)} s)`),
    ];
    if (app.start < 0) lines.push(line(t("details.buffs.beforePull", "On before the fight began"), "buffTooltipSub"));
    if (bar.stacks > 1 || (app.maxStacks || 0) > 1) {
      lines.push(line(fmt("details.buffs.stacks", { n: bar.stacks }, `Stacks: ${bar.stacks}`)));
    }
    lines.push(line(fmt(
      "details.buffs.ended",
      { reason: t(`details.buffs.end.${endKey}`, BUFF_END_FALLBACK[endKey]) },
      BUFF_END_FALLBACK[endKey],
    ), "buffTooltipSub"));
    tooltip.replaceChildren(...lines);
    tooltip.hidden = false;
    moveTooltip(event);
  };
  const moveTooltip = (event) => {
    if (!tooltip || tooltip.hidden) return;
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
  body?.addEventListener("mouseover", (event) => {
    const barEl = event.target.closest?.(".buffBar");
    if (!barEl) return hideTooltip();
    showTooltip(barEl.closest(".buffRow"), Number(barEl.dataset.bar), event);
  });
  body?.addEventListener("mousemove", moveTooltip);
  body?.addEventListener("mouseleave", hideTooltip);

  i18n?.onChange?.(() => {
    abnormalNames = null;
    render();
  });
  syncToolbar();

  return {
    // `next`: { tracks, durationMs, targetId, targetName, playerId, actorIds, older, loading }
    setData(next) {
      data = next;
      render();
    },
    render,
    clear() {
      data = null;
      render();
    },
  };
};

window.createBuffTimeline = createBuffTimeline;
