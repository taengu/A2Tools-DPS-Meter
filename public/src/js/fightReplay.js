// Fight replay: a saved fight in Details, played back from its start, or
// scrubbed to any moment, at a chosen speed. Every figure the window shows
// for the fight (the party's damage, DPS and share, the stats, healing, the
// skills' damage, hits and crits) is then what had happened by that moment;
// the fight timeline (chart, buffs, casts) is drawn once for the whole fight
// and shown up to the moment, the rest faint.
//
// What the record has to cut by: each skill row's hit times (ms into the
// fight; heals too, from meter 2.0.56), and each death's time. It keeps no
// per-hit amounts or flags, so a skill's damage and its crit, back, parry...
// counts are given out evenly over its hits up to the moment, as the DPS
// chart already does with its damage. Hit counts and deaths are exact. Not
// timed at all, and so left at the whole fight's (and greyed): Damage Taken,
// the skills' Min / Max, and the buffs' uptimes.
//
// Saved fights only: a live fight's details arrive merged per target from
// the backend every 2 s, and replaying that would mean holding a second copy
// of the fight in step with it.
//
// details.js hands this file its hooks (`window.fightReplay.attach`), and
// draws the figures from the cut ones with `renderFigures`.

// ── The cut (no DOM; Node can load this part to test it) ──

const REPLAY_COUNT_FIELDS = [
  "crit", "parry", "back", "frontal", "perfect", "double", "shieldBlock", "ironWall",
  "regeneration", "perfectBlock", "miss", "resist", "regen",
  "multiHitCount", "multiHitDamage", "multiHitHits",
];

// Hits at or before `t` in ascending `ts`.
const replayHitsUpTo = (ts, t) => {
  let lo = 0;
  let hi = ts.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (ts[mid] <= t) lo = mid + 1;
    else hi = mid;
  }
  return lo;
};

const replayIndexRows = (rows) => (Array.isArray(rows) ? rows : [])
  .filter((row) => row && typeof row === "object")
  .map((row) => {
    const ts = (Array.isArray(row.hitTimestamps) ? row.hitTimestamps : [])
      .map(Number).filter(Number.isFinite).sort((a, b) => a - b);
    return { row, ts };
  });

// Built once when a replay starts: each row's hit times sorted, and the
// whole fight's figures the cut ones are scaled against.
const buildReplayModel = (record) => {
  const details = record?.details && typeof record.details === "object" ? record.details : {};
  const damage = replayIndexRows(details.skills);
  const heals = replayIndexRows(details.healSkills);
  const lastHit = [...damage, ...heals].reduce((m, { ts }) => (ts.length ? Math.max(m, ts[ts.length - 1]) : m), 0);
  const durationMs = Number(details.battleTime) || Number(record?.durationMs) || lastHit;
  const fullSkillDamage = damage.reduce((sum, { row }) => sum + (Number(row.dmg) || 0), 0);
  return {
    record,
    details,
    damage,
    heals,
    durationMs,
    fullSkillDamage,
    totalDamage: Number(record?.totalDamage) || fullSkillDamage,
    damageTimed: damage.some(({ ts }) => ts.length > 0),
    // A fight saved before heals had times: its healing stays the whole
    // fight's (with a note) rather than all arriving at the end.
    healTimed: heals.some(({ row, ts }) => ts.length > 0 && (Number(row.dmg) || 0) > 0),
  };
};

// A row as it stood at `t`: null before its first hit, the row itself once
// all of it has happened. A row with no hit times (an older or partial
// record) counts at the end, unless `untimedWhole`.
const replayCutRow = ({ row, ts }, t, atEnd, untimedWhole) => {
  const n = ts.length;
  if (!n) return untimedWhole || atEnd ? row : null;
  const k = atEnd ? n : replayHitsUpTo(ts, t);
  if (k >= n) return row;
  if (k <= 0) return null;
  const f = k / n;
  const out = { ...row, hitTimestamps: ts.slice(0, k) };
  const time = Number(row.time) || 0;
  out.time = time === n ? k : Math.round(time * f);
  out.dmg = Math.round((Number(row.dmg) || 0) * f);
  for (const field of REPLAY_COUNT_FIELDS) {
    if (row[field] !== undefined) out[field] = Math.round((Number(row[field]) || 0) * f);
  }
  // Saved before the flags were renamed.
  if (row.smite !== undefined && row.regeneration === undefined) out.regeneration = Math.round((Number(row.smite) || 0) * f);
  if (row.powershard !== undefined && row.perfectBlock === undefined) out.perfectBlock = Math.round((Number(row.powershard) || 0) * f);
  return out;
};

