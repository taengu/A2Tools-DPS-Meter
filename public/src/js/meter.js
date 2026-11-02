// The figure beside a player's name, in the meter and in Details: combat
// power in thousands the way players quote it ("889k"; below 500 the raw
// number, since "0k" says nothing), gear score in full, or both ("889k ·
// 2,410"). Empty when the party roster never named the player: only your own
// party's members are on it. Shared with details.js, which loads after this
// file.
const partyStatText = (row, stat) => {
  if (stat === "off") return "";
  const gearScore = Number(row?.gearScore) || 0;
  const gearScoreText = gearScore > 0 ? gearScore.toLocaleString() : "";
  if (stat === "gearScore") return gearScoreText;
  const combatPower = Number(row?.combatPower) || 0;
  const thousands = Math.round(combatPower / 1000);
  const combatPowerText =
    combatPower <= 0 ? "" : thousands > 0 ? `${thousands.toLocaleString()}k` : combatPower.toLocaleString();
  if (stat === "both") return [combatPowerText, gearScoreText].filter(Boolean).join(" · ");
  return combatPowerText;
};

// A row's texture offset, 0-719px, the same every time for the same id.
const textureOffsetFor = (id) => {
  let hash = 2166136261;
  for (const ch of String(id)) hash = Math.imul(hash ^ ch.charCodeAt(0), 16777619);
  return (hash >>> 0) % 720;
};

// Whether a bar colour is bright (gold, yellow, pale): a supporter's gold name
// gets a firmer edge there (styles.css .onBright). The colour is the bar's
// --bar as the theme resolves it, read through a canvas so any CSS colour
// works; remembered per colour.
const brightBars = new Map();
let colorProbe = null;
const isBrightColor = (value) => {
  const key = String(value || "").trim();
  if (!key) return false;
  if (brightBars.has(key)) return brightBars.get(key);
  let bright = false;
  try {
    colorProbe ||= document.createElement("canvas").getContext("2d");
    colorProbe.fillStyle = "#000";
    colorProbe.fillStyle = key;
    colorProbe.fillRect(0, 0, 1, 1);
    const [r, g, b] = colorProbe.getImageData(0, 0, 1, 1).data;
    const lin = (c) => ((c /= 255) <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
    bright = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b) > 0.3;
  } catch {}
  brightBars.set(key, bright);
  return bright;
};

// The smallest change of a bar's length (share of the row) that glides;
// smaller ones jump there (see renderRows).
const GLIDE_MIN_JUMP = 0.03;

