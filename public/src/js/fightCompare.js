// Fight compare: two players of a saved fight side by side, and why one
// does more damage than the other (most useful within a class).
//
// A saved fight already holds every party member's skill rows, so this needs
// nothing new from the backend. In a saved fight's Details window, "Compare"
// shows, over the whole Details body:
//
//   1. The party: every member's DPS, damage, share, crit rate; an A and a B
//      button on each row picks the two to compare. A starts as you, B as the
//      top player of your class (else the top of the rest).
//   2. A vs B: who is ahead and by how much, and why. The damage gap splits
//      exactly (in logs) into
//        uptime        time spent hitting (gaps over CMP_IDLE_GAP_MS are idle)
//        pace          hits per second of that time
//        damage / hit  the average hit
//      with the hit-level rates (crit, back, perfect, double, multi-hit),
//      deaths, and (same class) the skills that make most of the gap and why.
//   3. Skills side by side: one row per skill with both players' damage,
//      hits, average, crit and back rates (same class), or each player's own
//      table next to the other's (different classes share no skills).
//
// details.js hands over its hooks (`window.fightCompare.attach`), like
// fightReplay.js does. The pure half (`buildCompareModel`, `cmpHeadToHead`,
// `cmpCompact`) has no DOM, so Node can load it to test it
// (tests/fight-compare.test.mjs).

// ── The model (no DOM) ──

const cmpNum = (v) => (Number.isFinite(Number(v)) ? Number(v) : 0);

// A pause between two hits longer than this counts as not attacking.
const CMP_IDLE_GAP_MS = 2500;

// Per-hit flag counts kept from each skill row, as rates of its hits.
const CMP_FLAGS = ["crit", "back", "perfect", "double", "multiHitCount"];

const cmpZeroFlags = () => Object.fromEntries(CMP_FLAGS.map((f) => [f, 0]));

// Time spent attacking: the fight less the pauses over CMP_IDLE_GAP_MS
// (before the first hit and after the last included). null without hit times.
const cmpActiveMs = (timestamps, durationMs) => {
  if (!timestamps.length || durationMs <= 0) return null;
  const ts = [...timestamps].sort((a, b) => a - b);
  let idle = 0;
  const gap = (d) => { if (d > CMP_IDLE_GAP_MS) idle += d - CMP_IDLE_GAP_MS; };
  gap(ts[0]);
  for (let i = 1; i < ts.length; i++) gap(ts[i] - ts[i - 1]);
  gap(durationMs - ts[ts.length - 1]);
  return Math.max(0, Math.min(durationMs, durationMs - idle));
};