// The record's details as they stood `t` ms into the fight, in the shape
// load_fight returns them, plus the target's per-actor damage and total.
const cutReplayAt = (model, tMs) => {
  const t = Math.max(0, Number(tMs) || 0);
  const atEnd = t >= model.durationMs;
  const skills = [];
  const actorDamage = {};
  let skillDamage = 0;
  for (const entry of model.damage) {
    const row = replayCutRow(entry, t, atEnd, false);
    if (!row) continue;
    skills.push(row);
    const dmg = Number(row.dmg) || 0;
    skillDamage += dmg;
    const id = Number(row.actorId);
    if (id > 0) actorDamage[id] = (actorDamage[id] || 0) + dmg;
  }
  const healSkills = [];
  for (const entry of model.heals) {
    const row = replayCutRow(entry, t, atEnd, !model.healTimed);
    if (row) healSkills.push(row);
  }
  const deathsIn = Array.isArray(model.details.deaths) ? model.details.deaths : null;
  const deaths = deathsIn && (atEnd ? deathsIn : deathsIn
    .filter((d) => Number(d?.tsMs) <= t)
    .map((d) => (d.revivedMs != null && Number(d.revivedMs) > t ? { actorId: d.actorId, tsMs: d.tsMs } : d)));
  // Damage the record names no actor for keeps the whole fight's share.
  const totalDamage = atEnd
    ? model.totalDamage
    : model.fullSkillDamage > 0 ? Math.round(model.totalDamage * (skillDamage / model.fullSkillDamage)) : skillDamage;
  // DPS is over the time so far, at least a second (as Details' Avg DPS is).
  const battleTime = atEnd ? model.durationMs : Math.max(1000, Math.round(t));
  const details = { ...model.details, battleTime, totalTargetDamage: totalDamage, skills, healSkills };
  if (deathsIn) details.deaths = deaths;
  // HP loss has no hit times. Do not distribute Others over the replay;
  // the observed aggregate is available only at the full-fight endpoint.
  const worldBossObservedDamage = atEnd ? model.record?.worldBossObservedDamage : null;
  return { t: atEnd ? model.durationMs : t, atEnd, details, actorDamage, totalDamage, battleTime, worldBossObservedDamage };
};

if (typeof module !== "undefined" && module.exports) {
  module.exports = { buildReplayModel, cutReplayAt, replayHitsUpTo };
}

// ── The controls ──