const createMeterUI = ({
  elList,
  dpsFormatter,
  getUserName,
  onClickUserRow,
  onHoverUserRow,
  onLeaveUserRow,
  getMetric,
  getSortDirection,
  getPinUserToTop,
  getPlayerLimit,
  getPartyStat,
  getJobColor,
}) => {
  const MAX_CACHE = 32;
  const cjkRegex = /[\u3400-\u9FFF\uF900-\uFAFF]/;
  const classIconSrcByJob = new Map();

  const rowViewById = new Map();
  let lastVisibleIds = new Set();
  let pendingRenderRows = null;
  let renderRowsRafId = 0;

  const nowMs = () => (typeof performance !== "undefined" ? performance.now() : Date.now());

  // Forget what each row last drew, so the next refresh redraws it (the
  // party stat setting changed).
  const invalidateRows = () => {
    for (const view of rowViewById.values()) view.lastCombatPowerText = undefined;
  };

  const createRowView = (id) => {
    const rowEl = document.createElement("div");
    rowEl.className = "item";
    rowEl.style.display = "none";
    rowEl.dataset.rowId = String(id);
    // Where this row's bar texture starts with "Offset textures" on: a place
    // of its own, from its id, so it stays put as rows change places.
    rowEl.style.setProperty("--tex-x", `${-textureOffsetFor(id)}px`);

    // The bar spans the full row, so 100% reads as 100%. The readout keeps
    // its own scrim (see .dps in styles.css) for where the bar passes behind.
    const fillTrackEl = document.createElement("div");
    fillTrackEl.className = "fillTrack";

    const fillEl = document.createElement("div");
    fillEl.className = "fill";
    fillTrackEl.appendChild(fillEl);

    const contentEl = document.createElement("div");
    contentEl.className = "content";

    // Ladder position by damage. Kept separate from row order so that
    // "pin me to top" moves the row without misreporting the rank.
    const rankEl = document.createElement("span");
    rankEl.className = "rank";

    const classIconEl = document.createElement("div");
    classIconEl.className = "classIcon";

    const classIconImg = document.createElement("img");
    classIconImg.className = "classIconImg";
    classIconImg.style.visibility = "hidden";

    classIconImg.draggable = false;

    classIconEl.appendChild(classIconImg);

    const nameEl = document.createElement("div");
    nameEl.className = "name";

    // Supporter badge. Sits between the name and the combat power, hidden
    // unless the roster names this player. Inline SVG rather than a lucide
    // `data-lucide` element because rows are built and rebuilt constantly and
    // lucide only swaps placeholders when createIcons() runs.
    const supporterBadgeEl = document.createElement("span");
    supporterBadgeEl.className = "supporterBadge";
    supporterBadgeEl.style.display = "none";
    supporterBadgeEl.setAttribute("aria-hidden", "true");
    supporterBadgeEl.innerHTML =
      '<svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor" stroke="rgba(28,16,0,0.9)" ' +
      'stroke-width="1.3" stroke-linejoin="round" paint-order="stroke" aria-hidden="true">' +
      '<path d="M3 9l4.6 3.4L12 6l4.4 6.4L21 9l-1.9 8.6H4.9z"/><rect x="4.6" y="18.6" width="14.8" height="2.4" rx="0.6"/>' +
      '<circle cx="3" cy="8.4" r="1.7"/><circle cx="12" cy="5" r="1.9"/><circle cx="21" cy="8.4" r="1.7"/></svg>';

    // A party member who runs A2Tools too (party_presence.rs): a small tag.
    const a2toolsBadgeEl = document.createElement("span");
    a2toolsBadgeEl.className = "a2toolsBadge";
    a2toolsBadgeEl.style.display = "none";
    a2toolsBadgeEl.textContent = "A2";
    a2toolsBadgeEl.title = window.i18n?.t?.("meter.a2toolsUser", "Uses A2Tools") ?? "Uses A2Tools";

    // Party combat power, shown beside the name. Hidden unless the party roster
    // packet supplied a value for this player.
    // Deaths in the fight, a small badge after the name and its figure (shown
    // once above 0): its own element, so a long name's ellipsis never hides it.
    const deathCountEl = document.createElement("span");
    deathCountEl.className = "deathCount";
    deathCountEl.style.display = "none";

    const combatPowerEl = document.createElement("span");
    combatPowerEl.className = "combatPower";
    combatPowerEl.style.display = "none";

    const dpsContainer = document.createElement("div");
    const dpsNumber = document.createElement("p");
    dpsContainer.className = "dps";
    const dpsContribution = document.createElement("p");
    dpsContribution.className = "dpsContribution";
    // The extra metrics ticked in the metric menu, after the main figure.
    const dpsExtra = document.createElement("p");
    dpsExtra.className = "dpsExtra";
    dpsExtra.style.display = "none";

    dpsContainer.appendChild(dpsNumber);
    dpsContainer.appendChild(dpsExtra);
    dpsContainer.appendChild(dpsContribution);

    contentEl.appendChild(rankEl);
    contentEl.appendChild(classIconEl);
    contentEl.appendChild(nameEl);
    contentEl.appendChild(supporterBadgeEl);
    contentEl.appendChild(a2toolsBadgeEl);
    contentEl.appendChild(combatPowerEl);
    contentEl.appendChild(deathCountEl);
    contentEl.appendChild(dpsContainer);
    rowEl.appendChild(fillTrackEl);
    rowEl.appendChild(contentEl);

    const view = {
      id,
      rowEl,
      prevContribClass: "",
      nameEl,
      supporterBadgeEl,
      a2toolsBadgeEl,
      combatPowerEl,
      rankEl,
      dpsContainer,
      classIconEl,
      classIconImg,
      dpsNumber,
      dpsExtra,
      dpsContribution,
      fillEl,
      currentRow: null,
      lastSeenAt: 0,
      isVisible: false,
      lastNameText: "",
      lastCombatPowerText: "",
      deathCountEl,
      lastDeaths: 0,
      lastIsCjk: false,
      lastIsSupporter: false,
      lastMetricText: "",
      lastExtraText: "",
      lastContributionText: "",
      lastRankText: "",
      lastFillRatio: -1,
      lastClassIconSrc: "",
      lastIsUser: false,
      lastIsIdentifying: false,
      hoverRipplePlayed: false,
      hoverRippleTimer: null,
    };

    rowEl.addEventListener("mouseenter", (event) => {
      if (!view.hoverRipplePlayed) {
        view.rowEl.classList.add("hoverRippleOnce");
        view.hoverRipplePlayed = true;
        if (view.hoverRippleTimer) {
          clearTimeout(view.hoverRippleTimer);
        }
        view.hoverRippleTimer = setTimeout(() => {
          view.rowEl.classList.remove("hoverRippleOnce");
          view.hoverRippleTimer = null;
        }, 950);
      }
      onHoverUserRow?.(view.currentRow, event);
    });


    rowEl.addEventListener("mousemove", (event) => {
      // The tooltip coalesces pointer events into one update per display frame.
      onHoverUserRow?.(view.currentRow, event);
    });
    rowEl.addEventListener("mouseleave", () => {
      view.hoverRipplePlayed = false;
      if (view.hoverRippleTimer) {
        clearTimeout(view.hoverRippleTimer);
        view.hoverRippleTimer = null;
      }
      view.rowEl.classList.remove("hoverRippleOnce");
      onLeaveUserRow?.(view.currentRow);
    });

    rowEl.addEventListener("click", () => {
      // if (view.currentRow?.isUser)
      onClickUserRow?.(view.currentRow);
    });

    return view;
  };

  const getRowView = (id) => {
    let view = rowViewById.get(id);
    if (!view) {
      view = createRowView(id);
      rowViewById.set(id, view);
      elList.appendChild(view.rowEl);
    }
    return view;
  };

  const getDisplayRows = (sortedAll) => {
    const limit = (typeof getPlayerLimit === "function" ? getPlayerLimit() : 6) || 6;
    const topN = sortedAll.slice(0, limit);
    const user = sortedAll.find((x) => x.isUser);

    if (!user) return topN;
    const pinUser = typeof getPinUserToTop === "function" && getPinUserToTop();
    if (pinUser) {
      return [user, ...topN.filter((row) => !row.isUser)];
    }
    if (topN.some((x) => x.isUser)) return topN;
    return [...topN, user];
  };

  const pruneCache = (keepIds) => {
    if (rowViewById.size <= MAX_CACHE) return;

    const candidates = [];
    for (const [id, view] of rowViewById) {
      if (keepIds.has(id)) {
        continue;
      }
      candidates.push({ id, t: view.lastSeenAt || 0 });
    }

    candidates.sort((a, b) => a.t - b.t); // 오래된거 제거

    for (let i = 0; rowViewById.size > MAX_CACHE && i < candidates.length; i++) {
      const id = candidates[i].id;
      const view = rowViewById.get(id);
      if (!view) continue;
      view.rowEl.remove();
      rowViewById.delete(id);
    }
  };

  const resolveMetric = (row) => {
    if (typeof getMetric === "function") {
      return getMetric(row);
    }
    const dps = Number(row?.dps) || 0;
    const suffix = window.i18n?.t?.("meter.dpsSuffix", "/s") ?? "/s";
    return { value: dps, text: `${dpsFormatter.format(dps)}${suffix}` };
  };

  let lastOrderKey = "";
  // Each metric's widest figure on the rows, in characters, so the same
  // metric lines up down the rows (styles.css --mw-<metric>).
  const columnWidths = new Map();

  const renderRows = (rows, rankById) => {
    const now = nowMs();
    const nextVisibleIds = new Set();

    const hadRows = elList.classList.contains("hasRows");
    const hasRows = rows.length > 0;
    if (hadRows !== hasRows) {
      elList.classList.toggle("hasRows", hasRows);
    }

    let topMetric = 1;
    for (const row of rows) {
      const metricValue = Number(resolveMetric(row)?.value) || 0;
      if (metricValue > topMetric) topMetric = metricValue;
    }
    const visibleTotalDamage = rows.reduce((sum, row) => sum + (Number(row?.totalDamage) || 0), 0);

    // Build order key to detect if DOM reordering is needed
    let orderKey = "";
    const validRows = [];
    for (const row of rows) {
      if (!row) continue;
      const id = row.id ?? row.name;
      if (!id) continue;
      validRows.push({ row, id });
      orderKey += id + ",";
    }

    const needsReorder = orderKey !== lastOrderKey;
    lastOrderKey = orderKey;
    const widest = new Map();
    const widen = (mode, text) => {
      if (mode) widest.set(mode, Math.max(widest.get(mode) || 0, String(text).length));
    };

    for (const [pos, { row, id }] of validRows.entries()) {
      nextVisibleIds.add(id);

      const view = getRowView(id);
      view.currentRow = row;
      view.lastSeenAt = now;

      // The row's place from the top, for themes that colour bars by it
      // (Spectrum, Aurora in styles.css).
      if (view.lastPos !== pos) {
        view.rowEl.style.setProperty("--row-pos", String(pos));
        view.lastPos = pos;
      }

      if (!view.isVisible) {
        view.rowEl.style.display = "";
        view.isVisible = true;
      }

      const isUser = !!row.isUser;
      if (view.lastIsUser !== isUser) {
        view.rowEl.classList.toggle("isUser", isUser);
        view.lastIsUser = isUser;
      }

      const isOthers = !!row.isOthers;
      if (view.lastIsOthers !== isOthers) {
        view.rowEl.classList.toggle("isOthers", isOthers);
        view.lastIsOthers = isOthers;
      }

      const isIdentifying = !!row.isIdentifying;
      if (view.lastIsIdentifying !== isIdentifying) {
        view.rowEl.classList.toggle("isIdentifying", isIdentifying);
        view.lastIsIdentifying = isIdentifying;
      }

      const rowId = row.id ?? row.name ?? "";
      const nameText = row.isIdentifying
        ? window.i18n?.format?.("meter.identifyingPlayer", { id: rowId }, `#${rowId}`) ??
          `#${rowId}`
        : row.name ?? "";
      if (view.lastNameText !== nameText) {
        view.nameEl.textContent = nameText;
        view.lastNameText = nameText;
      }

      const isCjk = cjkRegex.test(nameText);
      // Its own cached field rather than folded into the name: lastNameText
      // short-circuits when the text is unchanged, so a supporter whose name
      // stays the same would never get the class applied.
      const isSupporter = !!row.isSupporter;
      if (view.lastIsSupporter !== isSupporter) {
        view.nameEl.classList.toggle("isSupporter", isSupporter);
        view.rowEl.classList.toggle("isSupporterRow", isSupporter);
        view.supporterBadgeEl.style.display = isSupporter ? "inline-flex" : "none";
        view.lastIsSupporter = isSupporter;
      }

      const usesA2tools = !!row.usesA2tools && !row.isUser;
      if (view.lastUsesA2tools !== usesA2tools) {
        view.a2toolsBadgeEl.style.display = usesA2tools ? "inline-flex" : "none";
        view.lastUsesA2tools = usesA2tools;
      }

      if (view.lastIsCjk !== isCjk) {
        view.nameEl.classList.toggle("isCjk", isCjk);
        view.lastIsCjk = isCjk;
      }

      // Beside the name: combat power or gear score, as Settings → Meter
      // Settings says (partyStatText, above).
      const combatPowerText = partyStatText(row, getPartyStat?.());
      if (view.lastCombatPowerText !== combatPowerText) {
        view.combatPowerEl.textContent = combatPowerText;
        view.combatPowerEl.style.display = combatPowerText ? "" : "none";
        view.lastCombatPowerText = combatPowerText;
      }
      const deaths = Math.max(0, Number(row.deaths) || 0);
      if (view.lastDeaths !== deaths) {
        view.deathCountEl.textContent = deaths ? String(deaths) : "";
        view.deathCountEl.style.display = deaths ? "" : "none";
        view.deathCountEl.title = deaths
          ? window.i18n?.format?.("meter.deaths", { count: deaths }, `Died ${deaths} times in this fight`) ??
            `Died ${deaths} times in this fight`
          : "";
        view.lastDeaths = deaths;
      }

      // "Unknown" is a row whose class is not known yet: no icon, rather than
      // a broken image of a file that does not exist (issue #9).
      // The class's colour, for a theme that colours each bar by class
      // ("AION2 (class colours)"); the others leave it unused.
      const classColor = (row.job && getJobColor?.(row.job)) || "";
      if (view.lastClassColor !== classColor) {
        if (classColor) view.rowEl.style.setProperty("--class-fill", classColor);
        else view.rowEl.style.removeProperty("--class-fill");
        view.lastClassColor = classColor;
      }
      // A supporter's name on a bright bar: a firmer edge. Looked at again only
      // when what decides the bar's colour changes (theme, class, own row).
      const barKey = isSupporter
        ? `${document.documentElement.dataset.theme || ""}|${row.job || ""}|${!!row.isUser}`
        : "";
      if (view.lastBarKey !== barKey) {
        view.lastBarKey = barKey;
        const bright = !!barKey && isBrightColor(getComputedStyle(view.fillEl).getPropertyValue("--bar"));
        view.nameEl.classList.toggle("onBright", bright);
      }

      window.i18n?.setClassTooltip?.(view.classIconImg, row.job);
      if (row.job && row.job !== "Unknown") {
        if (!classIconSrcByJob.has(row.job)) {
          classIconSrcByJob.set(row.job, `./assets/${row.job}.png`);
        }
        const src = classIconSrcByJob.get(row.job);
        if (view.lastClassIconSrc !== src) {
          view.classIconImg.src = src;
          view.lastClassIconSrc = src;
        }
        if (view.classIconImg.style.visibility !== "visible") {
          view.classIconImg.style.visibility = "visible";
        }
      } else {
        if (view.lastClassIconSrc) {
          view.lastClassIconSrc = "";
          view.classIconImg.removeAttribute("src");
        }
        if (view.classIconImg.style.visibility !== "hidden") {
          view.classIconImg.style.visibility = "hidden";
        }
      }

      const metric = resolveMetric(row) || { value: 0, text: "-" };
      const metricValue = Number(metric.value) || 0;
      const damageContribution =
        visibleTotalDamage > 0 ? (Number(row.totalDamage) / visibleTotalDamage) * 100 : 0;

      let contributionClass = "";
      if (damageContribution < 3) {
        contributionClass = "error";
      } else if (damageContribution < 5) {
        contributionClass = "warning";
      }
      if (view.prevContribClass !== contributionClass) {
        if (view.prevContribClass) {
          view.rowEl.classList.remove(view.prevContribClass);
        }
        if (contributionClass) {
          view.rowEl.classList.add(contributionClass);
        }
        view.prevContribClass = contributionClass;
      }

      const metricText = metric.text;
      if (view.lastMetricText !== metricText) {
        view.dpsNumber.textContent = metricText;
        view.lastMetricText = metricText;
      }
      // Each metric in its own colour (data-metric, styles.css).
      const metricMode = metric.mode || "";
      widen(metricMode, metricText);
      if (view.dpsNumber.dataset.metric !== metricMode) view.dpsNumber.dataset.metric = metricMode;
      const extras = Array.isArray(metric.extras) ? metric.extras : [];
      for (const e of extras) widen(e.mode, e.text);
      const extraText = extras.map((e) => `${e.mode}:${e.text}`).join("|");
      if (view.lastExtraText !== extraText) {
        view.dpsExtra.replaceChildren(
          ...extras.map((e) => {
            const span = document.createElement("span");
            span.dataset.metric = e.mode;
            span.textContent = e.text;
            return span;
          }),
        );
        view.dpsExtra.style.display = extraText ? "" : "none";
        view.lastExtraText = extraText;
      }

      const contributionText = `${damageContribution.toFixed(1)}%`;
      if (view.lastContributionText !== contributionText) {
        view.dpsContribution.textContent = contributionText;
        view.lastContributionText = contributionText;
      }

      const rankText = String(rankById?.get(id) ?? "");
      if (view.lastRankText !== rankText) {
        view.rankEl.textContent = rankText;
        view.rowEl.classList.toggle("isRankOne", rankText === "1");
        view.lastRankText = rankText;
      }

      const ratio = Math.max(0, Math.min(1, metricValue / topMetric));
      if (view.lastFillRatio !== ratio) {
        // Width rather than scaleX: the bar's lit leading edge is a fixed
        // 2px child, and a scaled parent would squash it (and its glow) by
        // the fill ratio. Six absolutely-positioned rows, rAF-throttled, so
        // the layout cost is not measurable.
        // It glides only when the change shows: each glide step re-lays and
        // repaints the bar (its texture too), and in a steady fight most
        // updates move it by a hair, so gliding them all kept every bar
        // animating, about half the meter's drawing (2026-10-29).
        const jump = Math.abs(ratio - (view.lastFillRatio ?? 0));
        view.fillEl.classList.toggle("noGlide", jump < GLIDE_MIN_JUMP);
        view.fillEl.style.width = `${(ratio * 100).toFixed(2)}%`;
        view.lastFillRatio = ratio;
      }

      // Only touch DOM order when the sorted list actually changed
      if (needsReorder) {
        elList.appendChild(view.rowEl);
      }
    }

    for (const id of lastVisibleIds) {
      if (nextVisibleIds.has(id)) continue;
      const view = rowViewById.get(id);
      if (view && view.isVisible) {
        view.rowEl.style.display = "none";
        view.isVisible = false;
      }
    }

    lastVisibleIds = nextVisibleIds;

    // Set only when a column's widest figure changed length.
    for (const [mode, chars] of widest) {
      if (columnWidths.get(mode) === chars) continue;
      columnWidths.set(mode, chars);
      elList.style.setProperty(`--mw-${mode}`, `${chars}ch`);
    }

    pruneCache(nextVisibleIds);
  };

  const flushPendingRows = () => {
    renderRowsRafId = 0;
    const rows = pendingRenderRows;
    pendingRenderRows = null;
    if (!rows) return;

    const arr = Array.isArray(rows) ? rows.slice() : [];
    const sortDirection = typeof getSortDirection === "function" ? getSortDirection() : "desc";
    arr.sort((a, b) => {
      const aMetric = Number(resolveMetric(a)?.value) || 0;
      const bMetric = Number(resolveMetric(b)?.value) || 0;
      return sortDirection === "asc" ? aMetric - bMetric : bMetric - aMetric;
    });

    // Rank is always "1 = most damage", independent of the ascending /
    // descending list toggle and of pinning the user to the top.
    const rankById = new Map();
    arr
      .slice()
      .sort((a, b) => (Number(resolveMetric(b)?.value) || 0) - (Number(resolveMetric(a)?.value) || 0))
      .forEach((row, index) => {
        const id = row?.id ?? row?.name;
        if (id) rankById.set(id, index + 1);
      });

    renderRows(getDisplayRows(arr), rankById);
  };

  const updateFromRows = (rows) => {
    pendingRenderRows = rows;
    if (renderRowsRafId) return;
    renderRowsRafId = requestAnimationFrame(flushPendingRows);
  };
  const onResetMeterUi = () => {
    if (renderRowsRafId) {
      cancelAnimationFrame(renderRowsRafId);
      renderRowsRafId = 0;
    }
    pendingRenderRows = null;
    lastOrderKey = "";
    elList.classList.remove("hasRows");
    lastVisibleIds = new Set();

    const battleTimeEl = elList.querySelector(".battleTime");
    if (battleTimeEl) {
      elList.replaceChildren(battleTimeEl);
    } else {
      elList.replaceChildren();
    }

    rowViewById.clear();
    classIconSrcByJob.clear();
  };

  const getRowById = (id) => {
    if (id === null || id === undefined) return null;
    const view = rowViewById.get(id) || rowViewById.get(String(id));
    return view?.currentRow || null;
  };

  return { updateFromRows, onResetMeterUi, getRowById, invalidateRows };
};