// record -> { durationMs, totalDamage, members: [member], localActorId }.
// member: { actorId, name, job, jobId, damage, hits, flags, maxHit, dps,
// share, activeMs, deaths, deadMs, skills: Map<code, skill> }.
const buildCompareModel = (record) => {
  const details = record?.details && typeof record.details === "object" ? record.details : {};
  const actors = Array.isArray(record?.actors) ? record.actors : [];
  const rows = Array.isArray(details.skills) ? details.skills : [];
  const durationMs = cmpNum(details.battleTime) || cmpNum(record?.durationMs);
  const byId = new Map();
  const stamps = new Map();
  const member = (actorId) => {
    let m = byId.get(actorId);
    if (!m) {
      const a = actors.find((x) => x.actorId === actorId) || {};
      m = {
        actorId,
        name: a.nickname || `#${actorId}`,
        job: a.job || "",
        jobId: cmpNum(a.jobId),
        damage: 0, hits: 0, flags: cmpZeroFlags(), maxHit: 0,
        dps: 0, share: 0, activeMs: null, deaths: 0, deadMs: 0,
        skills: new Map(),
      };
      byId.set(actorId, m);
      stamps.set(actorId, []);
    }
    return m;
  };
  for (const row of rows) {
    if (!row || typeof row !== "object") continue;
    const dmg = cmpNum(row.dmg);
    if (dmg <= 0) continue;
    const m = member(row.actorId);
    const hits = cmpNum(row.time);
    m.damage += dmg;
    m.hits += hits;
    m.maxHit = Math.max(m.maxHit, cmpNum(row.maxDmg));
    // A DoT and its direct hit share a name; the code keeps them apart.
    const key = `${cmpNum(row.code)}`;
    let s = m.skills.get(key);
    if (!s) {
      s = { key, name: row.name || key, isDot: !!row.isDot, dmg: 0, hits: 0, flags: cmpZeroFlags(), maxHit: 0 };
      m.skills.set(key, s);
    }
    s.dmg += dmg;
    s.hits += hits;
    s.maxHit = Math.max(s.maxHit, cmpNum(row.maxDmg));
    for (const f of CMP_FLAGS) {
      s.flags[f] += cmpNum(row[f]);
      m.flags[f] += cmpNum(row[f]);
    }
    if (Array.isArray(row.hitTimestamps)) {
      const list = stamps.get(row.actorId);
      for (const t of row.hitTimestamps) if (Number.isFinite(Number(t))) list.push(Number(t));
    }
  }
  const deaths = Array.isArray(details.deaths) ? details.deaths : [];
  for (const d of deaths) {
    const m = byId.get(d?.actorId);
    if (!m) continue;
    m.deaths += 1;
    const end = d.revivedMs != null ? cmpNum(d.revivedMs) : durationMs;
    m.deadMs += Math.max(0, end - cmpNum(d.tsMs));
  }
  const members = [...byId.values()].sort((a, b) => b.damage - a.damage);
  const totalDamage = members.reduce((n, m) => n + m.damage, 0);
  for (const m of members) {
    m.dps = durationMs > 0 ? (m.damage * 1000) / durationMs : 0;
    m.share = totalDamage > 0 ? (m.damage / totalDamage) * 100 : 0;
    m.activeMs = cmpActiveMs(stamps.get(m.actorId), durationMs);
  }
  const localActorId = record?.localActorId != null ? cmpNum(record.localActorId) : null;
  return { durationMs, totalDamage, members, localActorId };
};

const cmpRate = (count, hits) => (hits > 0 ? (count / hits) * 100 : 0);
const cmpAvg = (x) => (x.hits > 0 ? x.damage / x.hits : 0);

