const createHistoryUI = ({ onOpenFight } = {}) => {
  const panel = document.querySelector(".historyPanel");
  if (!panel) return null;

  const listEl = panel.querySelector(".historyList");
  const closeBtn = panel.querySelector(".historyClose");
  const emptyEl = panel.querySelector(".historyEmpty");
  const trainToggleBtn = panel.querySelector(".historyTrainToggle");
  const deleteToggleBtn = panel.querySelector(".historyDeleteToggle");
  const viewToggleEl = panel.querySelector(".historyViewToggle");
  const viewBtns = viewToggleEl ? [...viewToggleEl.querySelectorAll(".historyViewBtn")] : [];
  const searchEl = panel.querySelector(".historyFilterSearch");
  const characterEl = panel.querySelector(".historyFilterCharacter");
  const difficultyEl = panel.querySelector(".historyFilterDifficulty");
  const periodEl = panel.querySelector(".historyFilterPeriod");
  const sortEl = panel.querySelector(".historyFilterSort");
  const killsOnlyEl = panel.querySelector(".historyKillsOnlyBox");
  const bestsSortEl = panel.querySelector(".historyBestsSort");
  const tabBtns = [...panel.querySelectorAll(".historyTab")];
  const filterPlayerEl = panel.querySelector(".historyFilterPlayer");
  const filterPlayerTrigger = filterPlayerEl?.querySelector(".historyClassDropdownTrigger");
  const filterPlayerLabel = filterPlayerEl?.querySelector(".historyClassDropdownLabel");
  const filterPlayerMenu = filterPlayerEl?.querySelector(".historyClassDropdownMenu");

  // Map from the Korean class name stored in fight records → stable enum key used for i18n
  const JOB_KEY_MAP = {
    "검성": "GLADIATOR",
    "수호성": "TEMPLAR",
    "궁성": "RANGER",
    "살성": "ASSASSIN",
    "마도성": "SORCERER",
    "치유성": "CLERIC",
    "정령성": "ELEMENTALIST",
    "호법성": "CHANTER",
    "권성": "FIGHTER",
  };

  // Class-filter icons that exist as assets (Korean class names). Guarding on
  // this set avoids requesting a missing file on every render (which spammed
  // "asset not found" for classes without an icon).
  const CLASS_ICON_JOBS = new Set(["검성", "궁성", "마도성", "살성", "수호성", "정령성", "치유성", "호법성", "권성"]);

  let showDeleteMode = false;
  let filterPlayer = "";
  let classDropdownOpen = false;
  // Search (a boss or dungeon's name), your character ("name|server"), a
  // difficulty's label, a period in days (0: all), the order, and wipes hidden.
  let search = "";
  let character = null; // null: not chosen yet (your latest character)
  let difficulty = "";
  let periodDays = 0;
  let sortBy = "newest";
  let killsOnly = false;
  // Personal bests' dungeons: "recent" (most recent record first) or "tier".
  const BESTS_SORT_KEY = "historyBestsSort";
  let bestsSort = (() => {
    try { return localStorage.getItem(BESTS_SORT_KEY) === "tier" ? "tier" : "recent"; } catch { return "recent"; }
  })();

  // "fights" or "bests" (personal bests).
  const TAB_KEY = "historyTab";
  let tab = (() => {
    try { return localStorage.getItem(TAB_KEY) === "bests" ? "bests" : "fights"; } catch { return "fights"; }
  })();
  // Personal bests (get_personal_bests), and the fights that hold one.
  let bests = [];
  let bestIds = new Set();

  // View mode: "dungeon" (each dungeon and difficulty a collapsible section,
  // the default), "grouped" (each boss a section) or "list" (flat, newest first).
  const VIEW_KEY = "historyViewMode";
  const VIEWS = ["dungeon", "grouped", "list"];
  let viewMode = (() => {
    try {
      const saved = localStorage.getItem(VIEW_KEY);
      return VIEWS.includes(saved) ? saved : "dungeon";
    } catch { return "dungeon"; }
  })();
  const expandedGroups = new Set();

  const syncDeleteToggle = () => {
    if (!deleteToggleBtn) return;
    deleteToggleBtn.classList.toggle("active", showDeleteMode);
    panel.classList.toggle("deleteMode", showDeleteMode);
  };

  const VIEW_TITLES = {
    dungeon: ["history.viewDungeon", "Group by dungeon"],
    grouped: ["history.viewGrouped", "Group by boss"],
    list: ["history.viewList", "List view"],
  };
  const syncViewToggle = () => {
    viewBtns.forEach((b) => {
      b.classList.toggle("active", b.dataset.view === viewMode);
      const [key, fallback] = VIEW_TITLES[b.dataset.view] || VIEW_TITLES.list;
      b.title = t(key, fallback);
    });
    panel.classList.toggle("groupedView", viewMode !== "list");
  };

  const i18n = window.i18n;
  const t = (key, fallback) => i18n?.t?.(key, fallback) ?? fallback;

  const STORAGE_KEY = "historyShowTraining";
  let showTraining = (() => {
    try { return localStorage.getItem(STORAGE_KEY) !== "0"; } catch { return true; }
  })();

  const syncTrainToggle = () => {
    if (!trainToggleBtn) return;
    trainToggleBtn.classList.toggle("active", showTraining);
    trainToggleBtn.title = t(
      showTraining ? "history.hideTrainingBattles" : "history.showTrainingBattles",
      showTraining ? "Hide Training Battles" : "Show Training Battles"
    );
  };

  const formatTime = (ms) => {
    const totalMs = Number(ms);
    if (!Number.isFinite(totalMs) || totalMs <= 0) return "00:00";
    const totalSeconds = Math.floor(totalMs / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  };

  const formatDate = (ms) => {
    const d = new Date(Number(ms));
    if (isNaN(d.getTime())) return "-";
    const pad = (n) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };

  const formatDamage = (v) => {
    const n = Number(v);
    if (!Number.isFinite(n)) return "-";
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}m`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
    return `${Math.round(n)}`;
  };

  const getJobLabel = (job) => {
    const key = JOB_KEY_MAP[job];
    if (!key) return job;
    return t(`classes.${key}`, key);
  };

  const setClassDropdownOpen = (open) => {
    classDropdownOpen = open;
    filterPlayerEl?.classList.toggle("open", open);
  };

  const setClassFilter = (job) => {
    filterPlayer = job;
    if (filterPlayerLabel) {
      if (job) {
        const img = CLASS_ICON_JOBS.has(job)
          ? `<img src="./assets/${job}.png" alt="" class="historyClassDropdownIcon" onerror="this.style.display='none'">`
          : "";
        filterPlayerLabel.innerHTML = `${img}${getJobLabel(job)}`;
      } else {
        filterPlayerLabel.textContent = t("history.filterPlayer", "All classes");
      }
    }
    filterPlayerMenu?.querySelectorAll(".historyClassOption").forEach((opt) => {
      opt.classList.toggle("selected", opt.dataset.job === job);
    });
    setClassDropdownOpen(false);
    renderList(allFights);
  };

  let allFights = [];
  const PAGE_SIZE = 30;
  let renderedCount = 0;
  let lastVisible = [];
  let loadingMore = false;

  // A fight's character, as the filter names it: "name|server".
  const charKey = (name, server) => `${name}|${Number(server) || 0}`;
  // Whether a fight or best is the chosen character's. One saved without
  // your server (before #128's fix) goes with the character of its name.
  const isCharacter = (name, server) => {
    if (!character) return true;
    const [n, sv] = character.split("|");
    return name === n && (!Number(server) || Number(server) === Number(sv));
  };
  // The name, and its server's number only where two of your characters
  // share the name.
  const charLabel = (key, keys) => {
    const [name, server] = key.split("|");
    const twin = keys.some((k) => k !== key && k.split("|")[0] === name);
    return twin && Number(server) ? `${name} (${server})` : name;
  };
  // A fight's difficulty: its boss's level (Nightmare) or its dungeon's tier.
  const tierOf = (f) => {
    const npcLevel = window.i18n?.getNpcLevel?.(Number(f.mobCode)) || "";
    if (npcLevel) return { key: "level", label: npcLevel };
    return Number(f.dungeonId) ? window.i18n?.getDungeonDifficulty?.(Number(f.dungeonId)) : null;
  };
  const option = (value, label, selected) => Object.assign(document.createElement("option"), { value, textContent: label, selected });

  // Your characters: names that were surely you somewhere (a dungeon, or a
  // fight that recorded you). An open-world fight's name that isn't one of
  // them is a stranger's fight the meter saw, credited to no one.
  let yourNames = new Set();
  const youIn = (f) => (f.myName && (f.myCertain || yourNames.has(f.myName)) ? f.myName : "");

  const populateDropdowns = (fights) => {
    if (!filterPlayerEl) return;
    yourNames = new Set([...fights.filter((f) => f.myCertain && f.myName).map((f) => f.myName), ...bests.map((e) => e.character)]);
    // Your characters, the latest first; your latest picked to start with.
    if (characterEl) {
      const seen = [];
      const add = (name, server) => {
        if (!name) return;
        // No server: the name's character if there is one, listed once.
        if (!Number(server) && seen.some((k) => k.split("|")[0] === name)) return;
        const key = charKey(name, server);
        if (seen.includes(key)) return;
        const bare = seen.indexOf(charKey(name, 0));
        if (bare >= 0) seen[bare] = key;
        else seen.push(key);
      };
      for (const f of [...fights].sort((a, b) => startMs(b) - startMs(a))) add(youIn(f), f.myServer);
      for (const e of bests) add(e.character, e.server);
      if (character === null) character = seen[0] || "";
      characterEl.replaceChildren(
        option("", t("history.allCharacters", "All characters"), character === ""),
        ...seen.map((key) => option(key, charLabel(key, seen), key === character)),
      );
    }
    if (difficultyEl) {
      const tiers = new Set();
      for (const f of fights) {
        const tier = tierOf(f);
        if (tier) tiers.add(tier.label);
      }
      difficultyEl.replaceChildren(
        option("", t("history.allDifficulties", "All difficulties"), difficulty === ""),
        ...[...tiers].sort().map((label) => option(label, label, label === difficulty)),
      );
    }
    if (periodEl) {
      periodEl.replaceChildren(...[
        [0, t("history.periodAll", "All time")],
        [1, t("history.periodToday", "Last 24 hours")],
        [7, t("history.period7", "Last 7 days")],
        [30, t("history.period30", "Last 30 days")],
      ].map(([v, label]) => option(String(v), label, v === periodDays)));
    }
    if (bestsSortEl) {
      bestsSortEl.replaceChildren(
        option("recent", t("history.sortRecent", "Most recent"), bestsSort === "recent"),
        option("tier", t("history.sortTier", "Dungeon tier"), bestsSort === "tier"),
      );
    }
    if (sortEl) {
      sortEl.replaceChildren(...[
        ["newest", t("history.sortNewest", "Newest")],
        ["fastest", t("history.sortFastest", "Fastest kill")],
        ["partyDps", t("history.recordedDps", "Recorded DPS")],
        ["myDps", t("history.sortMyDps", "Your DPS")],
      ].map(([v, label]) => option(v, label, v === sortBy)));
    }

    // Custom class dropdown
    if (filterPlayerMenu) {
      filterPlayerMenu.innerHTML = "";
      const allOpt = document.createElement("div");
      allOpt.className = "historyClassOption" + (filterPlayer === "" ? " selected" : "");
      allOpt.dataset.job = "";
      allOpt.textContent = t("history.filterPlayer", "All classes");
      allOpt.addEventListener("click", () => setClassFilter(""));
      filterPlayerMenu.appendChild(allOpt);

      const jobs = [...new Set(fights.flatMap((f) => Array.isArray(f.jobs) ? f.jobs : []).filter(Boolean))].sort(
        (a, b) => getJobLabel(a).localeCompare(getJobLabel(b))
      );
      jobs.forEach((job) => {
        const opt = document.createElement("div");
        opt.className = "historyClassOption" + (job === filterPlayer ? " selected" : "");
        opt.dataset.job = job;
        if (CLASS_ICON_JOBS.has(job)) {
          const img = document.createElement("img");
          img.src = `./assets/${job}.png`;
          img.alt = "";
          img.className = "historyClassDropdownIcon";
          img.onerror = () => { img.style.display = "none"; };
          opt.appendChild(img);
        }
        opt.appendChild(document.createTextNode(getJobLabel(job)));
        opt.addEventListener("click", () => setClassFilter(job));
        filterPlayerMenu.appendChild(opt);
      });

      // Sync label
      if (!filterPlayer) {
        if (filterPlayerLabel) filterPlayerLabel.textContent = t("history.filterPlayer", "All classes");
      }
    }
  };

  // Damage per second over the fight: all recorded damage, and yours.
  const partyDps = (f) => (Number(f.durationMs) > 0 ? (Number(f.totalDamage) || 0) * 1000 / Number(f.durationMs) : 0);
  const myDps = (f) => (Number(f.durationMs) > 0 ? (Number(f.myDamage) || 0) * 1000 / Number(f.durationMs) : 0);
  // What the search matches: the boss and its dungeon, in the shown language.
  const searchText = (f) => `${f.bossName || ""} ${dungeonTitle(f.dungeonId, f.mobCode)}`.toLowerCase();

  const applyFilters = (fights) => {
    const since = periodDays ? Date.now() - periodDays * 86_400_000 : 0;
    const needle = search.trim().toLowerCase();
    const shown = fights.filter((f) => {
      if (f.isTrain && !showTraining) return false;
      if (filterPlayer) {
        const jobs = Array.isArray(f.jobs) ? f.jobs : [];
        if (!jobs.includes(filterPlayer)) return false;
      }
      if (character && youIn(f) && !isCharacter(f.myName, f.myServer)) return false;
      if (difficulty && tierOf(f)?.label !== difficulty) return false;
      if (since && startMs(f) < since) return false;
      if (killsOnly && (f.outcome === "wipe" || !Number(f.dungeonId))) return false;
      if (needle && !searchText(f).includes(needle)) return false;
      return true;
    });
    // Fastest: kills first, the quickest of them; the rest after, newest first.
    const killed = (f) => f.outcome !== "wipe" && Number(f.dungeonId) > 0;
    const order = {
      newest: (a, b) => startMs(b) - startMs(a),
      fastest: (a, b) => (killed(b) - killed(a)) || (killed(a) ? Number(a.durationMs) - Number(b.durationMs) : startMs(b) - startMs(a)),
      partyDps: (a, b) => partyDps(b) - partyDps(a),
      myDps: (a, b) => myDps(b) - myDps(a),
    }[sortBy] || ((a, b) => startMs(b) - startMs(a));
    return shown.sort(order);
  };

  // Status line for the dry run. Created lazily so the panel markup does not
  // have to carry an element that is empty almost always.
  // `link`: a site URL in the text, made clickable (opens in the browser).
  const showNote = (text, isError, link) => {
    const panel = document.querySelector(".historyPanel");
    if (!panel) return;
    let note = panel.querySelector(".historyPreviewNote");
    if (!note) {
      note = document.createElement("div");
      note.className = "historyPreviewNote";
      const filters = panel.querySelector(".historyFilters");
      if (filters && filters.parentNode) {
        filters.parentNode.insertBefore(note, filters.nextSibling);
      } else {
        panel.appendChild(note);
      }
    }
    note.textContent = text;
    const at = link && isSiteUrl(link) ? text.indexOf(link) : -1;
    if (at >= 0) {
      const a = document.createElement("a");
      a.href = link;
      a.textContent = link;
      a.className = "historyNoteLink";
      a.addEventListener("click", (event) => {
        event.preventDefault();
        window.javaBridge?.openBrowser?.(link);
      });
      note.replaceChildren(text.slice(0, at), a, text.slice(at + link.length));
    }
    note.classList.toggle("isError", !!isError);
    note.style.display = text ? "block" : "none";
  };

  // Which fights have packets behind them, and which already have a link.
  // Loaded when the panel opens; a fight missing from it simply has no slice.
  let shareStatus = {};

  const UPLOAD_ICON = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" width="15" height="15"><path d="M12 13v8"/><path d="M4 14.9A7 7 0 1 1 15.7 8h1.8a4.5 4.5 0 0 1 2.5 8.2"/><path d="m8 17 4-4 4 4"/></svg>`;
  const LINK_ICON = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" width="15" height="15"><path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/></svg>`;

  const paintUploadBtn = (btn, fight) => {
    const url = shareStatus[fight.id]?.url;
    btn.classList.toggle("isUploaded", !!url);
    btn.innerHTML = url ? LINK_ICON : UPLOAD_ICON;
    const label = url
      ? t("history.uploadOpen", "Open this fight's log on a2tools.app")
      : t("history.uploadTip", "Upload this fight to a2tools.app and get a link to share");
    btn.title = label;
    btn.setAttribute("aria-label", label);
  };

  // The link comes from the server: open only https on a2tools.app or a subdomain.
  const isSiteUrl = (value) => {
    try {
      const u = new URL(value);
      const host = u.hostname.toLowerCase();
      return u.protocol === "https:" && !u.username && !u.password && !u.port &&
        (host === "a2tools.app" || host.endsWith(".a2tools.app"));
    } catch {
      return false;
    }
  };

  const runUpload = async (fight, btn) => {
    if (btn.disabled) return;
    const existing = shareStatus[fight.id]?.url;
    if (existing) {
      if (isSiteUrl(existing)) window.javaBridge?.openBrowser?.(existing);
      return;
    }
    btn.disabled = true;
    showNote(t("history.uploadWorking", "Uploading..."), false);
    try {
      const r = await window.javaBridge?.uploadFight?.(fight.id);
      if (!r?.url) throw new Error("no result");
      shareStatus[fight.id] = { hasSlice: true, url: r.url };
      paintUploadBtn(btn, fight);
      try { await navigator.clipboard?.writeText?.(r.url); } catch {}
      const isPrivate = r.visibility === "private";
      showNote(
        window.i18n?.format?.(
          isPrivate ? "history.uploadDonePrivate" : "history.uploadDonePublic",
          { url: r.url },
          isPrivate
            ? `Uploaded as private. Link copied: ${r.url}`
            : `Uploaded. Link copied: ${r.url}`
        ),
        false,
        r.url
      );
    } catch (err) {
      const msg = typeof err === "string" ? err : err?.message || String(err);
      showNote(msg, true);
    } finally {
      btn.disabled = false;
    }
  };

  const runPreview = async (fight, btn) => {
    if (btn.disabled) return;
    btn.disabled = true;
    showNote(t("history.previewWorking", "Building the preview..."), false);
    try {
      const r = await window.javaBridge?.previewShare?.(fight.id);
      if (!r) throw new Error("no result");
      const kb = Math.max(1, Math.round(r.sliceBytes / 1024));
      // What an upload would actually put on the wire, which is the number
      // people care about — not the size on disk.
      const sentKb = Math.max(1, Math.round((r.sliceCompressedBytes ?? r.sliceBytes) / 1024));
      showNote(
        window.i18n?.format?.(
          "history.previewDone",
          {
            slice: kb,
            sent: sentKb,
            kept: r.packetsKept,
            seen: r.packetsSeen,
            blinded: r.namesBlinded,
            dir: r.outDir,
          },
          `Wrote ${kb} KB (${sentKb} KB compressed — what an upload would send). Kept ${r.packetsKept} of ${r.packetsSeen} packets, blinded ${r.namesBlinded} names. Nothing was uploaded. Saved to ${r.outDir}`
        ),
        false
      );
      // Open the folder so the files are one click away rather than a path to
      // copy out of a status line.
      window.javaBridge?.openBrowser?.(r.outDir);
    } catch (err) {
      const msg = typeof err === "string" ? err : err?.message || String(err);
      showNote(msg, true);
    } finally {
      btn.disabled = false;
    }
  };

  // `grouped`: a row inside a boss section, which leads with its date (the
  // boss is the header). `child`: any row inside a section, styled as one; a
  // dungeon section's rows still lead with their boss.
  const buildRow = (fight, { grouped = false, child = grouped } = {}) => {
    const row = document.createElement("div");
    row.className = child ? "historyRow historyRowChild" : "historyRow";
    row.dataset.fightId = fight.id;

    const infoEl = document.createElement("div");
    infoEl.className = "historyRowInfo";

    const nameEl = document.createElement("div");
    nameEl.className = "historyRowName";
    // In grouped view the boss name is the section header, so the row leads with its date instead.
    nameEl.textContent = grouped
      ? formatDate(fight.startTimeMs)
      : (fight.bossName || `Boss #${fight.targetId}`);
    if (fight.isLive) {
      const lastActivityMs = Number(fight.startTimeMs) + Number(fight.durationMs);
      if (Date.now() - lastActivityMs < 60_000) {
        const badge = document.createElement("span");
        badge.className = "historyLiveBadge";
        badge.textContent = t("history.liveBadge", "Live");
        nameEl.appendChild(badge);
      }
    }
    // Every row names its dungeon tier, so a Hard clear stands out in any view.
    // Where one instance has bosses of several levels (Nightmare: Gatekeeper
    // Pinopi at Level 1-10), the boss's own level, from its NPC code.
    const npcLevel = window.i18n?.getNpcLevel?.(Number(fight.mobCode)) || "";
    const tier = npcLevel
      ? { key: "level", label: npcLevel }
      : fight.dungeonId
        ? window.i18n?.getDungeonDifficulty?.(Number(fight.dungeonId))
        : null;
    if (tier) {
      const badge = document.createElement("span");
      badge.className = `difficultyBadge difficulty-${tier.key}`;
      badge.textContent = tier.label;
      nameEl.appendChild(badge);
    }
    if (fight.isTrain) {
      const badge = document.createElement("span");
      badge.className = "historyTrainBadge";
      badge.textContent = t("history.trainBadge", "Training");
      nameEl.appendChild(badge);
    }
    // In a dungeon: a wipe, and a fight holding one of your bests.
    if (Number(fight.dungeonId) > 0 && fight.outcome === "wipe") {
      const badge = document.createElement("span");
      badge.className = "historyWipeBadge";
      badge.textContent = t("history.wipe", "Wipe");
      nameEl.appendChild(badge);
    }
    if (bestIds.has(fight.id)) {
      const badge = document.createElement("span");
      badge.className = "historyPbBadge";
      badge.textContent = t("history.pb", "PB");
      badge.title = t("history.pbTip", "One of your personal bests");
      nameEl.appendChild(badge);
    }

    const metaEl = document.createElement("div");
    metaEl.className = "historyRowMeta";

    const timeEl = document.createElement("span");
    timeEl.className = "historyRowTime";
    timeEl.textContent = formatDate(fight.startTimeMs);

    const durEl = document.createElement("span");
    durEl.className = "historyRowDuration";
    durEl.textContent = formatTime(fight.durationMs);

    const dmgEl = document.createElement("span");
    dmgEl.className = "historyRowDamage";
    dmgEl.textContent = formatDamage(fight.totalDamage);

    if (!grouped) metaEl.appendChild(timeEl);
    metaEl.appendChild(durEl);
    // Recorded DPS, and yours with your share of the recorded damage; the
    // damage alone where the fight has no length yet.
    if (Number(fight.durationMs) > 0) {
      const stat = (cls, label, value) => {
        const el = document.createElement("span");
        el.className = `historyRowStat ${cls}`;
        const b = document.createElement("b");
        b.textContent = formatDamage(value);
        el.append(`${label} `, b);
        return el;
      };
      metaEl.appendChild(stat("historyRowParty", t("history.recordedDps", "Recorded DPS"), partyDps(fight)));
      if (Number(fight.myDamage) > 0 && youIn(fight)) {
        const mine = stat("historyRowMine", t("history.you", "You"), myDps(fight));
        const share = Number(fight.totalDamage) > 0 ? Math.round(Number(fight.myDamage) * 100 / Number(fight.totalDamage)) : 0;
        if (share > 0 && share < 100) {
          const shareEl = document.createElement("span");
          shareEl.className = "historyRowShare";
          shareEl.textContent = ` ${share}%`;
          mine.appendChild(shareEl);
        }
        metaEl.appendChild(mine);
      }
      dmgEl.title = t("history.totalDamage", "Total damage");
    } else {
      metaEl.appendChild(dmgEl);
    }

    const iconsEl = document.createElement("div");
    iconsEl.className = "historyRowIcons";
    // One icon per party member; older builds sent only the distinct classes.
    const memberJobs = Array.isArray(fight.memberJobs) && fight.memberJobs.length ? fight.memberJobs : null;
    const allJobs = (memberJobs || (Array.isArray(fight.jobs) ? fight.jobs : [])).slice(0, 12);
    allJobs.forEach((job) => {
      if (!job) return;
      const wrap = document.createElement("span");
      wrap.className = "historyIconWrap";
      wrap.setAttribute("data-tip", getJobLabel(job));
      const img = document.createElement("img");
      img.src = `./assets/${job}.png`;
      img.alt = job;
      img.className = "historyRowClassIcon";
      img.onerror = () => { wrap.style.display = "none"; };
      wrap.appendChild(img);
      iconsEl.appendChild(wrap);
    });

    infoEl.appendChild(nameEl);
    infoEl.appendChild(metaEl);

    const actionsEl = document.createElement("div");
    actionsEl.className = "historyRowActions";

    // Training dummies are not logs, and a fight still in progress has no end.
    // Only dungeon fights are logs: open-world bosses stay in your history
    // (share::is_instanced). The private build uploads nothing
    // (build_features); preview stays.
    if (!fight.isLive && !fight.isTrain && Number(fight.dungeonId) && window.a2Build?.online !== false) {
      const uploadBtn = document.createElement("button");
      uploadBtn.className = "historyPreviewBtn historyUploadBtn";
      uploadBtn.type = "button";
      paintUploadBtn(uploadBtn, fight);
      uploadBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        runUpload(fight, uploadBtn);
      });
      actionsEl.appendChild(uploadBtn);
    }

    if (!fight.isLive) {
      const previewBtn = document.createElement("button");
      previewBtn.className = "historyPreviewBtn";
      previewBtn.type = "button";
      previewBtn.setAttribute("aria-label", t("history.previewUpload", "Preview upload"));
      previewBtn.title = t(
        "history.previewUploadTip",
        "Write what sharing this fight would upload. Nothing is sent."
      );
      previewBtn.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" width="15" height="15"><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7z"/><circle cx="12" cy="12" r="3"/></svg>`;
      previewBtn.addEventListener("click", (e) => {
        // The row itself opens the fight; without this the details window
        // opens behind the preview.
        e.stopPropagation();
        runPreview(fight, previewBtn);
      });
      actionsEl.appendChild(previewBtn);
    }

    if (!fight.isLive) {
      const deleteBtn = document.createElement("button");
      deleteBtn.className = "historyDeleteBtn";
      deleteBtn.type = "button";
      deleteBtn.setAttribute("aria-label", t("history.delete", "Delete"));
      deleteBtn.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" width="15" height="15"><polyline points="3 6 5 6 21 6"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6M14 11v6"/><path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/></svg>`;
      deleteBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        if (window.javaBridge?.deleteFight?.(fight.id)) {
          allFights = allFights.filter((x) => x.id !== fight.id);
          if (viewMode !== "list") {
            // Re-render so the section's fight count updates and empty sections drop out.
            renderList(allFights);
          } else {
            row.remove();
            if (!listEl.querySelector(".historyRow")) {
              if (emptyEl) emptyEl.style.display = "";
            }
          }
        }
      });
      actionsEl.appendChild(deleteBtn);
    }

    row.appendChild(infoEl);
    row.appendChild(iconsEl);
    row.appendChild(actionsEl);

    row.addEventListener("click", async () => {
      const rawRecord = await window.javaBridge?.getFightDetails?.(fight.id);
      if (!rawRecord) return;
      let record;
      try {
        record = typeof rawRecord === "string" ? JSON.parse(rawRecord) : rawRecord;
      } catch {
        return;
      }
      onOpenFight?.(record);
    });

    return row;
  };

  const appendPage = () => {
    if (!listEl || renderedCount >= lastVisible.length) return;
    const end = Math.min(renderedCount + PAGE_SIZE, lastVisible.length);
    const frag = document.createDocumentFragment();
    for (let i = renderedCount; i < end; i++) {
      frag.appendChild(buildRow(lastVisible[i]));
    }
    listEl.appendChild(frag);
    renderedCount = end;
  };

  const fightCountLabel = (n) =>
    n === 1
      ? t("history.fightCountOne", "1 fight")
      : t("history.fightCount", "{n} fights").replace("{n}", n);

  const startMs = (f) => Number(f.startTimeMs) || 0;

  // "3 hours ago", in the meter's language.
  const ago = (ms) => {
    const lang = window.i18n?.getLanguage?.() || "en";
    let v = (Number(ms) - Date.now()) / 1000;
    const steps = [["second", 60], ["minute", 60], ["hour", 24], ["day", 30], ["month", 12], ["year", Infinity]];
    for (const [unit, size] of steps) {
      if (Math.abs(v) < size) {
        try {
          return new Intl.RelativeTimeFormat(lang, { numeric: "auto" }).format(Math.round(v), unit);
        } catch {
          return formatDate(ms);
        }
      }
      v /= size;
    }
    return formatDate(ms);
  };

  // A section at a glance: when it was last fought, its quickest fight and
  // its most damage. No quickest where fights are cut off as players come and
  // go (the open world, training): 00:05 there is no clear.
  const groupSummary = (fights, { best = true } = {}) => {
    // Kills only (a wipe at 00:12 is no clear), and in a dungeon its last
    // boss's where there is one: the clear players time.
    const kills = fights.filter((f) => !f.isLive && Number(f.durationMs) > 0 && f.outcome !== "wipe");
    const finals = kills.filter((f) => f.finalBoss);
    const done = finals.length ? finals : kills;
    const parts = [ago(Math.max(...fights.map(startMs)))];
    if (best && done.length) {
      const best = Math.min(...done.map((f) => Number(f.durationMs)));
      parts.push(t("history.best", "Best {time}").replace("{time}", formatTime(best)));
    }
    const top = Math.max(...fights.map((f) => Number(f.totalDamage) || 0));
    if (top > 0) parts.push(t("history.top", "Top {damage}").replace("{damage}", formatDamage(top)));
    return parts.join(" · ");
  };

  // Rows shown before a "Show all" in a section, or a boss inside one.
  const SHOWN_ROWS = 5;
  // A list of fights: the most recent few, then the rest behind a button.
  const rowsWithMore = (fights, rowOptions) => {
    const frag = document.createDocumentFragment();
    const list = document.createElement("div");
    const fill = (all) => {
      list.replaceChildren(...(all ? fights : fights.slice(0, SHOWN_ROWS)).map((f) => buildRow(f, rowOptions)));
    };
    fill(false);
    frag.appendChild(list);
    if (fights.length > SHOWN_ROWS) {
      const more = document.createElement("button");
      more.type = "button";
      more.className = "historyShowAll";
      let all = false;
      const label = () => {
        more.textContent = all
          ? t("history.showFewer", "Show fewer")
          : t("history.showAll", "Show all {n}").replace("{n}", fights.length);
        more.setAttribute("aria-expanded", String(all));
      };
      label();
      more.addEventListener("click", () => {
        all = !all;
        fill(all);
        label();
      });
      frag.appendChild(more);
    }
    return frag;
  };

  const buildGroup = (group) => {
    // Most recent run first within a section.
    group.fights.sort((a, b) => startMs(b) - startMs(a));

    const wrap = document.createElement("div");
    wrap.className = "historyGroup";

    // A button: reached with Tab, and it says whether it is open.
    const header = document.createElement("button");
    header.type = "button";
    header.className = "historyGroupHeader";

    const chevron = document.createElement("span");
    chevron.className = "historyGroupChevron";
    chevron.innerHTML = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 18 6-6-6-6"/></svg>`;

    const titleEl = document.createElement("span");
    titleEl.className = "historyGroupTitle";
    const nameEl = document.createElement("span");
    nameEl.className = "historyGroupName";
    nameEl.textContent = group.name;
    const summaryEl = document.createElement("span");
    summaryEl.className = "historyGroupSummary";
    summaryEl.textContent = groupSummary(group.fights, { best: group.byDungeon && !group.training && !group.openWorld });
    titleEl.append(nameEl, summaryEl);

    const countEl = document.createElement("span");
    countEl.className = "historyGroupCount";
    countEl.textContent = fightCountLabel(group.fights.length);

    header.append(chevron, titleEl, countEl);

    const childWrap = document.createElement("div");
    childWrap.className = "historyGroupChildren";

    const renderChildren = () => {
      if (childWrap.childElementCount) return;
      const frag = document.createDocumentFragment();
      // A dungeon's bosses, each a small section of its own, the most recent
      // first: its rows then lead with their date, not the boss's name again.
      const bosses = new Map();
      if (group.byDungeon) {
        group.fights.forEach((f) => {
          const key = f.bossName || `Boss #${f.targetId}`;
          if (!bosses.has(key)) bosses.set(key, []);
          bosses.get(key).push(f);
        });
      }
      if (bosses.size > 1) {
        // The dungeon's last boss leads: the one players come for. Not in a
        // raid or on Hard, where each boss stands on its own.
        const allEqual = group.fights.some((f) => f.raid || tierOf(f)?.key === "hard");
        const final = (fights) => fights.some((f) => f.finalBoss);
        const ordered = allEqual ? [...bosses] : [...bosses].sort(([, a], [, b]) => final(b) - final(a));
        ordered.forEach(([boss, fights]) => {
          // A boss fought once: a row with its name, no heading over it.
          if (fights.length === 1) {
            frag.appendChild(buildRow(fights[0], { child: true }));
            return;
          }
          const sub = document.createElement("div");
          sub.className = "historySubGroup";
          const subHead = document.createElement("div");
          subHead.className = "historySubGroupHeader";
          const subName = document.createElement("span");
          subName.className = "historySubGroupName";
          subName.textContent = boss;
          if (!allEqual && final(fights)) {
            const chip = document.createElement("span");
            chip.className = "historyFinalBadge";
            chip.textContent = t("history.finalBoss", "Final boss");
            subName.appendChild(chip);
          }
          const subMeta = document.createElement("span");
          subMeta.className = "historySubGroupMeta";
          subMeta.textContent = `${fightCountLabel(fights.length)} · ${groupSummary(fights)}`;
          subHead.append(subName, subMeta);
          sub.append(subHead, rowsWithMore(fights, { grouped: true }));
          frag.appendChild(sub);
        });
      } else {
        frag.appendChild(rowsWithMore(group.fights, bosses.size === 1 ? { grouped: true } : group.rowOptions));
      }
      childWrap.appendChild(frag);
    };

    const setOpen = (open) => {
      if (open) {
        expandedGroups.add(group.name);
        renderChildren();
      } else {
        expandedGroups.delete(group.name);
      }
      header.classList.toggle("expanded", open);
      header.setAttribute("aria-expanded", String(open));
      childWrap.style.display = open ? "" : "none";
    };
    setOpen(expandedGroups.has(group.name));
    header.addEventListener("click", () => {
      setOpen(!expandedGroups.has(group.name));
      syncExpandAll();
    });
    wrap.setOpen = setOpen;

    wrap.appendChild(header);
    wrap.appendChild(childWrap);
    return wrap;
  };

  // Expand all / Collapse all, above the sections.
  let groupEls = [];
  let expandAllBtn = null;
  const syncExpandAll = () => {
    if (!expandAllBtn) return;
    const anyClosed = groupEls.some((g) => !expandedGroups.has(g.groupName));
    expandAllBtn.textContent = anyClosed ? t("history.expandAll", "Expand all") : t("history.collapseAll", "Collapse all");
  };

  // A dungeon section's title: the dungeon and its difficulty, or Open world.
  // An instance the dungeon table doesn't name (Nightmare, 200003) takes the
  // category the NPC table gives its boss.
  const dungeonTitle = (dungeonId, mobCode) => {
    const id = Number(dungeonId) || 0;
    if (!id) return t("history.openWorld", "Open world");
    return window.i18n?.getDungeonLabel?.(id)
      || window.i18n?.getNpcCategory?.(Number(mobCode))
      || `#${id}`;
  };

  const renderGrouped = (visible, byDungeon) => {
    const groups = new Map();
    visible.forEach((f) => {
      // Keyed by what the header says, so two instance ids with the same name
      // and difficulty share one section.
      // Training dummies are a section of their own, not the open world's.
      const training = byDungeon && !!f.isTrain;
      const key = training
        ? t("history.training", "Training")
        : byDungeon ? dungeonTitle(f.dungeonId, f.mobCode) : (f.bossName || `Boss #${f.targetId}`);
      let g = groups.get(key);
      if (!g) {
        g = {
          name: key, fights: [], byDungeon, training,
          openWorld: byDungeon && !training && !(Number(f.dungeonId) > 0),
          rowOptions: byDungeon ? { child: true } : { grouped: true },
        };
        groups.set(key, g);
      }
      g.fights.push(f);
    });
    // Sections ordered by their most recent run; training last.
    const ordered = [...groups.values()].sort(
      (a, b) => (a.training - b.training) || (Math.max(...b.fights.map(startMs)) - Math.max(...a.fights.map(startMs)))
    );
    const frag = document.createDocumentFragment();
    const tools = document.createElement("div");
    tools.className = "historyGroupTools";
    expandAllBtn = document.createElement("button");
    expandAllBtn.type = "button";
    expandAllBtn.className = "historyExpandAll";
    expandAllBtn.addEventListener("click", () => {
      const open = groupEls.some((g) => !expandedGroups.has(g.groupName));
      groupEls.forEach((g) => g.setOpen(open));
      syncExpandAll();
    });
    tools.appendChild(expandAllBtn);
    frag.appendChild(tools);
    groupEls = ordered.map((g) => {
      const el = buildGroup(g);
      el.groupName = g.name;
      return el;
    });
    groupEls.forEach((el) => frag.appendChild(el));
    syncExpandAll();
    listEl.appendChild(frag);
  };

  // ---- Personal bests ----

  // An encounter's live counts: the fights still in the history, by outcome.
  const liveCounts = (e) => {
    const c = { kills: 0, wipes: 0, unconfirmed: 0 };
    for (const f of allFights) {
      if (f.isTrain || f.myName !== e.character || (Number(f.myServer) && Number(f.myServer) !== Number(e.server))
        || Number(f.dungeonId) !== Number(e.dungeonId) || Number(f.mobCode) !== Number(e.mobCode)) continue;
      c[f.outcome === "kill" ? "kills" : f.outcome === "wipe" ? "wipes" : "unconfirmed"] += 1;
    }
    return c;
  };

  // One row per boss, merged across characters where "All characters" is
  // chosen: each best then the best of any of them, with who set it.
  const bestRows = () => {
    const needle = search.trim().toLowerCase();
    const better = (a, b, lower) => {
      if (!a) return b;
      if (!b) return a;
      if (a.confirmed !== b.confirmed) return a.confirmed ? a : b;
      return (lower ? b.value < a.value : b.value > a.value) ? b : a;
    };
    const rows = new Map();
    for (const e of bests) {
      if (!isCharacter(e.character, e.server)) continue;
      const tier = tierOf(e);
      if (difficulty && tier?.label !== difficulty) continue;
      if (filterPlayer && e.myDps?.job !== filterPlayer && e.fastest?.job !== filterPlayer) continue;
      if (needle && !`${e.bossName} ${dungeonTitle(e.dungeonId, e.mobCode)}`.toLowerCase().includes(needle)) continue;
      const key = `${e.dungeonId}|${e.mobCode}`;
      const live = liveCounts(e);
      const counts = {
        kills: (e.archived?.kills || 0) + live.kills,
        wipes: (e.archived?.wipes || 0) + live.wipes,
        unconfirmed: (e.archived?.unconfirmed || 0) + live.unconfirmed,
      };
      const tag = (b) => b && { ...b, who: e.character };
      const row = rows.get(key);
      if (!row) {
        rows.set(key, { ...e, counts, fastest: tag(e.fastest), myDps: tag(e.myDps), partyDps: tag(e.partyDps) });
        continue;
      }
      row.fastest = better(row.fastest, tag(e.fastest), true);
      row.myDps = better(row.myDps, tag(e.myDps), false);
      row.partyDps = better(row.partyDps, tag(e.partyDps), false);
      for (const k of Object.keys(counts)) row.counts[k] += counts[k];
    }
    return [...rows.values()];
  };

  const openBest = async (best) => {
    const raw = best && await window.javaBridge?.getFightDetails?.(best.fightId);
    if (!raw) {
      showNote(t("history.fightGone", "That fight is no longer saved; its best is kept."), false);
      return;
    }
    try {
      onOpenFight?.(typeof raw === "string" ? JSON.parse(raw) : raw);
    } catch {}
  };

  // A best's cell: its figure (with "?" while the kill wasn't recorded), and
  // under it the date, or your class and character.
  const bestCell = (best, value, sub) => {
    const cell = document.createElement("button");
    cell.type = "button";
    cell.className = "historyBestCell";
    if (!best) {
      cell.disabled = true;
      cell.textContent = "–";
      return cell;
    }
    const big = document.createElement("span");
    big.className = "historyBestValue";
    big.textContent = value;
    if (!best.confirmed) {
      const q = document.createElement("span");
      q.className = "historyBestUnconfirmed";
      q.textContent = "?";
      big.appendChild(q);
      cell.title = t("history.unconfirmedTip", "The kill wasn't recorded (a fight from before 2.0.87): judged from the damage dealt.");
    } else {
      cell.title = t("history.openBest", "Open this fight");
    }
    const small = document.createElement("span");
    small.className = "historyBestSub";
    small.append(...sub);
    cell.append(big, small);
    cell.addEventListener("click", () => openBest(best));
    return cell;
  };

  const shortDate = (ms) => {
    try {
      return new Date(Number(ms)).toLocaleDateString(window.i18n?.getLanguage?.() || undefined, { day: "numeric", month: "short" });
    } catch {
      return formatDate(ms).slice(0, 10);
    }
  };
  const classIcon = (job) => {
    if (!CLASS_ICON_JOBS.has(job)) return [];
    const img = document.createElement("img");
    img.src = `./assets/${job}.png`;
    img.alt = getJobLabel(job);
    img.className = "historyBestClass";
    return [img];
  };

  const buildBestRow = (e) => {
    const row = document.createElement("div");
    row.className = "historyBestRow";
    const name = document.createElement("div");
    name.className = "historyBestName";
    const title = document.createElement("div");
    title.className = "historyRowName";
    title.textContent = e.bossName || `#${e.mobCode}`;
    const level = window.i18n?.getNpcLevel?.(Number(e.mobCode)) || "";
    if (level) {
      const badge = document.createElement("span");
      badge.className = "difficultyBadge difficulty-level";
      badge.textContent = level;
      title.appendChild(badge);
    }
    const counts = document.createElement("div");
    counts.className = "historyBestCounts";
    const count = (n, one, many) => (n === 1
      ? t(`history.${one}`, `1 ${one}`)
      : window.i18n?.format?.(`history.${many}`, { n }, `${n} ${many}`));
    const kills = e.counts.kills + e.counts.unconfirmed;
    const parts = [count(kills, "killOne", "kills")];
    if (e.counts.wipes) parts.push(count(e.counts.wipes, "wipeOne", "wipes"));
    counts.textContent = parts.join(" · ");
    name.append(title, counts);
    const who = (b) => (character === "" && b?.who ? [` ${b.who}`] : []);
    row.append(
      name,
      bestCell(e.fastest, e.fastest ? formatTime(e.fastest.value) : "", e.fastest ? [shortDate(e.fastest.atMs), ...who(e.fastest)] : []),
      bestCell(e.myDps, e.myDps ? formatDamage(e.myDps.value) : "", e.myDps ? [...classIcon(e.myDps.job), ...who(e.myDps)] : []),
      bestCell(e.partyDps, e.partyDps ? formatDamage(e.partyDps.value) : "", e.partyDps ? [shortDate(e.partyDps.atMs)] : []),
    );
    return row;
  };

  // A folded list under a button: "Other bosses (2)", "Other levels (5)".
  const foldedRows = (rows, key, fallback) => {
    const frag = document.createDocumentFragment();
    if (!rows.length) return frag;
    const more = document.createElement("button");
    more.type = "button";
    more.className = "historyShowAll";
    const box = document.createElement("div");
    box.hidden = true;
    rows.forEach((e) => box.appendChild(buildBestRow(e)));
    const label = () => {
      more.textContent = box.hidden
        ? window.i18n?.format?.(key, { n: rows.length }, fallback.replace("{n}", rows.length))
        : t("history.showFewer", "Show fewer");
      more.setAttribute("aria-expanded", String(!box.hidden));
    };
    label();
    more.addEventListener("click", () => {
      box.hidden = !box.hidden;
      label();
    });
    frag.append(more, box);
    return frag;
  };

  // A boss's level (Nightmare), as a number, 0 where it has none.
  const levelOf = (e) => Number(String(window.i18n?.getNpcLevel?.(Number(e.mobCode)) || "").replace(/\D+/g, "")) || 0;

  // Dungeon sections opened on the bests tab (closed to start with).
  const openBestSections = new Set();

  // Personal bests by dungeon, the dungeons you cleared most recently first,
  // each folded to a line saying its last boss's fastest kill. Inside, its
  // last boss leads and the bosses before it fold under "Other bosses",
  // except in a raid or on Hard, where every boss is listed. A boss with
  // levels (Nightmare) leads with its highest; the rest fold under it.
  const renderBests = () => {
    const rows = bestRows();
    if (!rows.length) {
      if (emptyEl) {
        emptyEl.textContent = t("history.noBests", "No dungeon kills yet.");
        emptyEl.style.display = "";
      }
      return;
    }
    if (emptyEl) emptyEl.style.display = "none";
    const head = document.createElement("div");
    head.className = "historyBestRow historyBestHead";
    for (const [key, fallback] of [["history.encounter", "Encounter"], ["history.fastestKill", "Fastest kill"],
      ["history.yourBest", "Your best DPS"], ["history.partyBest", "Party best"]]) {
      head.appendChild(Object.assign(document.createElement("span"), { textContent: t(key, fallback) }));
    }
    const dungeons = new Map();
    for (const e of rows) {
      const title = dungeonTitle(e.dungeonId, e.mobCode);
      if (!dungeons.has(title)) dungeons.set(title, []);
      dungeons.get(title).push(e);
    }
    const latest = (list) => Math.max(...list.flatMap((e) => [e.fastest?.atMs || 0, e.myDps?.atMs || 0]));
    // Dungeon tier: raids, then the game's tiers from 6 down (Hard before
    // Normal in each), then dungeons with none, Nightmare (a boss's levels,
    // no dungeon of the table's) last; the most recent first in each.
    const DIFFICULTY_ORDER = { hard: 0, normal: 1, exploration: 2 };
    const rank = (list) => {
      const e = list[0];
      if (e.raid) return [0, 0, 0];
      if (levelOf(e) && !window.i18n?.getDungeonName?.(Number(e.dungeonId))) return [3, 0, 0];
      const tier = Math.max(...list.map((x) => Number(x.dungeonTier) || 0));
      return tier ? [1, -tier, DIFFICULTY_ORDER[tierOf(e)?.key] ?? 3] : [2, 0, DIFFICULTY_ORDER[tierOf(e)?.key] ?? 3];
    };
    const byTier = ([, a], [, b]) => {
      const ra = rank(a);
      const rb = rank(b);
      return ra[0] - rb[0] || ra[1] - rb[1] || ra[2] - rb[2] || latest(b) - latest(a);
    };
    const frag = document.createDocumentFragment();
    frag.appendChild(head);
    const ordered = [...dungeons].sort(bestsSort === "tier" ? byTier : ([, a], [, b]) => latest(b) - latest(a));
    for (const [title, list] of ordered) {
      // Bosses with levels: the highest of each leads, the others fold.
      const byBoss = new Map();
      for (const e of list) {
        const key = levelOf(e) ? `name:${e.bossName}` : `mob:${e.mobCode}`;
        if (!byBoss.has(key)) byBoss.set(key, []);
        byBoss.get(key).push(e);
      }
      const tops = [];
      const lowerLevels = [];
      for (const group of byBoss.values()) {
        group.sort((a, b) => levelOf(b) - levelOf(a));
        tops.push(group[0]);
        lowerLevels.push(...group.slice(1));
      }
      const allEqual = tops.some((e) => e.raid || tierOf(e)?.key === "hard");
      const finals = allEqual ? [] : tops.filter((e) => e.finalBoss);
      const lead = finals.length ? finals : tops;
      const others = finals.length ? tops.filter((e) => !e.finalBoss) : [];

      const section = document.createElement("div");
      section.className = "historyGroup historyBestDungeon";
      const header = document.createElement("button");
      header.type = "button";
      header.className = "historyGroupHeader";
      const chevron = document.createElement("span");
      chevron.className = "historyGroupChevron";
      chevron.innerHTML = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 18 6-6-6-6"/></svg>`;
      const titleEl = document.createElement("span");
      titleEl.className = "historyGroupTitle";
      const nameEl = Object.assign(document.createElement("span"), { className: "historyGroupName", textContent: title });
      // Folded, the section says what matters: its lead bosses' fastest kills.
      const summaryEl = document.createElement("span");
      summaryEl.className = "historyGroupSummary";
      summaryEl.textContent = lead.slice(0, 3).map((e) => {
        const level = levelOf(e) ? ` ${window.i18n?.getNpcLevel?.(Number(e.mobCode))}` : "";
        return `${e.bossName}${level}${e.fastest ? ` ${formatTime(e.fastest.value)}${e.fastest.confirmed ? "" : "?"}` : ""}`;
      }).join(" · ") + (lead.length > 3 ? " …" : "");
      titleEl.append(nameEl, summaryEl);
      const kills = list.reduce((n, e) => n + e.counts.kills + e.counts.unconfirmed, 0);
      const countEl = Object.assign(document.createElement("span"), {
        className: "historyGroupCount",
        textContent: kills === 1 ? t("history.killOne", "1 kill") : window.i18n?.format?.("history.kills", { n: kills }, `${kills} kills`),
      });
      header.append(chevron, titleEl, countEl);

      const body = document.createElement("div");
      body.className = "historyGroupChildren";
      const fill = () => {
        if (body.childElementCount) return;
        lead.forEach((e) => body.appendChild(buildBestRow(e)));
        body.appendChild(foldedRows(others, "history.otherBosses", "Other bosses ({n})"));
        body.appendChild(foldedRows(lowerLevels, "history.otherLevels", "Other levels ({n})"));
      };
      const setOpen = (open) => {
        if (open) {
          openBestSections.add(title);
          fill();
        } else {
          openBestSections.delete(title);
        }
        header.classList.toggle("expanded", open);
        header.setAttribute("aria-expanded", String(open));
        body.style.display = open ? "" : "none";
      };
      setOpen(openBestSections.has(title));
      header.addEventListener("click", () => setOpen(!openBestSections.has(title)));
      section.append(header, body);
      frag.appendChild(section);
    }
    listEl.appendChild(frag);
  };

  const syncTabs = () => {
    tabBtns.forEach((b) => {
      const on = b.dataset.tab === tab;
      b.classList.toggle("active", on);
      b.setAttribute("aria-selected", String(on));
    });
    panel.classList.toggle("bestsTab", tab === "bests");
  };

  const renderList = (fights) => {
    if (!listEl) return;
    listEl.innerHTML = "";
    renderedCount = 0;
    if (emptyEl) emptyEl.textContent = t("history.empty", "No boss fights recorded yet.");
    if (tab === "bests") {
      renderBests();
      return;
    }

    lastVisible = applyFilters(fights);

    if (!lastVisible || lastVisible.length === 0) {
      if (emptyEl) emptyEl.style.display = "";
      return;
    }
    if (emptyEl) emptyEl.style.display = "none";

    if (viewMode === "list") {
      appendPage();
    } else {
      renderGrouped(lastVisible, viewMode === "dungeon");
    }
  };

  const open = () => {
    panel.classList.add("open");
    syncTrainToggle();
    syncViewToggle();
    const raw = window.javaBridge?.getFightHistory?.();
    try {
      allFights = typeof raw === "string" ? JSON.parse(raw) : (Array.isArray(raw) ? raw : []);
    } catch {
      allFights = [];
    }
    syncTabs();
    populateDropdowns(allFights);
    renderList(allFights);
    // Personal bests come a moment later (they keep the history's summaries
    // up to date first); the fights don't wait for them.
    Promise.resolve(window.javaBridge?.getPersonalBests?.()).then((list) => {
      bests = Array.isArray(list) ? list : [];
      bestIds = new Set(bests.flatMap((e) => [e.fastest, e.myDps, e.partyDps]).filter(Boolean).map((b) => b.fightId));
      populateDropdowns(allFights);
      renderList(allFights);
    }).catch(() => {});
    // Links for fights already uploaded arrive a moment later; the buttons
    // work without them, so the list does not wait.
    Promise.resolve(window.javaBridge?.shareStatus?.()).then((status) => {
      if (!status || typeof status !== "object") return;
      shareStatus = status;
      if (Object.values(status).some((s) => s?.url)) renderList(allFights);
    }).catch(() => {});
  };

  // A fight uploaded in the background (Settings -> upload automatically) gets
  // its "open log" button without reopening the panel.
  window.__TAURI__?.event?.listen?.("fight-uploaded", (event) => {
    const p = event?.payload || {};
    if (!p.fightId || !p.url) return;
    shareStatus[p.fightId] = { hasSlice: true, url: p.url };
    if (panel.classList.contains("open")) renderList(allFights);
  });

  // Load more rows when scrolled near the bottom (flat list only; grouped renders sections eagerly)
  listEl?.addEventListener("scroll", () => {
    if (viewMode !== "list") return;
    if (loadingMore || renderedCount >= lastVisible.length) return;
    const threshold = 80;
    if (listEl.scrollTop + listEl.clientHeight >= listEl.scrollHeight - threshold) {
      loadingMore = true;
      appendPage();
      loadingMore = false;
    }
  });

  let searchTimer = 0;
  searchEl?.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      search = searchEl.value;
      renderList(allFights);
    }, 150);
  });
  characterEl?.addEventListener("change", () => {
    character = characterEl.value;
    renderList(allFights);
  });
  difficultyEl?.addEventListener("change", () => {
    difficulty = difficultyEl.value;
    renderList(allFights);
  });
  periodEl?.addEventListener("change", () => {
    periodDays = Number(periodEl.value) || 0;
    renderList(allFights);
  });
  sortEl?.addEventListener("change", () => {
    sortBy = sortEl.value;
    renderList(allFights);
  });
  bestsSortEl?.addEventListener("change", () => {
    bestsSort = bestsSortEl.value === "tier" ? "tier" : "recent";
    try { localStorage.setItem(BESTS_SORT_KEY, bestsSort); } catch {}
    renderList(allFights);
  });
  killsOnlyEl?.addEventListener("change", () => {
    killsOnly = killsOnlyEl.checked;
    renderList(allFights);
  });
  tabBtns.forEach((b) => b.addEventListener("click", () => {
    if (b.dataset.tab === tab) return;
    tab = b.dataset.tab === "bests" ? "bests" : "fights";
    try { localStorage.setItem(TAB_KEY, tab); } catch {}
    syncTabs();
    renderList(allFights);
  }));
  filterPlayerTrigger?.addEventListener("click", () => {
    setClassDropdownOpen(!classDropdownOpen);
  });
  document.addEventListener("click", (e) => {
    if (classDropdownOpen && filterPlayerEl && !filterPlayerEl.contains(e.target)) {
      setClassDropdownOpen(false);
    }
  });

  trainToggleBtn?.addEventListener("click", () => {
    showTraining = !showTraining;
    try { localStorage.setItem(STORAGE_KEY, showTraining ? "1" : "0"); } catch {}
    syncTrainToggle();
    renderList(allFights);
  });

  deleteToggleBtn?.addEventListener("click", () => {
    showDeleteMode = !showDeleteMode;
    syncDeleteToggle();
  });

  viewBtns.forEach((btn) => {
    btn.addEventListener("click", () => {
      const mode = VIEWS.includes(btn.dataset.view) ? btn.dataset.view : "dungeon";
      if (mode === viewMode) return;
      viewMode = mode;
      try { localStorage.setItem(VIEW_KEY, viewMode); } catch {}
      syncViewToggle();
      expandedGroups.clear();
      renderList(allFights);
    });
  });

  const close = () => {
    panel.classList.remove("open");
    showDeleteMode = false;
    syncDeleteToggle();
    filterPlayer = "";
    search = "";
    if (searchEl) searchEl.value = "";
    expandedGroups.clear();
    if (filterPlayerLabel) filterPlayerLabel.textContent = t("history.filterPlayer", "All classes");
    setClassDropdownOpen(false);
  };

  const isOpen = () => panel.classList.contains("open");

  closeBtn?.addEventListener("click", close);

  return { open, close, isOpen };
};