(() => {
  if (typeof window === "undefined" || typeof document === "undefined") return;

  const SPEEDS = [0.5, 1, 2, 4, 8, 16, 32];
  const SPEED_KEY = "a2.details.replaySpeed";
  // Figures are drawn at most this often while playing (the scrubber, the
  // time and the timeline's mask move every frame); less often in a fight so
  // big that drawing them takes long (Details' peak DPS goes through every
  // hit), so they never take more than about a sixth of the time.
  const FIGURE_INTERVAL_MS = 100;
  // How often the playhead and the progress step while playing: no
  // transition at all (any running one, even on a 2 px line, makes the
  // browser draw a full frame at every screen refresh; 2026-10-18). The big
  // shade steps every FIGURE_INTERVAL_MS. 50 ms (20 a second) measured within
  // budget on an RTX 2070; 33 ms did not quite. window.__A2_REPLAY_STEP_MS
  // overrides it, for measuring.
  const MOVER_STEP_MS = Number(window.__A2_REPLAY_STEP_MS) > 0 ? Number(window.__A2_REPLAY_STEP_MS) : 50;
  const FIGURE_SHARE = 6;
  // How faint the timeline is past the moment.
  const FUTURE_ALPHA = 0.16;

  const t = (key, fallback) => window.i18n?.t?.(key, fallback) ?? fallback;
  const clock = (ms) => {
    const whole = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
    return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
  };

  const attach = (hooks) => {
    if (!hooks?.root || !hooks?.timeline) return;
    const panel = hooks.root;
    const timeline = hooks.timeline;
    const scroll = hooks.timelineScroll;

    let speed = 4;
    try {
      const saved = Number(localStorage.getItem(SPEED_KEY));
      if (SPEEDS.includes(saved)) speed = saved;
    } catch { /* default */ }

    // ── DOM ──
    const bar = document.createElement("div");
    bar.className = "fightReplayBar";
    bar.hidden = true;
    bar.setAttribute("role", "group");
    bar.dataset.i18nAriaLabel = "details.replay.label";
    bar.setAttribute("aria-label", t("details.replay.label", "Replay"));

    const controls = document.createElement("div");
    controls.className = "fightReplayControls";

    const playBtn = document.createElement("button");
    playBtn.type = "button";
    playBtn.className = "fightReplayBtn fightReplayPlay";
    // A labelled pill: its icon and what it does ("Play replay", "Pause"),
    // in place of a bare icon beside a "Replay" label.
    playBtn.innerHTML = '<i data-lucide="play" class="fightReplayIconPlay" aria-hidden="true"></i>'
      + '<i data-lucide="pause" class="fightReplayIconPause" aria-hidden="true"></i>'
      + '<span class="fightReplayPlayText"></span>';

    const timeEl = document.createElement("span");
    timeEl.className = "fightReplayTime";
    timeEl.setAttribute("aria-hidden", "true");

    const speedSel = document.createElement("select");
    speedSel.className = "fightReplaySpeed";
    speedSel.dataset.i18nTitle = "details.replay.speed";
    speedSel.dataset.i18nAriaLabel = "details.replay.speed";
    speedSel.title = t("details.replay.speed", "Playback speed");
    speedSel.setAttribute("aria-label", speedSel.title);
    for (const s of SPEEDS) {
      const opt = document.createElement("option");
      opt.value = String(s);
      opt.textContent = `${s}×`;
      speedSel.appendChild(opt);
    }
    speedSel.value = String(speed);

    controls.append(playBtn, timeEl, speedSel);

    const scrubWrap = document.createElement("div");
    scrubWrap.className = "fightReplayScrubWrap";
    const scrub = document.createElement("input");
    scrub.type = "range";
    scrub.className = "fightReplayScrub";
    scrub.min = "0";
    scrub.max = "0";
    scrub.step = "any";
    scrub.value = "0";
    scrub.dataset.i18nAriaLabel = "details.replay.position";
    scrub.setAttribute("aria-label", t("details.replay.position", "Replay position"));
    // While playing, progress shows on a track of its own, moved by the GPU.
    // The slider stays (invisible, and still takes a drag) and gets the
    // position only when paused or dragged: written every frame, a native
    // slider repainted the whole page with it (2026-10-18, on the owner's PC).
    const prog = document.createElement("div");
    prog.className = "fightReplayProg";
    prog.setAttribute("aria-hidden", "true");
    const progFill = document.createElement("div");
    progFill.className = "fightReplayProgFill";
    const progKnob = document.createElement("div");
    progKnob.className = "fightReplayProgKnob";
    prog.append(progFill, progKnob);
    scrubWrap.append(prog, scrub);

    const exitBtn = document.createElement("button");
    exitBtn.type = "button";
    exitBtn.className = "fightReplayBtn fightReplayExit";
    exitBtn.innerHTML = '<i data-lucide="x" aria-hidden="true"></i>';
    exitBtn.dataset.i18nTitle = "details.replay.exit";
    exitBtn.dataset.i18nAriaLabel = "details.replay.exit";
    exitBtn.title = t("details.replay.exit", "End replay (show the whole fight)");
    exitBtn.setAttribute("aria-label", exitBtn.title);

    const note = document.createElement("div");
    note.className = "fightReplayNote";
    note.dataset.i18n = "details.replay.wholeFightNote";
    note.textContent = t("details.replay.wholeFightNote",
      "Damage Taken, Min / Max and buff uptimes show the whole fight.");
    const healNote = document.createElement("div");
    healNote.className = "fightReplayNote fightReplayHealNote";
    healNote.dataset.i18n = "details.replay.noHealTiming";
    healNote.textContent = t("details.replay.noHealTiming",
      "Healing in this fight has no times (an older version saved it): it shows the whole fight.");
    const othersNote = document.createElement("div");
    othersNote.className = "fightReplayNote";
    othersNote.dataset.i18n = "details.replay.noOthersTiming";
    othersNote.textContent = t("details.replay.noOthersTiming",
      "Replay excludes Others because their hit times were not recorded. End replay to see the observed total.");

    bar.append(controls, scrubWrap, exitBtn, note, healNote, othersNote);

    // Under the title row, where it is seen at once; its columns still line
    // the slider up with the chart below (styles.css).
    const header = timeline.closest(".detailsPanel")?.querySelector(".detailsHeader");
    if (header) {
      bar.classList.add("isTop");
      header.after(bar);
    } else {
      const axis = timeline.querySelector(".fightTimelineAxis");
      if (axis?.nextSibling) timeline.insertBefore(bar, axis.nextSibling);
      else timeline.appendChild(bar);
    }

    const head = document.createElement("div");
    head.className = "fightReplayHead";
    head.hidden = true;
    head.setAttribute("aria-hidden", "true");
    timeline.appendChild(head);
    // The part of the timeline after the moment, dimmed by a shade that
    // slides with it. A mask over the timeline, changed every frame, made the
    // browser repaint the whole timeline every frame, and a web page's replay
    // slowed the whole browser down (2026-10-18). A shade moved by transform
    // repaints nothing.
    const shade = document.createElement("div");
    shade.className = "fightReplayShade";
    shade.hidden = true;
    shade.setAttribute("aria-hidden", "true");
    const shadeFill = document.createElement("div");
    shadeFill.className = "fightReplayShadeFill";
    shade.appendChild(shadeFill);
    timeline.appendChild(shade);
    let shadeFor = null;

    try { window.lucide?.createIcons?.(); } catch { /* text-less buttons keep their labels */ }

    // ── State ──
    let record = null;
    let model = null;
    let active = false;
    let playing = false;
    let at = 0; // ms into the fight
    let frame = 0;
    let dragging = false; // the slider held (see the drag handling below)
    let lastFiguresTs = 0;
    let figuresPending = false;
    let cutSeq = 0;
    let geometry = null; // the timeline's lane edges, re-read on resize
    let ownRender = false;
    let figureCostMs = 0;

    const durationMs = () => Number(hooks.durationMs?.()) || model?.durationMs || 0;

    const syncPlayButton = () => {
      bar.classList.toggle("isPlaying", playing);
      panel.classList.toggle("isReplayPlaying", playing);
      const key = playing ? "details.replay.pause" : "details.replay.play";
      const text = playing ? t(key, "Pause") : t(key, "Play replay");
      playBtn.dataset.i18nTitle = key;
      playBtn.dataset.i18nAriaLabel = key;
      playBtn.title = text;
      playBtn.setAttribute("aria-label", text);
      const words = playBtn.querySelector(".fightReplayPlayText");
      if (words) {
        words.dataset.i18n = key;
        if (words.textContent !== text) words.textContent = text;
      }
    };

    const readGeometry = () => {
      if (!scroll) return null;
      const m = hooks.metrics?.() || { gutter: 0, right: 0 };
      const left = m.gutter;
      const right = scroll.clientWidth - m.right;
      if (!(right > left)) return null;
      return { left, right, top: scroll.offsetTop, height: scroll.clientHeight, x0: scroll.offsetLeft };
    };

    // The timeline up to the moment, the rest faint, and a line at it.
    // `withShade`: false while playing between the shade's own steps.
    const paintTimeline = (withShade = true) => {
      if (!scroll) return;
      if (!active) {
        head.hidden = true;
        shade.hidden = true;
        shadeFor = null;
        return;
      }
      if (!geometry) geometry = readGeometry();
      const dur = durationMs();
      if (!geometry || dur <= 0) return;
      const width = geometry.right - geometry.left;
      const x = geometry.left + Math.min(1, at / dur) * width;
      if (shadeFor !== geometry) {
        // Placed once per layout; each frame only moves it.
        shadeFor = geometry;
        shade.style.left = `${geometry.x0 + geometry.left}px`;
        shade.style.top = `${geometry.top}px`;
        shade.style.width = `${width}px`;
        shade.style.height = `${geometry.height}px`;
        shadeFill.style.opacity = String(1 - FUTURE_ALPHA);
        head.style.top = `${geometry.top}px`;
        head.style.height = `${geometry.height}px`;
      }
      if (shade.hidden) shade.hidden = false;
      if (head.hidden) head.hidden = false;
      if (shadeFill.style.transition !== "none") shadeFill.style.transition = "none";
      if (head.style.transition !== "none") head.style.transition = "none";
      if (withShade) shadeFill.style.transform = `translateX(${(x - geometry.left).toFixed(1)}px)`;
      head.style.transform = `translateX(${(geometry.x0 + x).toFixed(1)}px)`;
    };

    let progWidth = 0;
    const paintControls = () => {
      const dur = durationMs();
      const shown = active ? at : dur;
      const share = dur > 0 ? Math.min(1, shown / dur) : 1;
      if (!playing) {
        scrub.max = String(dur);
        scrub.value = String(shown);
        scrub.style.setProperty("--range-pct", `${(share * 100).toFixed(2)}%`);
      }
      paintClock(shown, dur);
    };
    const paintClock = (shown, dur) => {
      const text = `${clock(shown)} / ${clock(dur)}`;
      // Once a second, not every frame: the clock shows whole seconds.
      if (text !== timeEl.textContent) {
        // The text node's data, not textContent: setting that replaces the
        // node, and a drag changes the second on nearly every frame.
        if (timeEl.firstChild?.nodeType === 3 && timeEl.childNodes.length === 1) timeEl.firstChild.data = text;
        else timeEl.textContent = text;
        scrub.setAttribute("aria-valuetext", t("details.replay.positionText", "{at} of {total}")
          .replace("{at}", clock(shown)).replace("{total}", clock(dur)));
      }
    };

    // Cut the fight at the moment and draw the figures from it. Resolves in
    // microtasks (load_fight's details need no IPC), so a full redraw the
    // observer caught is replaced before it is painted.
    const drawFigures = (duringDrag = false) => {
      if (!active || !model) return;
      // Nothing redraws the figures while the slider is held, but its settle.
      if (dragging && !duringDrag) {
        figuresPending = true;
        return;
      }
      const seq = ++cutSeq;
      // HEAL in a fight whose heals have no times: every figure there is
      // healing, so all of it stays the whole fight's (the note says so).
      if (!model.healTimed && panel.classList.contains("isHealMode")) {
        ownRender = true;
        try { hooks.renderFigures(null); } finally { observer.takeRecords(); ownRender = false; }
        return;
      }
      const cut = cutReplayAt(model, at);
      const sel = hooks.selection?.() || {};
      const attackerIds = Array.isArray(sel.attackerIds) && sel.attackerIds.length ? sel.attackerIds : null;
      const row = { id: null, job: "", name: record?.bossName || "" };
      const load = (ids) => {
        // getDetails reads the override before its first await.
        const prev = window._historyDetailsOverride;
        window._historyDetailsOverride = cut.details;
        try {
          return hooks.getDetails(row, {
            targetId: record?.targetId,
            attackerIds: ids,
            totalTargetDamage: cut.totalDamage,
            showSkillIcons: !ids,
          });
        } finally {
          window._historyDetailsOverride = prev;
        }
      };
      Promise.all([load(attackerIds), attackerIds ? load(null) : null])
        .then(([details, unfiltered]) => {
          if (seq !== cutSeq || !active || !details) return;
          const base = hooks.targets?.()?.[0] || { targetId: record?.targetId };
          const target = { ...base, actorDamage: cut.actorDamage, totalDamage: cut.totalDamage, battleTime: cut.battleTime,
            worldBossObservedDamage: cut.worldBossObservedDamage };
          ownRender = true;
          const started = performance.now();
          try {
            hooks.renderFigures({ details, unfiltered: unfiltered || details, target, playing });
          } finally {
            observer.takeRecords();
            ownRender = false;
          }
          const took = performance.now() - started;
          figureCostMs = figureCostMs ? figureCostMs * 0.7 + took * 0.3 : took;
        })
        .catch((e) => window.javaBridge?.logToDebug?.(`Replay figures failed: ${e?.message || e}`));
    };

    const requestFigures = (now = performance.now(), force = false) => {
      const every = playing ? Math.max(FIGURE_INTERVAL_MS, figureCostMs * FIGURE_SHARE) : figureCostMs * 3;
      if (!force && now - lastFiguresTs < every) {
        figuresPending = true;
        return;
      }
      figuresPending = false;
      lastFiguresTs = now;
      drawFigures();
    };

    // While playing, the playhead and the progress knob step every
    // MOVER_STEP_MS from the timer below (the shade every tenth of a second),
    // with no transition. Long CSS
    // glides kept the GPU making a frame at every screen refresh for the
    // whole replay (a core's worth on the owner's PC, 2026-10-18); stepping
    // per frame made the page lay out and paint as often. A pause, a seek, a
    // speed change or a resize just draws the moment again.
    let playTimer = 0;
    let playFrom = 0;
    let playStart = 0;
    const playedAt = () => playFrom + (performance.now() - playStart) * speed;
    // The playhead and progress every step (MOVER_STEP_MS); the big shade
    // every FIGURE_INTERVAL_MS, its moves being the costly ones.
    let shadeAt = 0;
    const stepMovers = (playingStep = false) => {
      const dur = durationMs();
      if (dur <= 0) return;
      const now = performance.now();
      const withShade = !playingStep || now - shadeAt >= FIGURE_INTERVAL_MS;
      if (withShade) shadeAt = now;
      paintTimeline(withShade);
      const share = Math.min(1, at / dur);
      if (!progWidth) progWidth = scrubWrap.clientWidth;
      progFill.style.transform = `scaleX(${share.toFixed(4)})`;
      progKnob.style.transform = `translateX(${(share * Math.max(0, progWidth - 9)).toFixed(1)}px)`;
    };
    // Play, a speed change or a resize: the movers at the moment, at once.
    const glide = () => {
      if (playing) stepMovers(false);
    };
    const stopGlide = () => paintTimeline();
    const playTick = () => {
      if (!active || !playing) return;
      const now = performance.now();
      at = playedAt();
      const dur = durationMs();
      if (at >= dur) {
        // The end: back to the whole fight.
        exit();
        return;
      }
      stepMovers(true);
      hooks.advancePartyBars?.();
      requestFigures(now);
      paintClock(at, dur);
    };

    const tick = (now) => {
      frame = 0;
      if (!active || playing) return;
      if (figuresPending) requestFigures(now);
      paintControls();
      paintTimeline();
      if (figuresPending) frame = requestAnimationFrame(tick);
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(tick);
    };

    const enter = () => {
      if (active) return true;
      record = hooks.record?.() || null;
      if (!record) return false;
      if (!model || model.record !== record) model = buildReplayModel(record);
      if (!model.damageTimed || durationMs() <= 0) return false;
      active = true;
      at = 0;
      geometry = null;
      progWidth = 0;
      panel.classList.add("isReplaying");
      bar.classList.add("isActive");
      syncHealNote();
      return true;
    };

    const exit = () => {
      const was = active;
      active = false;
      playing = false;
      cutSeq++;
      if (frame) cancelAnimationFrame(frame);
      frame = 0;
      clearInterval(playTimer);
      playTimer = 0;
      figuresPending = false;
      panel.classList.remove("isReplaying");
      bar.classList.remove("isActive");
      syncPlayButton();
      panel.classList.remove("isReplayPlaying");
      paintTimeline();
      paintControls();
      if (was && hooks.isOpen?.()) {
        ownRender = true;
        try { hooks.renderFigures(null); } finally { observer.takeRecords(); ownRender = false; }
      }
    };

    const play = () => {
      if (!active) {
        if (!enter()) return;
      } else if (at >= durationMs()) {
        at = 0;
      }
      playing = true;
      playFrom = at;
      playStart = performance.now();
      syncPlayButton();
      requestFigures(playStart, true);
      glide();
      clearInterval(playTimer);
      playTimer = setInterval(playTick, MOVER_STEP_MS);
    };
    const pause = () => {
      if (playing) at = Math.min(durationMs(), playedAt());
      playing = false;
      clearInterval(playTimer);
      playTimer = 0;
      syncPlayButton();
      stopGlide();
      requestFigures(performance.now(), true);
      schedule();
    };

    const seek = (ms) => {
      if (!active && !enter()) return;
      at = Math.max(0, Math.min(durationMs(), Number(ms) || 0));
      if (playing) {
        // Arrow keys while playing: carry on from the new moment.
        playFrom = at;
        playStart = performance.now();
        requestFigures(playStart, true);
        paintClock(at, durationMs());
        glide();
        return;
      }
      figuresPending = true;
      schedule();
    };

    // ── Input ──
    playBtn.addEventListener("click", () => (playing ? pause() : play()));
    exitBtn.addEventListener("click", () => {
      exit();
      playBtn.focus();
    });
    speedSel.addEventListener("change", () => {
      const next = Number(speedSel.value);
      if (!SPEEDS.includes(next)) return;
      if (playing) {
        at = playedAt();
        playFrom = at;
        playStart = performance.now();
      }
      speed = next;
      try { localStorage.setItem(SPEED_KEY, String(next)); } catch { /* not kept */ }
      if (playing) glide();
    });
    // Dragging holds playback and lets go where it was.
    // While dragging, only the playhead, the shade and the clock follow, once
    // a frame; the figures are drawn exactly when the drag stops (let go, or
    // a moment without moving). Every drag step redrew everything, the steps
    // queued up behind each other and the page lagged (2026-10-18).
    let resumeAfterDrag = false;
    let dragFrame = 0;
    let settleTimer = 0;
    const dragPaint = () => {
      dragFrame = 0;
      const dur = durationMs();
      paintTimeline();
      paintClock(at, dur);
      scrub.style.setProperty("--range-pct", `${dur > 0 ? ((at / dur) * 100).toFixed(2) : 100}%`);
    };
    const settle = () => {
      clearTimeout(settleTimer);
      settleTimer = 0;
      if (!active) return;
      figuresPending = false;
      lastFiguresTs = performance.now();
      drawFigures(true);
    };
    scrub.addEventListener("pointerdown", () => {
      dragging = true;
      resumeAfterDrag = playing;
      if (playing) pause();
    });
    const endDrag = () => {
      if (!dragging) return;
      dragging = false;
      if (dragFrame) {
        cancelAnimationFrame(dragFrame);
        dragPaint();
      }
      settle();
      if (resumeAfterDrag) {
        resumeAfterDrag = false;
        play();
      }
    };
    scrub.addEventListener("pointerup", endDrag);
    scrub.addEventListener("pointercancel", endDrag);
    // Let go outside the slider: the drag ends all the same.
    window.addEventListener("pointerup", endDrag);
    scrub.addEventListener("input", () => {
      if (!dragging) {
        seek(scrub.value);
        return;
      }
      if (!active && !enter()) return;
      at = Math.max(0, Math.min(durationMs(), Number(scrub.value) || 0));
      if (!dragFrame) dragFrame = requestAnimationFrame(dragPaint);
      clearTimeout(settleTimer);
      // Held still for a while: the figures, exactly.
      settleTimer = setTimeout(settle, 400);
    });
    // Arrows a second (Shift: ten), Page keys thirty, Home / End the ends;
    // Space plays or pauses.
    scrub.addEventListener("keydown", (event) => {
      const dur = durationMs();
      const step = { ArrowLeft: -1000, ArrowDown: -1000, ArrowRight: 1000, ArrowUp: 1000, PageDown: -30000, PageUp: 30000 }[event.key];
      if (step !== undefined) {
        event.preventDefault();
        seek((active ? at : dur) + step * (event.shiftKey && Math.abs(step) === 1000 ? 10 : 1));
      } else if (event.key === "Home") {
        event.preventDefault();
        seek(0);
      } else if (event.key === "End") {
        event.preventDefault();
        seek(dur);
      } else if (event.key === " " || event.key === "Spacebar") {
        event.preventDefault();
        if (playing) pause();
        else play();
      } else if (event.key === "Escape" && active) {
        event.preventDefault();
        event.stopPropagation();
        exit();
      }
    });
    bar.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && active && event.target !== scrub) {
        event.preventDefault();
        event.stopPropagation();
        exit();
        playBtn.focus();
      }
    });

    // ── Keeping in step with the window ──
    const syncHealNote = () => {
      bar.classList.toggle("noHealTiming", !!model && !model.healTimed && panel.classList.contains("isHealMode"));
      const observed = Number(record?.worldBossObservedDamage);
      othersNote.style.display = Number.isFinite(observed) && observed > 0 && !panel.classList.contains("isHealMode") ? "" : "none";
    };

    // Shown for a saved fight that has hit times; a new fight or a closed
    // panel ends a replay.
    const syncAvailability = () => {
      const open = !!hooks.isOpen?.();
      const next = open ? hooks.record?.() || null : null;
      if (next !== record || !open) {
        if (active) exit();
        record = next;
        model = null;
      }
      const timed = !!record && (Array.isArray(record.details?.skills) ? record.details.skills : [])
        .some((s) => Array.isArray(s?.hitTimestamps) && s.hitTimestamps.length > 0);
      bar.hidden = !record;
      bar.classList.toggle("isUnavailable", !!record && !timed);
      playBtn.disabled = !!record && !timed;
      scrub.disabled = !!record && !timed;
      if (record && !timed) {
        playBtn.title = t("details.replay.unavailable", "This fight was saved without hit times, so it cannot be replayed");
      } else {
        syncPlayButton();
      }
      paintControls();
    };

    // A full redraw of the figures (a player picked, DMG / HEAL, a sort, the
    // peak switch) is drawn again from the cut while replaying.
    const observer = new MutationObserver(() => {
      if (ownRender) return;
      syncAvailability();
      syncHealNote();
      if (active && dragging) {
        figuresPending = true;
      } else if (active) {
        figuresPending = false;
        lastFiguresTs = performance.now();
        drawFigures();
      }
    });
    const watched = hooks.figureEls?.() || [];
    for (const el of watched) if (el) observer.observe(el, { childList: true, subtree: true, characterData: true });
    new MutationObserver(() => {
      syncAvailability();
      syncHealNote();
    }).observe(panel, { attributes: true, attributeFilter: ["class"] });

    if (typeof ResizeObserver === "function" && scroll) {
      new ResizeObserver(() => {
        geometry = null;
        progWidth = 0;
        if (!active) return;
        if (playing) {
          at = playedAt();
          playFrom = at;
          playStart = performance.now();
          glide();
        } else {
          paintTimeline();
        }
      }).observe(scroll);
    }
    window.i18n?.onChange?.(() => {
      syncPlayButton();
      syncAvailability();
    });

    syncPlayButton();
    syncAvailability();
  };

  window.fightReplay = { attach };
})();