// A vs B. gapPct: how far A's damage is above B's (negative: below).
// factors: the log split of that gap, each { key, a, b, x, effectPct, weight }:
// effectPct is the gap that factor alone makes, weight its share of the whole
// (the weights add up to 1; one against the gap's direction is negative).
// rates: hit-level rates side by side. skills: rows to show; drivers: the
// skills that make most of the gap (same class only).
const cmpHeadToHead = (model, aId, bId) => {
  const a = model.members.find((m) => m.actorId === aId);
  const b = model.members.find((m) => m.actorId === bId);
  if (!a || !b || a === b) return null;
  const gapPct = b.damage > 0 ? (a.damage / b.damage - 1) * 100 : null;

  const factors = [];
  const ln = (x, y) => (x > 0 && y > 0 ? Math.log(x / y) : null);
  const timed = a.activeMs > 0 && b.activeMs > 0;
  const push = (key, va, vb, x) => {
    if (x == null) return;
    factors.push({ key, a: va, b: vb, x, effectPct: (Math.exp(x) - 1) * 100 });
  };
  if (timed) {
    push("uptime", (a.activeMs / model.durationMs) * 100, (b.activeMs / model.durationMs) * 100, ln(a.activeMs, b.activeMs));
    const pa = a.hits / (a.activeMs / 1000);
    const pb = b.hits / (b.activeMs / 1000);
    push("pace", pa, pb, ln(pa, pb));
  } else {
    push("hits", a.hits, b.hits, ln(a.hits, b.hits));
  }
  push("perHit", cmpAvg(a), cmpAvg(b), ln(cmpAvg(a), cmpAvg(b)));
  const total = ln(a.damage, b.damage);
  for (const f of factors) f.weight = total ? f.x / total : 0;

  const rates = [
    ["crit", "crit"], ["back", "back"], ["perfect", "perfect"], ["double", "double"], ["multi", "multiHitCount"],
  ].map(([key, flag]) => ({ key, a: cmpRate(a.flags[flag], a.hits), b: cmpRate(b.flags[flag], b.hits) }));

  const sameClass = a.jobId > 0 && a.jobId === b.jobId;
  let skills;
  let drivers = [];
  if (sameClass) {
    const keys = new Set([...a.skills.keys(), ...b.skills.keys()]);
    skills = [...keys].map((key) => {
      const sa = a.skills.get(key) || null;
      const sb = b.skills.get(key) || null;
      return { key, name: (sa || sb).name, a: sa, b: sb, diff: (sa?.dmg || 0) - (sb?.dmg || 0) };
    }).sort((x, y) => Math.max(y.a?.dmg || 0, y.b?.dmg || 0) - Math.max(x.a?.dmg || 0, x.b?.dmg || 0));
    // The skills that make the gap, in its direction, with what differs.
    const sign = Math.sign(a.damage - b.damage) || 1;
    drivers = skills
      .filter((s) => Math.sign(s.diff) === sign)
      .sort((x, y) => Math.abs(y.diff) - Math.abs(x.diff))
      .slice(0, 3)
      .map((s) => {
        const ahead = sign > 0 ? s.a : s.b;
        const behind = sign > 0 ? s.b : s.a;
        let cause = "unused";
        if (behind && behind.hits > 0 && ahead.hits > 0) {
          const byHits = Math.log(ahead.hits / behind.hits);
          const byAvg = Math.log((ahead.dmg / ahead.hits) / (behind.dmg / behind.hits));
          cause = Math.abs(byHits) >= Math.abs(byAvg) ? "hits" : "avg";
        }
        return { ...s, cause, ahead, behind, aheadIsA: sign > 0 };
      });
  } else {
    const ranked = (m) => [...m.skills.values()].sort((x, y) => y.dmg - x.dmg);
    skills = { a: ranked(a), b: ranked(b) };
  }
  return { a, b, gapPct, factors, rates, sameClass, skills, drivers };
};

// ── The view ──

