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
  const filterBossEl = panel.querySelector(".historyFilterBoss");
  const filterPlayerEl = panel.querySelector(".historyFilterPlayer");
  const filterPlayerTrigger = filterPlayerEl?.querySelector(".historyClassDropdownTrigger");
  const filterPlayerLabel = filterPlayerEl?.querySelector(".historyClassDropdownLabel");
  const filterPlayerMenu = filterPlayerEl?.querySelector(".historyClassDropdownMenu");
  const filterDateEl = panel.querySelector(".historyFilterDate");

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
  let filterBoss = "";
  let filterPlayer = "";
  let filterDate = "";
  let classDropdownOpen = false;

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

  const populateDropdowns = (fights) => {
    if (!filterBossEl || !filterPlayerEl || !filterDateEl) return;
    const allOption = (label) => `<option value="">${label}</option>`;

    const bossNames = [...new Set(fights.map((f) => f.bossName || "").filter(Boolean))].sort();
    filterBossEl.innerHTML = allOption(t("history.filterBoss", "All bosses"));
    bossNames.forEach((name) => {
      const opt = document.createElement("option");
      opt.value = name;
      opt.textContent = name;
      if (name === filterBoss) opt.selected = true;
      filterBossEl.appendChild(opt);
    });

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

    const dates = [...new Set(fights.map((f) => formatDate(f.startTimeMs).slice(0, 10)).filter((d) => d !== "-"))].sort().reverse();
    filterDateEl.innerHTML = allOption(t("history.filterDate", "All dates"));
    dates.forEach((date) => {
      const opt = document.createElement("option");
      opt.value = date;
      opt.textContent = date;
      if (date === filterDate) opt.selected = true;
      filterDateEl.appendChild(opt);
    });
  };

  const applyFilters = (fights) => {
    return fights.filter((f) => {
      if (f.isTrain && !showTraining) return false;
      if (filterBoss && (f.bossName || "") !== filterBoss) return false;
      if (filterPlayer) {
        const jobs = Array.isArray(f.jobs) ? f.jobs : [];
        if (!jobs.includes(filterPlayer)) return false;
      }
      if (filterDate && formatDate(f.startTimeMs).slice(0, 10) !== filterDate) return false;
      return true;
    });
  };

  // Status line for the dry run. Created lazily so the panel markup does not
  // have to carry an element that is empty almost always.
  const showNote = (text, isError) => {
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
        false
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
    const tier = fight.dungeonId
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
    metaEl.appendChild(dmgEl);

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
    if (!fight.isLive && !fight.isTrain) {
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

  const buildGroup = (group) => {
    // Most recent run first within a section.
    group.fights.sort((a, b) => startMs(b) - startMs(a));

    const wrap = document.createElement("div");
    wrap.className = "historyGroup";

    const header = document.createElement("div");
    header.className = "historyGroupHeader";

    const chevron = document.createElement("span");
    chevron.className = "historyGroupChevron";
    chevron.innerHTML = `<svg width="10" height="6" viewBox="0 0 10 6" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M0 0l5 6 5-6z" fill="currentColor"/></svg>`;

    const nameEl = document.createElement("div");
    nameEl.className = "historyGroupName";
    nameEl.textContent = group.name;

    const countEl = document.createElement("span");
    countEl.className = "historyGroupCount";
    countEl.textContent = fightCountLabel(group.fights.length);

    header.appendChild(chevron);
    header.appendChild(nameEl);
    header.appendChild(countEl);

    const childWrap = document.createElement("div");
    childWrap.className = "historyGroupChildren";

    const renderChildren = () => {
      if (childWrap.childElementCount) return;
      const frag = document.createDocumentFragment();
      group.fights.forEach((f) => frag.appendChild(buildRow(f, group.rowOptions)));
      childWrap.appendChild(frag);
    };

    const expanded = expandedGroups.has(group.name);
    header.classList.toggle("expanded", expanded);
    childWrap.style.display = expanded ? "" : "none";
    if (expanded) renderChildren();

    header.addEventListener("click", () => {
      const nowExpanded = !expandedGroups.has(group.name);
      if (nowExpanded) {
        expandedGroups.add(group.name);
        renderChildren();
      } else {
        expandedGroups.delete(group.name);
      }
      header.classList.toggle("expanded", nowExpanded);
      childWrap.style.display = nowExpanded ? "" : "none";
    });

    wrap.appendChild(header);
    wrap.appendChild(childWrap);
    return wrap;
  };

  // A dungeon section's title: the dungeon and its difficulty, or Open world.
  const dungeonTitle = (dungeonId) => {
    const id = Number(dungeonId) || 0;
    if (!id) return t("history.openWorld", "Open world");
    return window.i18n?.getDungeonLabel?.(id) || `#${id}`;
  };

  const renderGrouped = (visible, byDungeon) => {
    const groups = new Map();
    visible.forEach((f) => {
      // Keyed by what the header says, so two instance ids with the same name
      // and difficulty share one section.
      const key = byDungeon ? dungeonTitle(f.dungeonId) : (f.bossName || `Boss #${f.targetId}`);
      let g = groups.get(key);
      if (!g) {
        g = { name: key, fights: [], rowOptions: byDungeon ? { child: true } : { grouped: true } };
        groups.set(key, g);
      }
      g.fights.push(f);
    });
    // Sections ordered by their most recent run.
    const ordered = [...groups.values()].sort(
      (a, b) => Math.max(...b.fights.map(startMs)) - Math.max(...a.fights.map(startMs))
    );
    const frag = document.createDocumentFragment();
    ordered.forEach((g) => frag.appendChild(buildGroup(g)));
    listEl.appendChild(frag);
  };

  const renderList = (fights) => {
    if (!listEl) return;
    listEl.innerHTML = "";
    renderedCount = 0;

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
    populateDropdowns(allFights);
    renderList(allFights);
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

  filterBossEl?.addEventListener("change", () => {
    filterBoss = filterBossEl.value;
    renderList(allFights);
  });
  filterPlayerTrigger?.addEventListener("click", () => {
    setClassDropdownOpen(!classDropdownOpen);
  });
  document.addEventListener("click", (e) => {
    if (classDropdownOpen && filterPlayerEl && !filterPlayerEl.contains(e.target)) {
      setClassDropdownOpen(false);
    }
  });
  filterDateEl?.addEventListener("change", () => {
    filterDate = filterDateEl.value;
    renderList(allFights);
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
    filterBoss = "";
    filterPlayer = "";
    filterDate = "";
    expandedGroups.clear();
    if (filterBossEl) filterBossEl.selectedIndex = 0;
    if (filterPlayerLabel) filterPlayerLabel.textContent = t("history.filterPlayer", "All classes");
    setClassDropdownOpen(false);
    if (filterDateEl) filterDateEl.selectedIndex = 0;
  };

  const isOpen = () => panel.classList.contains("open");

  closeBtn?.addEventListener("click", close);

  return { open, close, isOpen };
};