// 1234 -> "1.2k", 255957427 -> "256.0m": the meter's own k / m style.
const cmpCompact = (v) => {
  const n = Number(v) || 0;
  const a = Math.abs(n);
  if (a >= 1e9) return `${(n / 1e9).toFixed(2)}b`;
  if (a >= 1e6) return `${(n / 1e6).toFixed(1)}m`;
  if (a >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return `${Math.round(n)}`;
};

// English, for any language without a translation (src/data/i18n/ui/*.json,
// under details.compare).
const CMP_EN = {
  button: "Compare",
  party: "Party",
  vs: "A vs B",
  why: "Why",
  skills: "Skills",
  name: "Name",
  damage: "Damage",
  dps: "DPS",
  share: "Share",
  hits: "Hits",
  crit: "Crit",
  avg: "Avg",
  back: "Back",
  perfect: "Perfect",
  double: "Double",
  multi: "Multi",
  maxHit: "Max hit",
  diff: "Diff",
  swap: "Swap",
  pickHint: "Pick the two to compare with A / B on each row",
  noData: "No damage recorded",
  needTwo: "Comparing needs two players",
  ahead: "{winner} out-damages {loser} by {pct}",
  even: "About the same DPS",
  uptime: "Uptime (time spent attacking)",
  pace: "Pace (hits per active second)",
  hitsF: "Hits",
  perHit: "Damage per hit",
  factorHint: "The DPS gap split into three multiplied factors; bars show each one's share, the right column the gap it alone makes",
  rateTitle: "Hit quality",
  rateHint: "The rates behind damage per hit",
  drivers: "Skill behind the gap",
  mainCause: "Mainly",
  counter: "{name} is ahead on",
  unused: "{name} never used it",
  causeHits: "more uses",
  causeAvg: "harder hits",
  deaths: "{name} died {count}x ({sec}s down)",
  otherClass: "Different classes: each player's skills are listed side by side",
  perSec: "/s",
  times: "hits",
  pt: "pt",
};

const cmpText = () => {
  const i18n = typeof window !== "undefined" ? window.i18n : null;
  const tx = {};
  for (const [k, v] of Object.entries(CMP_EN)) tx[k] = i18n?.t?.(`details.compare.${k}`, v) ?? v;
  // The templates with {placeholders}, kept before tx.ahead & co. become functions.
  const templates = { ahead: tx.ahead, unused: tx.unused, deaths: tx.deaths, counter: tx.counter };
  const fmt = (k, vars) => templates[k].replace(/\{(\w+)\}/g, (_, x) => (vars[x] ?? ""));
  tx.ahead = (winner, loser, pct) => fmt("ahead", { winner, loser, pct });
  tx.unused = (name) => fmt("unused", { name });
  tx.deaths = (name, count, sec) => fmt("deaths", { name, count, sec });
  tx.counter = (name) => fmt("counter", { name });
  return tx;
};

const CMP_A = "#4fc3f7";
const CMP_B = "#ffb74d";


const cmpEsc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const cmpPct = (v, d = 1) => `${v.toFixed(d)}%`;
const cmpSigned = (v, d = 1, unit = "%") => `${v >= 0 ? "+" : ""}${v.toFixed(d)}${unit}`;

const attachCompare = ({ panel, record, isOpen }) => {
  if (!panel || panel.dataset.fightCompare) return;
  panel.dataset.fightCompare = "1";

  const controls = panel.querySelector(".detailsHeaderControls");
  const body = panel.querySelector(".detailsBody");
  if (!controls || !body) return;

  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "fightCompareBtn";
  controls.prepend(btn);
  const view = document.createElement("div");
  view.className = "fightCompare";
  body.prepend(view);

  let pickA = null;
  let pickB = null;
  let pickedFor = null;
  let open = false;

  const partyHtml = (tx, model) => {
    const top = model.members[0]?.share || 1;
    const rows = model.members.map((m) => {
      const id = m.actorId;
      const cls = id === pickA ? "isA" : id === pickB ? "isB" : "";
      return `<tr class="${cls}">
        <td style="white-space:nowrap"><button type="button" class="pk${id === pickA ? " onA" : ""}" data-pick="A" data-id="${id}">A</button><button type="button" class="pk${id === pickB ? " onB" : ""}" data-pick="B" data-id="${id}">B</button></td>
        <td class="nm">${cmpEsc(m.name)} <span class="dim">${cmpEsc(m.job)}</span></td>
        <td class="num" style="font-weight:700">${cmpCompact(m.dps)}</td>
        <td class="num">${cmpCompact(m.damage)}</td>
        <td><div class="cmpShare"><div class="track"><div class="fill" style="width:${(m.share / top) * 100}%"></div></div><span class="num" style="width:44px">${cmpPct(m.share)}</span></div></td>
        <td class="num">${cmpPct(cmpRate(m.flags.crit, m.hits))}</td>
        <td class="num dim">${m.hits.toLocaleString("en-US")}</td>
      </tr>`;
    }).join("");
    return `<h3>${tx.party}<small>${tx.pickHint}</small></h3>
      <table class="cmpParty"><thead><tr><th></th><th class="nm">${tx.name}</th><th class="num">${tx.dps}</th><th class="num">${tx.damage}</th><th>${tx.share}</th><th class="num">${tx.crit}</th><th class="num">${tx.hits}</th></tr></thead><tbody>${rows}</tbody></table>`;
  };

  const vsHtml = (tx, h) => {
    const { a, b } = h;
    const gap = h.gapPct;
    const aAhead = a.damage >= b.damage;
    const winner = aAhead ? a : b;
    const loser = aAhead ? b : a;
    const winPct = loser.damage > 0 ? (winner.damage / loser.damage - 1) * 100 : 0;
    const verdict = winPct < 1 ? tx.even : tx.ahead(cmpEsc(winner.name), cmpEsc(loser.name), cmpPct(winPct));
    const side = (m, cls) => `<div class="side ${cls}"><b class="c${cls.toUpperCase()}">${cmpEsc(m.name)}</b><span class="dim">${cmpEsc(m.job)}</span>
      <div class="big">${cmpCompact(m.dps)} <small class="dim" style="font-size:12px">DPS</small></div>
      <div class="dim">${cmpCompact(m.damage)} · ${cmpPct(m.share)}</div></div>`;
    let html = `<h3>${tx.vs}</h3><div class="cmpVs">${side(a, "a")}<div class="mid">
      <div class="gap ${aAhead ? "cA" : "cB"}">${gap == null ? "-" : cmpSigned(gap)}</div>
      <button type="button" data-swap="1">⇄ ${tx.swap}</button></div>${side(b, "b")}</div>
      <div class="cmpVerdict">${verdict}</div>`;

    // Why: the gap's factors.
    const fmt = {
      uptime: (v) => cmpPct(v),
      pace: (v) => `${v.toFixed(2)}${tx.perSec}`,
      hits: (v) => Math.round(v).toLocaleString("en-US"),
      perHit: (v) => cmpCompact(v),
    };
    const label = { uptime: tx.uptime, pace: tx.pace, hits: tx.hitsF, perHit: tx.perHit };
    const maxW = Math.max(...h.factors.map((f) => Math.abs(f.x)), 1e-9);
    html += `<h3>${tx.why}<small>${tx.factorHint}</small></h3><div class="cmpFactors">
      <div class="hd"></div><div class="hd num cA">${cmpEsc(a.name)}</div><div class="hd num cB">${cmpEsc(b.name)}</div><div class="hd"></div><div class="hd num">${tx.diff}</div>`;
    for (const f of h.factors) {
      const favorsA = f.x >= 0;
      const w = (Math.abs(f.x) / maxW) * 100;
      html += `<div>${label[f.key]}</div>
        <div class="num${favorsA ? " cA" : ""}" style="${favorsA ? "font-weight:700" : ""}">${fmt[f.key](f.a)}</div>
        <div class="num${!favorsA ? " cB" : ""}" style="${!favorsA ? "font-weight:700" : ""}">${fmt[f.key](f.b)}</div>
        <div class="bar"><i style="width:${w}%;background:${favorsA ? CMP_A : CMP_B}"></i></div>
        <div class="num ${favorsA ? "cA" : "cB"}">${cmpSigned(f.effectPct)}</div>`;
    }
    html += "</div>";

    // The verdict in words: the factors for the leader, biggest first, and
    // the ones that go the other way.
    if (winPct >= 1) {
      const lead = aAhead ? 1 : -1;
      // From the leader's side: the gap this factor alone gives them.
      const named = (f) => `${label[f.key]}（${cmpSigned((Math.exp(Math.abs(f.x)) - 1) * 100)}）`;
      const pro = h.factors.filter((f) => f.x * lead > 0).sort((x, y) => Math.abs(y.x) - Math.abs(x.x));
      const con = h.factors.filter((f) => f.x * lead < 0).sort((x, y) => Math.abs(y.x) - Math.abs(x.x));
      let line = `${tx.mainCause}: ${pro.map(named).join("、")}`;
      if (con.length) line += `。${tx.counter(cmpEsc(loser.name))}: ${con.map((f) => label[f.key]).join("、")}`;
      html = html.replace(`<div class="cmpVerdict">${verdict}</div>`, `<div class="cmpVerdict">${verdict}</div><div class="cmpSummary">${line}</div>`);
    }

    // Notes: deaths, and the skills that make the gap.
    const notes = [];
    for (const m of [a, b]) {
      if (m.deaths > 0) notes.push(tx.deaths(cmpEsc(m.name), m.deaths, Math.round(m.deadMs / 1000)));
    }
    for (const d of h.drivers) {
      const who = d.aheadIsA ? a : b;
      const other = d.aheadIsA ? b : a;
      let why;
      if (d.cause === "unused") why = tx.unused(cmpEsc(other.name));
      else if (d.cause === "hits") why = `${tx.causeHits}（${d.ahead.hits} / ${d.behind.hits} ${tx.times}）`;
      else why = `${tx.causeAvg}（${tx.avg} ${cmpCompact(d.ahead.dmg / d.ahead.hits)} / ${cmpCompact(d.behind.dmg / d.behind.hits)}）`;
      notes.push(`${tx.drivers}: <b>${cmpEsc(d.name)}</b> — <span class="${d.aheadIsA ? "cA" : "cB"}">${cmpEsc(who.name)} +${cmpCompact(Math.abs(d.diff))}</span>、${why}`);
    }
    if (notes.length) html += `<ul class="cmpNotes">${notes.map((n) => `<li>${n}</li>`).join("")}</ul>`;

    // Hit quality: the rates behind damage per hit.
    html += `<h3>${tx.rateTitle}<small>${tx.rateHint}</small></h3><div class="cmpRates">`;
    for (const r of h.rates) {
      if (r.a === 0 && r.b === 0) continue;
      const d = r.a - r.b;
      html += `<div class="cmpRate"><div class="lb">${tx[r.key]}</div><div class="vals">
        <span class="cA">${cmpPct(r.a)}</span><span class="cB">${cmpPct(r.b)}</span></div>
        <div class="d ${Math.abs(d) < 0.5 ? "dim" : d > 0 ? "cA" : "cB"}">${cmpSigned(d, 1, tx.pt)}</div></div>`;
    }
    html += `<div class="cmpRate"><div class="lb">${tx.maxHit}</div><div class="vals">
      <span class="cA">${cmpCompact(a.maxHit)}</span><span class="cB">${cmpCompact(b.maxHit)}</span></div><div class="d dim">&nbsp;</div></div></div>`;
    return html;
  };

  const skillCells = (s, m, cls, maxDmg, best) => {
    if (!s) return `<td class="num ${cls} dim">-</td><td class="num ${cls}"></td><td class="num ${cls}"></td><td class="num ${cls}"></td><td class="num ${cls}"></td>`;
    const color = cls === "ga" ? CMP_A : CMP_B;
    const share = m.damage > 0 ? (s.dmg / m.damage) * 100 : 0;
    return `<td class="num ${cls} cmpDmg${best ? " best" : ""}" title="${s.dmg.toLocaleString("en-US")}"><i style="width:${(s.dmg / maxDmg) * 100}%;background:${color}"></i><span>${cmpCompact(s.dmg)}</span> <span class="dim">${cmpPct(share, 0)}</span></td>
      <td class="num ${cls}">${s.hits}</td>
      <td class="num ${cls}">${cmpCompact(s.hits > 0 ? s.dmg / s.hits : 0)}</td>
      <td class="num ${cls}">${s.isDot ? "-" : cmpPct(cmpRate(s.flags.crit, s.hits), 0)}</td>
      <td class="num ${cls}">${s.isDot ? "-" : cmpPct(cmpRate(s.flags.back, s.hits), 0)}</td>`;
  };

  const skillsHtml = (tx, h) => {
    const { a, b } = h;
    const head = (cls) => `<th class="num ${cls}">${tx.damage}</th><th class="num ${cls}">${tx.hits}</th><th class="num ${cls}">${tx.avg}</th><th class="num ${cls}">${tx.crit}</th><th class="num ${cls}">${tx.back}</th>`;
    if (h.sameClass) {
      const maxDmg = Math.max(1, ...h.skills.map((s) => Math.max(s.a?.dmg || 0, s.b?.dmg || 0)));
      const rows = h.skills.map((s) => {
        const da = s.a?.dmg || 0;
        const db = s.b?.dmg || 0;
        const diffCls = s.diff === 0 ? "dim" : s.diff > 0 ? "cA" : "cB";
        return `<tr><td class="sk">${cmpEsc(s.name)}</td>${skillCells(s.a, a, "ga", maxDmg, da > db)}${skillCells(s.b, b, "gb", maxDmg, db > da)}
          <td class="num ${diffCls}">${s.diff === 0 ? "-" : (s.diff > 0 ? "+" : "-") + cmpCompact(Math.abs(s.diff))}</td></tr>`;
      }).join("");
      return `<h3>${tx.skills}</h3><div class="cmpScroll"><table class="cmpSkills"><thead>
        <tr><th></th><th colspan="5" class="ga cA" style="text-align:center">${cmpEsc(a.name)}</th><th colspan="5" class="gb cB" style="text-align:center">${cmpEsc(b.name)}</th><th></th></tr>
        <tr><th>${tx.skills}</th>${head("ga")}${head("gb")}<th class="num">${tx.diff}</th></tr></thead><tbody>${rows}</tbody></table></div>`;
    }
    const one = (m, list, cls) => {
      const maxDmg = Math.max(1, list[0]?.dmg || 0);
      const rows = list.map((s) => `<tr><td class="sk">${cmpEsc(s.name)}</td>${skillCells(s, m, cls, maxDmg, false)}</tr>`).join("");
      return `<div class="cmpScroll"><table class="cmpSkills"><thead><tr><th colspan="6" class="${cls} ${cls === "ga" ? "cA" : "cB"}">${cmpEsc(m.name)} <span class="dim">${cmpEsc(m.job)}</span></th></tr>
        <tr><th>${tx.skills}</th>${head(cls)}</tr></thead><tbody>${rows}</tbody></table></div>`;
    };
    return `<h3>${tx.skills}<small>${tx.otherClass}</small></h3><div class="cmpTwo">${one(a, h.skills.a, "ga")}${one(b, h.skills.b, "gb")}</div>`;
  };

  const render = () => {
    const tx = cmpText();
    btn.textContent = tx.button;
    const rec = record();
    // Only a saved fight has the whole party's rows; hide for a live one.
    btn.style.display = rec ? "" : "none";
    if (!rec || !open) return;
    const model = buildCompareModel(rec);
    if (!model.members.length) {
      view.innerHTML = `<div class="dim">${tx.noData}</div>`;
      return;
    }
    const ids = model.members.map((m) => m.actorId);
    if (pickedFor !== rec.id || !ids.includes(pickA) || !ids.includes(pickB) || pickA === pickB) {
      pickedFor = rec.id;
      // A: you when the fight knows you, else the top. B: the top player of
      // A's class, else the top of the rest.
      pickA = ids.includes(model.localActorId) ? model.localActorId : ids[0];
      const job = model.members.find((m) => m.actorId === pickA)?.jobId;
      const sameClass = model.members.find((m) => m.actorId !== pickA && job > 0 && m.jobId === job);
      pickB = sameClass ? sameClass.actorId : ids.find((id) => id !== pickA) ?? null;
    }
    const h = cmpHeadToHead(model, pickA, pickB);
    const scroll = body.scrollTop;
    view.innerHTML = partyHtml(tx, model) + (h ? vsHtml(tx, h) + skillsHtml(tx, h) : `<p class="dim">${tx.needTwo}</p>`);
    body.scrollTop = scroll;
  };

  view.addEventListener("click", (e) => {
    const pk = e.target.closest("[data-pick]");
    if (pk) {
      const id = Number(pk.dataset.id);
      if (pk.dataset.pick === "A") {
        if (id === pickB) pickB = pickA;
        pickA = id;
      } else {
        if (id === pickA) pickA = pickB;
        pickB = id;
      }
      render();
      return;
    }
    if (e.target.closest("[data-swap]")) {
      [pickA, pickB] = [pickB, pickA];
      render();
    }
  });

  btn.addEventListener("click", () => {
    open = !open;
    btn.classList.toggle("isActive", open);
    panel.classList.toggle("fightCompareOpen", open);
    render();
  });

  // The window shows another fight, or closes: the comparison closes with it.
  const reset = () => {
    open = false;
    btn.classList.remove("isActive");
    panel.classList.remove("fightCompareOpen");
    render();
  };
  // The Details window closed: so does the comparison.
  if (typeof MutationObserver !== "undefined") {
    new MutationObserver(() => {
      if (open && !panel.classList.contains("open")) reset();
    }).observe(panel, { attributes: true, attributeFilter: ["class"] });
  }
  return { refresh: render, reset, isOpen };
};

if (typeof module !== "undefined" && module.exports) {
  module.exports = { buildCompareModel, cmpHeadToHead, cmpCompact, cmpActiveMs, cmpText };
}
if (typeof window !== "undefined") {
  window.fightCompare = { attach: attachCompare };
}
