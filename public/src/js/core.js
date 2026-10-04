// Settings whose change in the Settings window must redraw the meter, mapped to
// the control that applies them. See DpsApp.applyRemoteSettingChange().
const REMOTE_APPLIED_SETTING_CONTROLS = {
  "dpsMeter.roundDps": ".roundDpsCheckbox",
  "dpsMeter.showTotalDps": ".showTotalDpsCheckbox",
  "dpsMeter.pinMeToTop": ".pinMeToTopCheckbox",
  "dpsMeter.mainPlayerNamesBold": ".playerNamesBoldCheckbox",
  "dpsMeter.mainPlayerDpsBold": ".playerDpsBoldCheckbox",
  "dpsMeter.showPing": ".showPingCheckbox",
  "dpsMeter.bossNameSize": ".bossNameSizeInput",
  "dpsMeter.showSupporterColors": ".showSupporterColorsCheckbox",
  "dpsMeter.showSuspendBtn": ".showSuspendBtnCheckbox",
  "dpsMeter.showLockBtn": ".showLockBtnCheckbox",
};

class DpsApp {
  constructor() {
    if (DpsApp.instance) return DpsApp.instance;

    this.POLL_MS = 100;
    this.WINDOW_TITLE_POLL_MS = 3000;
    this.USER_NAME = "";
    this.onlyShowUser = false;
    this.debugLoggingEnabled = false;
    this.pinMeToTop = false;
    this.slimMode = false;
    this.mainPlayerNamesBold = true;
    this.showSupporterColors = true;
    this.mainPlayerDpsBold = true;
    this.includeMainMeterScreenshot = false;
    this.saveScreenshotToFolder = false;
    this.screenshotFolder = "";
    this.storageKeys = {
      userName: "dpsMeter.userName",
      onlyShowUser: "dpsMeter.onlyShowUser",
      allTargetsWindowMs: "dpsMeter.allTargetsWindowMs",
      trainSelectionMode: "dpsMeter.trainSelectionMode",
      targetSelectionWindowMs: "dpsMeter.targetSelectionWindowMs",
      meterFillOpacity: "dpsMeter.meterFillOpacity",
      detailsBackgroundOpacity: "dpsMeter.detailsBackgroundOpacity",
      detailsFontSize: "dpsMeter.detailsFontSize",
      detailsIconSize: "dpsMeter.detailsIconSize",
      detailsIncludeMeterScreenshot: "dpsMeter.detailsIncludeMeterScreenshot",
      detailsSaveScreenshotToFolder: "dpsMeter.detailsSaveScreenshotToFolder",
      detailsScreenshotFolder: "dpsMeter.detailsScreenshotFolder",
      detailsHiddenColumns: "dpsMeter.detailsHiddenColumns",
      detailsSeenColumns: "dpsMeter.detailsSeenColumns",
      defaultMeterMode: "dpsMeter.defaultMeterMode",
      targetSelection: "dpsMeter.targetSelection",
      displayMode: "dpsMeter.displayMode",
      language: "dpsMeter.language",
      debugLogging: "dpsMeter.debugLoggingEnabled",
      pinMeToTop: "dpsMeter.pinMeToTop",
      mainPlayerNamesBold: "dpsMeter.mainPlayerNamesBold",
      showSupporterColors: "dpsMeter.showSupporterColors",
      mainPlayerDpsBold: "dpsMeter.mainPlayerDpsBold",
      showPing: "dpsMeter.showPing",
      showTotalDps: "dpsMeter.showTotalDps",
      roundDps: "dpsMeter.roundDps",
      playerLimit: "dpsMeter.playerLimit",
      theme: "dpsMeter.theme",
      slimMode: "dpsMeter.slimMode",
      autoHideMeter: "dpsMeter.autoHideMeter",
      bossLogs: "dpsMeter.bossLogsEnabled",
      saveRawPackets: "dpsMeter.saveRawPackets",
      autoUpload: "dpsMeter.autoUpload",
      discordActivity: "dpsMeter.discordActivity",
      discordPromoShown: "dpsMeter.discordPromoShown",
      windowOpacity: "dpsMeter.windowOpacity",
      bossNameSize: "dpsMeter.bossNameSize",
      betaUi: "dpsMeter.betaUi",
      detailsMonitor: "dpsMeter.detailsMonitor",
      showSuspendBtn: "dpsMeter.showSuspendBtn",
      showLockBtn: "dpsMeter.showLockBtn",
      overlayLocked: "dpsMeter.overlayLocked",
    };

    this.dpsFormatter = new Intl.NumberFormat("en-US");
    this.lastJson = null;
    this.isCollapse = false;
    this._windowHidden = false;
    this.displayMode = "dps";
    this.betaUi = true;
    this.theme = "aion2";
    this.availableThemes = [
      "aion2",
      "asmodian",
      "cogni",
      "elyos",
      "ember",
      "fera",
      "frost",
      "natura",
      "obsidian",
      "varian",
    ];
    this.supportQrImages = {
      afdian: "./assets/afdian.png",
      kofi: "./assets/kofi.png",
      wechat: "./assets/wechat.png",
    };
    this.jobColorMap = {
      정령성: "#E06BFF",
      Spiritmaster: "#E06BFF",
      궁성: "#41D98A",
      Ranger: "#41D98A",
      살성: "#7BE35A",
      Assassin: "#7BE35A",
      수호성: "#5F8CFF",
      Templar: "#5F8CFF",
      마도성: "#9A6BFF",
      Sorcerer: "#9A6BFF",
      호법성: "#FF9A3D",
      Chanter: "#FF9A3D",
      치유성: "#F2C15A",
      Cleric: "#F2C15A",
      검성: "#4FD1C5",
      Gladiator: "#4FD1C5",
      권성: "#E85D5D",
      Brawler: "#E85D5D",
      Fighter: "#E85D5D",
    };

    // 빈데이터 덮어쓰기 방지 스냅샷
    this.lastSnapshot = null;
    // reset 직후 서버가 구 데이터 계속 주는 현상 방지
    this.resetPending = false;
    this.refreshPending = false;
    this.refreshPendingStartedAt = 0;

    this.BATTLE_TIME_BASIS = "render";
    this.GRACE_MS = 30000;
    this.GRACE_ARM_MS = 3000;
    this.DETAILS_FONT_SIZE_MIN = 11;
    this.DETAILS_FONT_SIZE_MAX = 20;
    this.DETAILS_ICON_SIZE_MIN = 20;
    this.DETAILS_ICON_SIZE_MAX = 56;


    // battleTime 캐시
    this._battleTimeVisible = false;
    this._lastBattleTimeMs = null;

    this._pollTimer = null;
    this._windowTitleTimer = null;

    this.i18n = window.i18n;
    this.targetSelection = "bossTargets";
    this.listSortDirection = "desc";
    this.lastTargetMode = "";
    this.lastTargetName = "";
    this.lastTargetId = 0;
    this._lastRenderedListSignature = "";
    this._lastRenderedTargetLabel = "";
    this._lastTargetSelection = this.targetSelection;
    this._lastRenderedRowsSummary = null;
    this.localPlayerId = null;
    this.trainSelectionMode = "all";
    this._detailsFlashTimer = null;
    this._meterFlashTimer = null;
    this._recentLocalIdByName = new Map();
    this.pinnedDetailsRowId = null;
    this.hoveredDetailsRowId = null;
    this.setWindowDragFreeze(false);
    this.latestRowsById = new Map();
    this.isWindowDragging = false;
    this.isMeterBarHovered = false;
    this.deferFetchUntilDragEnd = false;
    this.deferFetchUntilHoverEnd = false;
    this.suppressRowInteractionUntilMs = 0;
    this.hoverTooltipCacheByRowId = new Map();
    this.hoverTooltipRequestSeqByRowId = new Map();
    this.hoverTooltipEl = null;
    this.hoverMousePos = { x: 0, y: 0 };
    this.hoverTooltipPendingRowIds = new Set();

    DpsApp.instance = this;
  }

  safeGetStorage(key) {
    try {
      return localStorage.getItem(key);
    } catch (e) {
      globalThis.uiDebug?.log?.("localStorage.get blocked", { key, error: String(e) });
      return null;
    }
  }

  safeSetStorage(key, value) {
    try {
      localStorage.setItem(key, value);
    } catch (e) {
      globalThis.uiDebug?.log?.("localStorage.set blocked", { key, error: String(e) });
    }
  }

  safeGetSetting(key) {
    try {
      const bridgeValue = window.javaBridge?.getSetting?.(key);
      if (bridgeValue !== undefined && bridgeValue !== null) {
        return bridgeValue;
      }
    } catch (e) {
      globalThis.uiDebug?.log?.("getSetting blocked", { key, error: String(e) });
    }
    return this.safeGetStorage(key);
  }

  safeSetSetting(key, value) {
    try {
      window.javaBridge?.setSetting?.(key, value);
    } catch (e) {
      globalThis.uiDebug?.log?.("setSetting blocked", { key, error: String(e) });
    }
    this.safeSetStorage(key, value);
  }


  static createInstance() {
    if (!DpsApp.instance) DpsApp.instance = new DpsApp();
    return DpsApp.instance;
  }

  start() {
    window._dpsApp = this;
    this.elList = document.querySelector(".list");
    this.elBossName = document.querySelector(".bossName");
    this.elBossName.textContent = this.getDefaultTargetLabel();
    this._lastRenderedTargetLabel = this.elBossName.textContent;
    this.elBossHpBar = document.querySelector(".bossHpBar");
    this.elBossHpFill = document.querySelector(".bossHpFill");
    this.elBossHpText = document.querySelector(".bossHpText");
    this.battleTimeRoot = document.querySelector(".battleTime");
    this.analysisStatusEl = document.querySelector(".analysisStatus");
    this.aionRunning = false;
    this.isDetectingPort = false;
    this._connectionStatusOverride = false;

    this.resetBtn = document.querySelector(".resetBtn");
    this.suspendBtn = document.querySelector(".suspendBtn");
    this.lockBtn = document.querySelector(".lockBtn");
    this.headerBtns = document.querySelector(".headerBtns");
    this.targetModeBtn = document.querySelector(".footerBtns .targetModeBtn");
    this.collapseBtn = document.querySelector(".collapseBtn");
    this.metricToggleBtn = document.querySelector(".metricToggleBtn");

    this.bindHeaderButtons();
    this.bindDragToMoveWindow();
    this.bindResizeHandle();
    this.initHoverTooltip();

    this.meterUI = createMeterUI({
      elList: this.elList,
      dpsFormatter: this.dpsFormatter,
      getUserName: () => this.USER_NAME,
      getMetric: (row) => this.getMetricForRow(row),
      getSortDirection: () => this.listSortDirection,
      getPinUserToTop: () => this.pinMeToTop,
      getPlayerLimit: () => this.playerLimit,
      onHoverUserRow: (row, event) => {
        if (this.shouldSuppressRowInteractions()) return;
        this.openHoverDetailsRow(row, event);
      },
      onLeaveUserRow: () => {
        this.hoveredDetailsRowId = null;
        requestAnimationFrame(() => {
          const hoveredRow = this.elList?.querySelector?.(".item:hover");
          if (hoveredRow) return;
          // Only the hover tooltip belongs to hovering. Leaving a row used to
          // close the Details view as well, which killed a Details opened any
          // other way — the target-name path clears pinnedDetailsRowId before
          // opening, so the pin guard never protected it. Hover never opens the
          // panel (openHoverDetailsRow bails when it is already open), so there
          // is nothing here for it to close.
          this.hideHoverTooltip();
        });
      },
      onClickUserRow: (row) => {
        if (!row || this.isWindowDragging) return;
        const rowId = Number(row?.id);
        const pinnedId = Number.isFinite(rowId) && rowId > 0 ? rowId : null;
        this.hideHoverTooltip();
        const options = this.getDefaultDetailsOpenOptions();
        // Details lives in its own window; the overlay only says what to show.
        // Only the in-overlay fallback pins the row — pinning suppresses the
        // hover tooltip, which should stay live when the panel is elsewhere.
        this.openDetailsSurface(
          {
            kind: "row",
            row: {
              id: pinnedId,
              name: row?.name ?? "",
              job: row?.job ?? "",
              isIdentifying: !!row?.isIdentifying,
            },
            defaultTargetAll: !!options.defaultTargetAll,
            defaultTargetId: options.defaultTargetId ?? null,
          },
          () => {
            this.pinnedDetailsRowId = pinnedId;
            this.detailsUI.open(row, { pin: true, ...options });
          }
        );
      },
    });


    const withBacklog = (text) => {
      if (!window.javaBridge?.isRunningFromIde?.()) return text;
      const backlog = window.javaBridge?.getParsingBacklog?.();
      if (!Number.isFinite(backlog)) return text;
      return `${text} (backlog: ${backlog})`;
    };

    const getBattleTimeStatusText = () => {
      if (this._captureSuspended) {
        return this.i18n?.t?.("battleTime.suspended", "App suspended") ?? "App suspended";
      }
      if (!this.aionRunning) {
        const text = this.i18n?.t("battleTime.notRunning", "AION2 not running") ?? "AION2 not running";
        return withBacklog(text);
      }
      if (this.isDetectingPort) {
        const text = this.i18n?.t("connection.detecting", "Detecting AION2 connection...") ??
          "Detecting AION2 connection...";
        return withBacklog(text);
      }
      if (this.battleTime?.getState?.() === "state-idle") {
        const text = this.i18n?.t("battleTime.idle", "Idle") ?? "Idle";
        return withBacklog(text);
      }
      const text = this.i18n?.t("battleTime.analysing", "Monitoring data...") ?? "Monitoring data...";
      return withBacklog(text);
    };

    this.battleTime = createBattleTimeUI({
      rootEl: document.querySelector(".battleTime"),
      tickSelector: ".tick",
      statusSelector: ".status",
      analysisSelector: ".analysisStatus",
      getAnalysisText: getBattleTimeStatusText,
      graceMs: this.GRACE_MS,
      graceArmMs: this.GRACE_ARM_MS,
      idleMs: 60000,
      visibleClass: "isVisible",
    });
    this.battleTime.setVisible(false);
    this.updateConnectionStatusUi();

    this.pingEl = document.querySelector(".pingDisplay");
    this.showPing = this.safeGetSetting(this.storageKeys.showPing) !== "false";
    // Ping is pushed immediately from PingTracker via window._dpsApp.updatePing().
    // A slow fallback poll handles edge cases (e.g. push not wired yet on startup).
    this._pingTimer = setInterval(() => this.updatePing(), 30000);

    this.showTotalDps = this.safeGetSetting(this.storageKeys.showTotalDps) !== "false";
    // Defaults on: `!== "false"` treats "never set" as enabled.
    this.roundDps = this.safeGetSetting(this.storageKeys.roundDps) !== "false";
    this.meterTotalBar = document.querySelector(".meterTotalBar");
    this.meterTotalDpsEl = document.querySelector(".meterTotalDps");
    this.meterTotalDmgEl = document.querySelector(".meterTotalDmg");
    const savedLimit = parseInt(this.safeGetSetting(this.storageKeys.playerLimit), 10);
    this.playerLimit = Number.isFinite(savedLimit) && savedLimit >= 1 ? savedLimit : 6;

    this.detailsPanel = document.querySelector(".detailsPanel");
    this.detailsClose = document.querySelector(".detailsClose");
    this.detailsBackBtn = document.querySelector(".detailsBackBtn");
    this.detailsFightTitleEl = document.querySelector(".detailsFightTitle");
    this.detailsPartyListEl = document.querySelector(".detailsPartyList");
    this.detailsScreenshotBtn = document.querySelector(".detailsScreenshotBtn");
    this.detailsScreenshotNote = document.querySelector(".detailsScreenshotNote");
    this.detailsStatsEl = document.querySelector(".detailsStats");
    this.skillsListEl = document.querySelector(".skills");

    this.detailsUI = createDetailsUI({
      detailsPanel: this.detailsPanel,
      detailsClose: this.detailsClose,
      detailsBackBtn: this.detailsBackBtn,
      detailsFightTitleEl: this.detailsFightTitleEl,
      detailsPartyListEl: this.detailsPartyListEl,
      detailsStatsEl: this.detailsStatsEl,
      skillsListEl: this.skillsListEl,
      dpsFormatter: this.dpsFormatter,
      getDetails: (row, options) => this.getDetails(row, options),
      getDetailsContext: () => this.getDetailsContext(),
      getDungeonId: () => this.lastDungeonId,
      onPinnedRowChange: (rowId) => {
        const nextId = Number(rowId);
        this.pinnedDetailsRowId = Number.isFinite(nextId) && nextId > 0 ? nextId : null;
      },
      // "Back to History" means the History window now, not a list drawn over
      // the fight — a fight window is one of several and the list is a window
      // in its own right. Only the overlay still opens the panel inline.
      onBack: () => {
        if (window.A2_VIEW === "main") {
          this.historyUI?.open?.();
          return;
        }
        // detailsUI.close() already ran, so a fight window is now empty. Going
        // back means "done with this fight": raise the list and dismiss this
        // window rather than leaving a blank one behind.
        const label = window.__TAURI__?.window?.getCurrentWindow?.()?.label || "";
        const isFightWindow = label.startsWith("details-");
        const sent = window.javaBridge?.requestDetailsView?.({ kind: "history" });
        Promise.resolve(sent)
          .catch(() => { if (!isFightWindow) this.historyUI?.open?.(); })
          .then(() => { if (isFightWindow) window.javaBridge?.closeToolWindow?.(); });
      },
    });
    if (this.detailsScreenshotBtn) {
      let screenshotNoteTimer = null;
      this.detailsScreenshotBtn.addEventListener("click", async () => {
        const tooltipText =
          this.i18n?.t("details.screenshot.captured", "Captured Screenshot") ?? "Captured Screenshot";
        const detailsRect = this.detailsPanel?.classList?.contains("open")
          ? this.detailsPanel.getBoundingClientRect()
          : null;
        const includeMeter = !!this.includeMainMeterScreenshot;
        // In its own window, Details cannot measure the meter (a different
        // window), so the backend adds the meter window to the capture. When
        // Details is a panel beside the meter, both are measured here.
        const ownWindow = document.documentElement.classList.contains("detailsWindow");
        const meterRect = !ownWindow ? document.querySelector(".meter")?.getBoundingClientRect?.() : null;
        const rects = [detailsRect, includeMeter ? meterRect : null].filter(Boolean);
        if (!rects.length) return;
        const left = Math.min(...rects.map((r) => r.left));
        const top = Math.min(...rects.map((r) => r.top));
        const right = Math.max(...rects.map((r) => r.right));
        const bottom = Math.max(...rects.map((r) => r.bottom));
        const saveFile = !!this.saveScreenshotToFolder;
        const result = await window.javaBridge?.captureScreenshot?.({
          x: left,
          y: top,
          width: Math.max(1, right - left),
          height: Math.max(1, bottom - top),
          scale: window.devicePixelRatio || 1,
          includeMeter: includeMeter && ownWindow,
          saveFile,
          folder: saveFile ? this.screenshotFolder : null,
          filename: saveFile ? this.buildScreenshotFilename() : null,
        });
        const clipboardSuccess = !!result?.clipboard;
        const fileSuccess = !!result?.file;
        if (!this.detailsScreenshotNote) return;
        if (!clipboardSuccess && !fileSuccess) {
          this.detailsScreenshotNote.textContent = this.i18n?.t("details.screenshot.failed", "Screenshot failed") ?? "Screenshot failed";
        } else if (clipboardSuccess && fileSuccess) {
          this.detailsScreenshotNote.textContent = "Saved to clipboard + file";
        } else if (fileSuccess) {
          this.detailsScreenshotNote.textContent = "Saved to file";
        } else {
          this.detailsScreenshotNote.textContent = saveFile ? "Saved to clipboard (file failed)" : "Saved to clipboard";
        }
        this.detailsScreenshotBtn.setAttribute("title", fileSuccess ? `${tooltipText}: ${result.file}` : tooltipText);
        this.detailsScreenshotNote.classList.add("isVisible");
        // Flash after the capture, so the flash is not in the picture.
        if (clipboardSuccess || fileSuccess) {
          if (this.detailsPanel) this.triggerDetailsFlash();
          if (includeMeter && meterRect) this.triggerMeterFlash();
        }
        if (screenshotNoteTimer) window.clearTimeout(screenshotNoteTimer);
        screenshotNoteTimer = window.setTimeout(() => {
          this.detailsScreenshotNote?.classList.remove("isVisible");
          this.detailsScreenshotNote.textContent = "";
        }, 2000);
      });
    }
    this.setupDetailsPanelSettings();
    this.setupSettingsPanel();
    this.detailsUI?.updateLabels?.();
    this.i18n?.onChange?.((lang) => {
      this.settingsSelections.language = lang;
      this.initializeSettingsDropdowns();
      this.detailsUI?.updateLabels?.();
      this.detailsUI?.refresh?.();
      this.updateDisplayToggleLabel();
      if (this.battleTime?.setAnalysisTextProvider) {
        this.battleTime.setAnalysisTextProvider(getBattleTimeStatusText);
      }
      this.refreshConnectionInfo();
      this.refreshBossLabel();
      this.updateSupportVisibility(lang);
      this.updateSupportPrimaryAction(lang);
      this.updateSupportQrImage(this.supportPrimaryButton?.dataset.support || "afdian");
    });
    // Settings, Details and History run this same bundle; only the overlay
    // checks, or every Settings open would ask again.
    if (window.A2_VIEW === "main") window.ReleaseChecker?.start?.();
    this.setupConsoleDebugging();
    this.bindNativeHotkeyBridge();

    const storedDisplayMode = this.safeGetStorage(this.storageKeys.displayMode);
    this.setDisplayMode(storedDisplayMode || this.displayMode, { persist: false });

    // History is a browser you leave open: picking a fight launches it into a
    // window of its own so several can be compared, and the list stays put.
    // Only the in-overlay fallback closes itself, because there the list and
    // the fight would otherwise be stacked in the same 30px-row window.
    this.historyUI = typeof createHistoryUI === "function"
      ? createHistoryUI({
          onOpenFight: (record) => {
            const inPlace = () => {
              this.historyUI?.close?.();
              this.detailsUI?.openHistoryFight?.(record);
            };
            const fightId = record?.id ? String(record.id) : "";
            if (!fightId) {
              inPlace();
              return;
            }
            this.openDetailsSurface({ kind: "fight", fightId }, inPlace);
          },
        })
      : null;

    this.startPolling();
    this.startWindowTitlePolling();
    this.fetchDps();
  }

  bindNativeHotkeyBridge() {
    if (this._nativeHotkeyBridgeBound) return;
    this._nativeHotkeyBridgeBound = true;

    window.addEventListener("nativeResetHotKey", () => {
      this.refreshDamageData({ reason: "native hotkey refresh" });
    });
  }

  nowMs() {
    return typeof performance !== "undefined" ? performance.now() : Date.now();
  }

  shouldSuppressRowInteractions() {
    return this.isWindowDragging || this.nowMs() < Number(this.suppressRowInteractionUntilMs || 0);
  }

  setWindowDragFreeze(active) {
    const enabled = !!active;
    document.documentElement?.classList?.toggle?.("windowDragFreeze", enabled);
    document.body?.classList?.toggle?.("windowDragFreeze", enabled);
    if (enabled) {
      try {
        const selection = window.getSelection?.();
        selection?.removeAllRanges?.();
      } catch {
        // noop
      }
    }
  }

  setMeterHoverFreeze(active) {
    const next = !!active;
    if (this.isMeterBarHovered === next) return;
    this.isMeterBarHovered = next;
    if (!next && this.deferFetchUntilHoverEnd) {
      this.deferFetchUntilHoverEnd = false;
      this.fetchDps();
    }
  }

  formatBattleTime(ms) {
    const totalMs = Number(ms);
    if (!Number.isFinite(totalMs) || totalMs <= 0) return "00:00";
    const totalSeconds = Math.floor(totalMs / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  }

  safeParseJSON(raw, fallback = {}) {
    if (typeof raw !== "string") {
      return fallback;
    }
    try {
      const value = JSON.parse(raw);
      return value && typeof value === "object" ? value : fallback;
    } catch {
      return fallback;
    }
  }

  startPolling() {
    if (this._pollTimer) return;
    this._pollTimer = setInterval(() => this.fetchDps(), this.POLL_MS);
  }

  startWindowTitlePolling() {
    if (this._windowTitleTimer) return;
    this._windowTitleTimer = setInterval(
      () => this.checkAion2WindowTitle(),
      this.WINDOW_TITLE_POLL_MS
    );
    this.checkAion2WindowTitle();
  }

  parseCharacterNameFromWindowTitle(title) {
    const trimmed = String(title ?? "").trim();
    if (!trimmed) return "";
    if (!trimmed.toLowerCase().startsWith("aion2")) return "";
    const remainder = trimmed.slice(5).trim();
    if (!remainder) return "";
    return remainder.replace(/^[|l:-]+/i, "").trim();
  }

  checkAion2WindowTitle() {
    const title = window.javaBridge?.getAion2WindowTitle?.();
    const running = typeof title === "string" && title.trim().length > 0;
    if (running !== this.aionRunning) {
      this.aionRunning = running;
      this.refreshConnectionInfo();
      this.updateConnectionStatusUi();
    }
    if (!running) return;
    this.syncCharacterNameFromGame();
    const detectedName = this.parseCharacterNameFromWindowTitle(title);
    // Act on the title only when it changes. It is not kept current (a new
    // character keeps the title of the session it was created in), so a title
    // that merely disagrees with the game's own record of who is playing is
    // stale, and acting on it every poll would reset the meter every poll.
    if (detectedName === this._lastTitleName) return;
    this._lastTitleName = detectedName;
    if (!detectedName || detectedName === this.USER_NAME) return;
    const hadPreviousName = !!this.USER_NAME;
    if (hadPreviousName) {
      // Character switch means new TCP connection — full refresh resets
      // port detection, backend data, and UI so new damage displays immediately.
      this.refreshDamageData({ reason: "character switch" });
    }
    this.setUserName(detectedName, { persist: true, syncBackend: true });
    if (this.characterNameInput && document.activeElement !== this.characterNameInput) {
      this.characterNameInput.value = detectedName;
    }
  }

  // Once the game has sent its self record the backend knows who is playing,
  // and that beats both the window title and the name remembered from last
  // time. Adopt it quietly: this is not a character switch, so none of
  // setUserName's resets apply. Returns whether the game has named the local
  // player (a name of "" is an unnamed tutorial character).
  syncCharacterNameFromGame() {
    const raw = window.javaBridge?.getConnectionInfo?.();
    const info = typeof raw === "string" ? this.safeParseJSON(raw, {}) : {};
    if (!info?.characterNameFromGame) return false;
    const name = String(info.characterName ?? "").trim();
    if (name === this.USER_NAME) return true;
    this.USER_NAME = name;
    if (this.characterNameInput && document.activeElement !== this.characterNameInput) {
      this.characterNameInput.value = name;
    }
    // A tutorial character's missing name is not worth remembering over the
    // last real one.
    if (name) {
      this.safeSetStorage(this.storageKeys.userName, name);
      const id = Number(info.localPlayerId);
      if (Number.isFinite(id) && id > 0) this.rememberLocalIdForName(name, id);
    }
    this.renderCurrentRows();
    return true;
  }

  stopPolling() {
    if (!this._pollTimer) return;
    clearInterval(this._pollTimer);
    this._pollTimer = null;
  }

  /** Called from Kotlin when the window is hidden/shown (hotkey or auto-hide). */
  _setWindowHidden(hidden) {
    this._windowHidden = !!hidden;
    if (hidden) {
      this.stopPolling();
    } else {
      this.startPolling();
    }
  }

  resetAll({ callBackend = true } = {}) {
    this.resetPending = !!callBackend;


    this.lastSnapshot = null;
    this.lastJson = null;
    this.lastTargetMode = "";
    this.lastTargetName = "";
    this.lastTargetId = 0;
    this._lastRenderedListSignature = "";
    this._lastRenderedTargetLabel = "";
    this._lastRenderedRowsSummary = null;

    this._battleTimeVisible = false;
    this._lastBattleTimeMs = null;
    this.battleTime?.reset?.();
    this.battleTime?.setVisible?.(false);

    this.pinnedDetailsRowId = null;
    this.hoveredDetailsRowId = null;
    this.setWindowDragFreeze(false);
    this.setMeterHoverFreeze(false);
    this.detailsUI?.close?.({ keepPinned: false });
    this.meterUI?.onResetMeterUi?.();
    if (this.meterTotalBar) this.meterTotalBar.style.display = "none";

    if (this.elBossName) {
      this.elBossName.textContent = this.getDefaultTargetLabel();
    }
    if (this.battleTimeRoot) {
      this.battleTimeRoot.classList.add("isVisible");
    }
    if (this.analysisStatusEl) {
      if (this._captureSuspended) {
        this.analysisStatusEl.textContent =
          this.i18n?.t?.("battleTime.suspended", "App suspended") ?? "App suspended";
      } else {
        this.analysisStatusEl.textContent =
          this.i18n?.t("battleTime.analysing", "Monitoring data...") ?? "Monitoring data...";
      }
      this.analysisStatusEl.style.display = "";
    }
    this.updateConnectionStatusUi();
    this.logDebug("Target label reset: resetAll invoked.");
    this.logDebug("Meter list reset: resetAll invoked.");
    if (callBackend) {
      window.javaBridge?.resetDps?.();
    }
  }




  initHoverTooltip() {
    const container = document.querySelector(".container");
    if (!container || this.hoverTooltipEl) return;
    const tooltip = document.createElement("div");
    tooltip.className = "hoverDetailsTooltip";
    tooltip.setAttribute("aria-hidden", "true");
    container.appendChild(tooltip);
    this.hoverTooltipEl = tooltip;
  }

  hideHoverTooltip() {
    if (!this.hoverTooltipEl) return;
    this.hoverTooltipEl.classList.remove("isVisible");
    this.hoverTooltipEl.innerHTML = "";
  }

  openHoverDetailsRow(row, event = null) {
    if (!row || this.pinnedDetailsRowId !== null || this.shouldSuppressRowInteractions()) return;
    const rowId = Number(row?.id);
    if (!Number.isFinite(rowId) || rowId <= 0) return;
    if (event && Number.isFinite(event.clientX) && Number.isFinite(event.clientY)) {
      this.hoverMousePos = { x: event.clientX, y: event.clientY };
    }
    const isSameRow = this.hoveredDetailsRowId === rowId;
    this.hoveredDetailsRowId = rowId;
    if (this.detailsUI?.isOpen?.()) {
      return;
    }
    // Skip redundant tooltip renders when still hovering the same row
    if (isSameRow && this.hoverTooltipEl?.classList.contains("isVisible")) {
      return;
    }
    this.detailsUI?.close?.({ keepPinned: false });
    this.applyHoverTooltip(row, { forceRefresh: !isSameRow });
  }

  getJobColor(job) {
    return this.jobColorMap[String(job || "")] || "";
  }

  renderHoverTooltip(details, row, rowEl) {
    if (!this.hoverTooltipEl || !rowEl) return;
    const skills = Array.isArray(details?.skills) ? details.skills.slice(0, 5) : [];
    const tooltipState = details?.state || "empty";
    const stateFallback = {
      loading: "Loading...",
      empty: "No skill data for this fight",
      error: "Could not load skills",
    }[tooltipState] || "No skill data for this fight";
    const stateText = this.i18n?.t(`details.tooltip.${tooltipState}`, stateFallback) ?? stateFallback;
    let top = 0;
    let left = 372;
    const dps = Number(row?.dps) || 0;
    const dpsText = `${this.dpsFormatter.format(dps)}${this.i18n?.t("meter.dpsSuffix", "/s") ?? "/s"}`;
    const totalDamage = Number(row?.totalDamage) || 0;
    const totalDamageText = this.dpsFormatter.format(totalDamage);

    const skillsHtml = skills
      .map((skill, index) => {
        const name = String(skill?.name || "-").replace(/</g, "&lt;").replace(/>/g, "&gt;");
        const dmg = this.dpsFormatter.format(Number(skill?.dmg) || 0);
        const theostoneNameColor = window.skillIcons?.getTheostoneNameColor?.(skill) || "";
        const skillColor = theostoneNameColor || this.getJobColor(skill?.job || row?.job);
        const skillStyle = skillColor ? ` style="color:${skillColor}"` : "";
        const iconHtml = `<img class="skillIcon isPlaceholder" alt="" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" onerror="window.skillIcons&&window.skillIcons.handleImgError&&window.skillIcons.handleImgError(this)">`;
        return `<div class="hoverDetailsTooltipSkill"><span class="idx">${index + 1}.</span><span class="name">${iconHtml}<span class="skillName"${skillStyle}>${name}</span></span><span class="dmg">${dmg}</span></div>`;
      })
      .join("");

    const tooltipName = String(row?.name || "-").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    const tooltipClassIcon = row?.job ? `<img class="hoverDetailsTooltipClassIcon" src="./assets/${row.job}.png" alt="" onerror="this.style.display='none'">` : "";
    // Wrapped rather than styled on the header, so the icon keeps its own colour.
    const tooltipNameHtml = row?.isSupporter
      ? `<span class="isSupporter">${tooltipName}</span>`
      : tooltipName;
    this.hoverTooltipEl.innerHTML = `
      <div class="hoverDetailsTooltipHeader">${tooltipClassIcon}${tooltipNameHtml}</div>
      <div class="hoverDetailsTooltipStats">
        <span>${this.i18n?.t("header.display.dps", "DPS") ?? "DPS"}: ${dpsText}</span>
        <span>${this.i18n?.t("details.stats.totalDamage", "Total Damage") ?? "Total Damage"}: ${totalDamageText}</span>
      </div>
      <div class="hoverDetailsTooltipSkills">${skillsHtml || `<div class="hoverDetailsTooltipSkill muted">${stateText}</div>`}</div>
    `;
    // Apply cached skill icons to tooltip img elements
    if (window.skillIcons?.applyIconToImage && skills.length) {
      const iconEls = this.hoverTooltipEl.querySelectorAll(".hoverDetailsTooltipSkill .skillIcon");
      iconEls.forEach((img, i) => {
        if (skills[i]) window.skillIcons.applyIconToImage(img, skills[i]);
      });
    }
    const maxLeft = Math.max(372, (this.elList?.clientWidth || 0) - (this.hoverTooltipEl.offsetWidth || 0) - 8);
    const maxTop = Math.max(8, (this.elList?.clientHeight || 0) - (this.hoverTooltipEl.offsetHeight || 0) - 8);
    left = Math.max(372, Math.min(maxLeft, left));
    top = Math.max(0, Math.min(maxTop, top));

    this.hoverTooltipEl.style.left = `${left}px`;
    this.hoverTooltipEl.style.top = `${top}px`;
    this.hoverTooltipEl.classList.add("isVisible");
  }

  applyHoverTooltip(row, { forceRefresh = false } = {}) {
    const rowId = Number(row?.id);
    if (!Number.isFinite(rowId) || rowId <= 0) return;
    const rowEl = this.elList?.querySelector?.(`.item[data-row-id="${rowId}"]`);
    if (!rowEl) return;

    const cached = this.hoverTooltipCacheByRowId.get(rowId);
    if (cached) {
      this.renderHoverTooltip(cached, row, rowEl);
      if (!forceRefresh) return;
    }

    this.renderHoverTooltip({ skills: [], state: "loading" }, row, rowEl);
    if (!forceRefresh && this.hoverTooltipPendingRowIds.has(rowId)) {
      return;
    }
    const requestSeq = (this.hoverTooltipRequestSeqByRowId.get(rowId) || 0) + 1;
    this.hoverTooltipRequestSeqByRowId.set(rowId, requestSeq);
    this.hoverTooltipPendingRowIds.add(rowId);

    this.getDetails(row, { maxSkills: 5, showSkillIcons: false })
      .then((details) => {
        this.hoverTooltipPendingRowIds.delete(rowId);
        const currentSeq = this.hoverTooltipRequestSeqByRowId.get(rowId);
        if (currentSeq !== requestSeq || this.hoveredDetailsRowId !== rowId) return;
        const lightweightDetails = { skills: Array.isArray(details?.skills) ? details.skills.slice(0, 5) : [] };
        this.hoverTooltipCacheByRowId.set(rowId, lightweightDetails);
        this.renderHoverTooltip(lightweightDetails, row, rowEl);
      })
      .catch((error) => {
        this.hoverTooltipPendingRowIds.delete(rowId);
        const currentSeq = this.hoverTooltipRequestSeqByRowId.get(rowId);
        if (currentSeq !== requestSeq || this.hoveredDetailsRowId !== rowId) return;
        window.javaBridge?.logToDebug?.(`Hover skill details failed: ${error?.message || error}`);
        this.renderHoverTooltip({ skills: [], state: "error" }, row, rowEl);
      });
  }




  _isAnyPanelOpen() {
    return this.settingsPanel?.classList.contains("isOpen")
      || this.historyUI?.isOpen?.()
      || this.detailsUI?.isOpen?.();
  }

  fetchDps() {
    if (this.isCollapse || this._windowHidden) return;
    if (this.isWindowDragging) {
      this.deferFetchUntilDragEnd = true;
      return;
    }
    if (this.isMeterBarHovered) {
      this.deferFetchUntilHoverEnd = true;
      return;
    }
    const now = this.nowMs();
    // Throttle meter DOM updates while a panel is open (1s instead of 100ms)
    if (this._isAnyPanelOpen()) {
      if (this._lastPanelRenderAt && now - this._lastPanelRenderAt < 1000) return;
      this._lastPanelRenderAt = now;
    } else {
      this._lastPanelRenderAt = 0;
    }
    const raw = window.dpsData?.getDpsData?.();
    // globalThis.uiDebug?.log?.("getBattleDetail", raw);

    // 값이 없으면 타이머 숨김
    if (typeof raw !== "string") {
      this._rawLastChangedAt = now;

      this._lastBattleTimeMs = null;
      this._battleTimeVisible = false;
      this.battleTime.setVisible(false);
      this.updateConnectionStatusUi();
      return;
    }

    if (raw === this.lastJson && !this.refreshPending) {
      // Staleness watchdog: if the backend JSON hasn't changed for 10+ seconds
      // but the backend IS still parsing packets, something may be stuck.
      // Force a re-process to recover from invisible backend exceptions.
      if (!this._rawLastChangedAt) this._rawLastChangedAt = now;
      const staleMs = now - this._rawLastChangedAt;
      if (staleMs > 5_000) {
        const lastParsed = Number(window.javaBridge?.getLastParsedAtMs?.());
        const parsingRecently = Number.isFinite(lastParsed) && lastParsed > 0 && (Date.now() - lastParsed) < 15_000;
        if (parsingRecently) {
          this.logDebug(`Staleness watchdog: data unchanged for ${Math.round(staleMs/1000)}s while backend parsing active — forcing re-render`);
          this.lastJson = null;
          this._rawLastChangedAt = now;
          return;
        }
      }

      const shouldBeVisible = this._battleTimeVisible && !this.isCollapse;

      this.battleTime.setVisible(shouldBeVisible);
      if (shouldBeVisible) {
        this.battleTime.update(now, this._lastBattleTimeMs);
      }

      this.updateConnectionStatusUi();
      return;
    }
    this._rawLastChangedAt = now;

    const previousTargetName = this.lastTargetName;
    const previousTargetMode = this.lastTargetMode;
    const previousTargetId = this.lastTargetId;
    const {
      rows,
      targetName,
      targetMode,
      battleTimeMs,
      targetId,
      localPlayerId,
      targetMaxHp,
      targetTotalDamage,
      targetCurrentHp,
      dungeonId,
    } = this.buildRowsFromPayload(raw);
    // Boss mode with no boss engaged: say what it is waiting for. The rows of
    // the last fight can still be on screen (the meter keeps them), so this
    // goes by the target alone. Set before anything below can return early,
    // so switching back to Boss mode shows it again on the next update.
    const waitingForBoss = targetMode === "bossTargets" && !(Number(targetId) > 0);
    if (waitingForBoss !== Boolean(this._waitingForBoss)) {
      this._waitingForBoss = waitingForBoss;
      this.updateConnectionStatusUi();
    }
    if (this.refreshPending) {
      const pendingAgeMs = Math.max(0, now - (Number(this.refreshPendingStartedAt) || 0));
      const allowFallbackResume = rows.length > 0 && pendingAgeMs >= 1000;

      if (rows.length > 0 && !allowFallbackResume) {
        return;
      }

      this.refreshPending = false;
      this.refreshPendingStartedAt = 0;

      if (rows.length === 0) {
        this.lastJson = raw;
        this.lastSnapshot = [];
        this._lastRenderedListSignature = "";
        this._lastRenderedRowsSummary = null;
        this.meterUI?.onResetMeterUi?.();
        return;
      }
    }

    this.lastJson = raw;
    this.applyLocalPlayerIdUpdate(localPlayerId, "backend local id update");
    this.updateLocalPlayerIdentity(rows);
    this._lastBattleTimeMs = battleTimeMs;
    this.lastTargetMode = targetMode;
    this.lastTargetName = targetName;
    this.lastTargetId = targetId;

    if (
      targetId !== this._lastLoggedTargetId ||
      targetMode !== this._lastLoggedTargetMode ||
      targetName !== this._lastLoggedTargetName
    ) {
      const reasons = [];
      if (targetId !== this._lastLoggedTargetId) reasons.push("targetId changed");
      if (targetMode !== this._lastLoggedTargetMode) reasons.push("mode changed");
      if (targetName !== this._lastLoggedTargetName) reasons.push("name changed");
      console.log("[Target Lock]", {
        targetId,
        targetName,
        targetMode,
        reason: reasons.join(", ") || "initial",
      });
      this._lastLoggedTargetId = targetId;
      this._lastLoggedTargetMode = targetMode;
      this._lastLoggedTargetName = targetName;
    }


    const showByServer = rows.length > 0;
    if (this.resetPending) {
      const resetAck = rows.length === 0;

      this._battleTimeVisible = false;
      this.battleTime.setVisible(false);

      if (!resetAck) {
        return;
      }

      this.resetPending = false;
    }
    const isOutOfCombat = this.isOutOfCombatState();
    // 빈값은 ui 안덮어씀
    let rowsToRender = rows;
    const listReasons = [];
    if (rows.length === 0) {
      if (this.lastSnapshot) rowsToRender = this.lastSnapshot;
      else {
        this._battleTimeVisible = false;
        this.battleTime.setVisible(false);
        this.updateConnectionStatusUi();
        return;
      }
    } else if (!isOutOfCombat) {
      this.lastSnapshot = rows;
    } else if (this.lastSnapshot) {
      const updatedSnapshot = this.updateSnapshotNicknameForUser(rows, this.lastSnapshot);
      this.lastSnapshot = updatedSnapshot;
      rowsToRender = updatedSnapshot;
    } else {
      this.lastSnapshot = rows;
      rowsToRender = rows;
      listReasons.push("idle baseline captured");
    }

    // 타이머 표시 여부
    const showByRender = rowsToRender.length > 0;
    const showBattleTime = this.BATTLE_TIME_BASIS === "server" ? showByServer : showByRender;

    let nextBattleTimeMs = battleTimeMs;
    if (
      Number.isFinite(Number(nextBattleTimeMs)) &&
      Number.isFinite(Number(this._lastBattleTimeMs)) &&
      targetId === previousTargetId &&
      Number(nextBattleTimeMs) < Number(this._lastBattleTimeMs)
    ) {
      nextBattleTimeMs = this._lastBattleTimeMs;
    }

    const eligible = showBattleTime && Number.isFinite(Number(nextBattleTimeMs));

    this._battleTimeVisible = eligible;
    const shouldBeVisible = eligible && !this.isCollapse;

    this.battleTime.setVisible(shouldBeVisible);

    if (shouldBeVisible) {
      this.battleTime.update(now, nextBattleTimeMs);
    }

    this.updateConnectionStatusUi();
    if (targetMode === "trainTargets" && this.isLocalUserIdentified()) {
      rowsToRender = rowsToRender.filter((row) => row.name === this.USER_NAME);
    }
    // render
    this.lastDungeonId = dungeonId;
    const nextTargetLabel = this.getTargetLabel({ targetId, targetName, targetMode, dungeonId });
    if (this.elBossName) {
      if (this.elBossName.textContent !== nextTargetLabel) {
        this.elBossName.textContent = nextTargetLabel;
        this.fitBossName();
      }
      this.elBossName.classList.toggle("isAllTargets", targetMode === "allTargets");
    }
    this.updateBossHpBar(targetMaxHp, targetTotalDamage, targetCurrentHp);
    if (
      nextTargetLabel !== this._lastRenderedTargetLabel ||
      previousTargetName !== targetName ||
      previousTargetMode !== targetMode
    ) {
      const reasons = [];
      if (previousTargetName !== targetName || previousTargetMode !== targetMode) {
        reasons.push("payload target changed");
      }
      if (!targetName) {
        reasons.push(`default label for mode ${targetMode || "unknown"}`);
      }
      this.logDebug(
        `Target label changed: "${this._lastRenderedTargetLabel}" -> "${nextTargetLabel}" (reason: ${reasons.join(
          "; "
        )}).`
      );
      this._lastRenderedTargetLabel = nextTargetLabel;
    }
    const rowsSummary = this.getRowsSummary(rowsToRender);
    if (rowsSummary.listSignature !== this._lastRenderedListSignature) {
      const changeReasons = this.describeRowsChange(rowsSummary, this._lastRenderedRowsSummary);
      const reasonText = [...changeReasons, ...listReasons].filter(Boolean).join("; ");
      this.logDebug(
        `Meter list changed (${rowsToRender.length} rows). reason: ${
          reasonText || "list membership changed"
        }.`
      );
      this._lastRenderedListSignature = rowsSummary.listSignature;
      this._lastRenderedRowsSummary = rowsSummary;
    }
    this.latestRowsById = new Map(rowsToRender.map((row) => [String(row.id), row]));
    this.hoverTooltipCacheByRowId.clear();
    this.updateMeterTotalBar(rowsToRender);
    this.meterUI.updateFromRows(rowsToRender);
  }

  buildRowsFromPayload(raw) {
    const payload = this.safeParseJSON(raw, {});
    const targetName = typeof payload?.targetName === "string" ? payload.targetName : "";
    const targetMode = typeof payload?.targetMode === "string" ? payload.targetMode : "";
    const targetIdRaw = payload?.targetId;
    const targetId = Number.isFinite(Number(targetIdRaw)) ? Number(targetIdRaw) : 0;
    const localPlayerIdRaw = payload?.localPlayerId;
    const localPlayerId = Number.isFinite(Number(localPlayerIdRaw))
      ? Number(localPlayerIdRaw)
      : null;

    const mapObj = payload?.map && typeof payload.map === "object" ? payload.map : {};
    const rows = this.buildRowsFromMapObject(mapObj);

    const battleTimeMsRaw = payload?.battleTime;
    const battleTimeMs = Number.isFinite(Number(battleTimeMsRaw)) ? Number(battleTimeMsRaw) : null;

    const targetMaxHp = Number.isFinite(Number(payload?.targetMaxHp)) ? Number(payload.targetMaxHp) : 0;
    const targetTotalDamage = Number.isFinite(Number(payload?.targetTotalDamage))
      ? Number(payload.targetTotalDamage)
      : 0;
    const dungeonId = Math.trunc(Number(payload?.dungeonId)) || 0;
    const targetCurrentHp = Number.isFinite(Number(payload?.targetCurrentHp))
      ? Number(payload.targetCurrentHp)
      : -1;

    return {
      rows,
      targetName,
      targetMode,
      battleTimeMs,
      targetId,
      localPlayerId,
      targetMaxHp,
      targetTotalDamage,
      targetCurrentHp,
      dungeonId,
    };
  }

  buildRowsFromMapObject(mapObj) {
    const rows = [];

    for (const [id, value] of Object.entries(mapObj || {})) {
      const numericId = Number(id);
      if (Number.isFinite(numericId) && numericId <= 0) {
        continue;
      }
      const isObj = value && typeof value === "object";

      const job = isObj ? (value.job ?? "") : "";
      const nickname = isObj ? (value.nickname ?? "") : "";
      const idText = String(id);
      const hasNickname = !!nickname && nickname !== idText;
      const isIdentifying = !hasNickname;
      const name = hasNickname ? nickname : idText;

      const dpsRaw = isObj ? value.dps : value;
      const dps = Math.trunc(Number(dpsRaw));
      const totalDamage = Math.trunc(Number(isObj ? value.amount : 0));

      // 소수점 한자리
      const contribRaw = isObj ? Number(value.damageContribution) : NaN;
      const damageContribution = Number.isFinite(contribRaw)
        ? Math.round(contribRaw * 10) / 10
        : NaN;

      if (!Number.isFinite(dps)) {
        continue;
      }

      // Combat power comes from the party roster packet, so it only exists for
      // players actually in your party; 0 means "unknown", not "zero CP".
      const combatPower = Math.trunc(Number(isObj ? value.combatPower : 0)) || 0;

      rows.push({
        id: String(id),
        name,
        job,
        dps,
        totalDamage,
        damageContribution,
        combatPower,
        isUser: name === this.USER_NAME,
        isIdentifying,
        // Resolved in Rust against a downloaded roster; the frontend only
        // renders it. Cosmetic only — it must not reach sorting or bar colour.
        isSupporter: !!(isObj && value.isSupporter) && this.showSupporterColors !== false,
      });
    }

    const dedupedByName = new Map();
    for (const row of rows) {
      const key = String(row.name ?? "");
      if (!key) continue;
      const existing = dedupedByName.get(key);
      if (!existing) {
        dedupedByName.set(key, row);
        continue;
      }
      const scoreRow = (candidate) => {
        let score = 0;
        if (candidate.job) score += 2;
        if (!candidate.isIdentifying) score += 1;
        return score;
      };
      const existingScore = scoreRow(existing);
      const nextScore = scoreRow(row);
      if (nextScore > existingScore) {
        dedupedByName.set(key, row);
        continue;
      }
      if (nextScore < existingScore) {
        continue;
      }
      const existingId = Number(existing.id);
      const nextId = Number(row.id);
      if (!Number.isFinite(existingId) || (Number.isFinite(nextId) && nextId > existingId)) {
        dedupedByName.set(key, row);
      }
    }

    return Array.from(dedupedByName.values());
  }

  isOutOfCombatState() {
    const state = this.battleTime?.getState?.();
    return state === "state-idle" || state === "state-ended";
  }

  updateSnapshotNicknameForUser(rows, snapshot) {
    if (!Array.isArray(snapshot) || snapshot.length === 0) return snapshot;
    const localId = Number(this.localPlayerId);
    if (!Number.isFinite(localId) || localId <= 0) return snapshot;
    const incoming = Array.isArray(rows)
      ? rows.find((row) => Number(row?.id) === localId && !row.isIdentifying)
      : null;
    if (!incoming) return snapshot;
    return snapshot.map((row) => {
      if (Number(row?.id) !== localId) return row;
      return {
        ...row,
        name: incoming.name,
        isIdentifying: false,
        isUser: incoming.isUser,
      };
    });
  }

  getDefaultMeterFillOpacity() {
    const raw = getComputedStyle(document.documentElement)
      .getPropertyValue("--meter-fill-opacity")
      .trim();
    const value = Number.parseFloat(raw);
    if (!Number.isFinite(value)) return 100;
    return Math.round(value * 100);
  }

  normalizeMeterOpacity(value, fallback = 100) {
    const numeric = Number(value);
    if (!Number.isFinite(numeric)) return fallback;
    return Math.min(100, Math.max(10, Math.round(numeric)));
  }

  applyMeterFillOpacity(percent, { persist } = {}) {
    const normalized = this.normalizeMeterOpacity(percent, 100);
    document.documentElement.style.setProperty("--meter-fill-opacity", String(normalized / 100));
    if (persist) {
      this.safeSetSetting(this.storageKeys.meterFillOpacity, String(normalized));
    }
  }

  getDefaultBossNameSize() {
    const raw = getComputedStyle(document.documentElement)
      .getPropertyValue("--boss-name-size")
      .trim();
    const value = Number.parseFloat(raw);
    return Number.isFinite(value) ? value : 15;
  }

  normalizeBossNameSize(value, fallback = 15) {
    const numeric = Number(value);
    if (!Number.isFinite(numeric)) return fallback;
    return Math.min(20, Math.max(8, Math.round(numeric * 2) / 2));
  }

  applyBossNameSize(px, { persist } = {}) {
    const normalized = this.normalizeBossNameSize(px, 15);
    document.documentElement.style.setProperty("--boss-name-size", `${normalized}px`);
    // fitBossName reads the computed size, so re-run it: a bigger size may now
    // overflow and need shrinking, a smaller one may have room to grow back.
    this.fitBossName();
    if (persist) {
      this.safeSetSetting(this.storageKeys.bossNameSize, String(normalized));
    }
  }

  applyWindowOpacity(percent, { persist } = {}) {
    const normalized = Math.max(0, Math.min(100, Math.round(Number(percent))));
    document.documentElement.style.setProperty("--window-opacity", String(normalized / 100));
    if (persist) {
      this.safeSetSetting(this.storageKeys.windowOpacity, String(normalized));
    }
  }

  resetAllSettings() {
    for (const key of Object.values(this.storageKeys)) {
      try {
        localStorage.removeItem(key);
      } catch (_) {}
    }
    try {
      window.javaBridge?.clearAllSettings?.();
    } catch (_) {}
    window.location.reload();
  }

  triggerDetailsFlash() {
    if (!this.detailsPanel) return;
    this.detailsPanel.classList.remove("flash");
    void this.detailsPanel.offsetWidth;
    this.detailsPanel.classList.add("flash");
    if (this._detailsFlashTimer) window.clearTimeout(this._detailsFlashTimer);
    this._detailsFlashTimer = window.setTimeout(() => {
      this.detailsPanel?.classList.remove("flash");
    }, 1000);
  }

  triggerMeterFlash() {
    const meterEl = document.querySelector(".meter");
    if (!meterEl) return;
    meterEl.classList.remove("flash");
    void meterEl.offsetWidth;
    meterEl.classList.add("flash");
    if (this._meterFlashTimer) window.clearTimeout(this._meterFlashTimer);
    this._meterFlashTimer = window.setTimeout(() => {
      meterEl.classList.remove("flash");
    }, 1000);
  }

  captureMainMeterScreenshot() {
    const meterRect = document.querySelector(".meter")?.getBoundingClientRect?.();
    if (!meterRect) return;
    const scale = window.devicePixelRatio || 1;
    const success = window.javaBridge?.captureScreenshotToClipboard?.(
      meterRect.left,
      meterRect.top,
      meterRect.width,
      meterRect.height,
      scale
    );
    if (success) {
      this.triggerMeterFlash();
    }
  }

  reinitTargetSelection(reason) {
    this.resetTargetTrackingState();
    window.javaBridge?.restartTargetSelection?.();
    this.refreshPending = false;
    this.refreshPendingStartedAt = 0;
    this.resetPending = false;
    this.lastJson = null;
    this.lastSnapshot = null;
    this._lastRenderedListSignature = "";
    this._lastRenderedRowsSummary = null;
    this.setTargetSelection(this.targetSelection, { persist: false, syncBackend: true, reason });
    if (!this.isCollapse) {
      this.fetchDps();
    }
  }

  resetTargetTrackingState() {
    this.lastTargetMode = "";
    this.lastTargetName = "";
    this.lastTargetId = 0;
    this._lastRenderedTargetLabel = "";
    this._lastLoggedTargetId = null;
    this._lastLoggedTargetMode = null;
    this._lastLoggedTargetName = null;
  }

  updateLocalPlayerIdentity(rows = []) {
    if (!Array.isArray(rows) || !rows.length || !this.USER_NAME) {
      return;
    }
    // Stay sticky: if our current local id is still an active row, keep it. The
    // canonical id for the local player can flip between co-existing self-ids as
    // damage accumulates; re-binding on every flip churned the binding needlessly.
    if (this.localPlayerId && rows.some((row) => Number(row?.id) === this.localPlayerId)) {
      return;
    }
    const matched = rows.find((row) => row?.name === this.USER_NAME);
    if (!matched) {
      return;
    }
    const actorId = Number(matched.id);
    this.applyLocalPlayerIdUpdate(actorId, "local id update");
  }

  applyLocalPlayerIdUpdate(actorId, reason) {
    if (!Number.isFinite(actorId) || actorId <= 0) {
      return;
    }
    if (this.localPlayerId === actorId) {
      return;
    }
    this.localPlayerId = actorId;
    window.javaBridge?.bindLocalActorId?.(String(actorId));
    window.javaBridge?.setLocalPlayerId?.(String(actorId));
    // When the game has named the local player the backend already holds the
    // right name; pushing ours could stamp another character's on this row.
    if (this.USER_NAME && !this.syncCharacterNameFromGame()) {
      window.javaBridge?.bindLocalNickname?.(String(actorId), this.USER_NAME);
      this.setUserName(this.USER_NAME, { persist: true, syncBackend: true });
      this.rememberLocalIdForName(this.USER_NAME, actorId);
    }
    if (this.localActorIdInput && document.activeElement !== this.localActorIdInput) {
      this.localActorIdInput.value = String(actorId);
    }
    this.refreshConnectionInfo({ skipSettingsRefresh: true });
    // NOTE: do NOT reinitTargetSelection() here. Which entity id is "you" can
    // change several times within one fight (the local player churns entity ids,
    // e.g. after casting Divine Aura), and reinit → restartTargetSelection →
    // reset_combat wipes ALL combat data and every teammate nickname. That fired
    // on every poll, causing the meter to reset over and over (PC lag), teammates
    // to flip to raw ids, and skills like Divine Aura to be dropped. Re-labelling
    // who "you" are is a display concern only — the next dps poll re-renders the
    // highlight from the updated localPlayerId with no reset.
  }

  rememberLocalIdForName(name, actorId) {
    const key = String(name ?? "").trim().toLowerCase();
    if (!key || !Number.isFinite(actorId) || actorId <= 0) return;
    this._recentLocalIdByName.set(key, { actorId, timestamp: Date.now() });
  }

  getRecentLocalIdForName(name) {
    const key = String(name ?? "").trim().toLowerCase();
    if (!key) return null;
    const entry = this._recentLocalIdByName.get(key);
    if (!entry) return null;
    if (Date.now() - entry.timestamp > 120000) {
      this._recentLocalIdByName.delete(key);
      return null;
    }
    return entry.actorId;
  }

  getDetailsContext() {
    const raw = window.dpsData?.getDetailsContext?.();
    if (!raw) return null;
    if (typeof raw === "string") {
      return this.safeParseJSON(raw, null);
    }
    return raw;
  }

  async getDetails(
    row,
    { targetId = null, attackerIds = null, totalTargetDamage = null, showSkillIcons = false, maxSkills = null } = {}
  ) {
    let raw = null;
    let backendFiltered = false;
    if (window._historyDetailsOverride) {
      raw = window._historyDetailsOverride;
    } else if (targetId && window.dpsData?.getTargetDetails) {
      const payload = Array.isArray(attackerIds) ? JSON.stringify(attackerIds) : "";
      raw = await window.dpsData.getTargetDetails(targetId, payload);
      backendFiltered = true;
    } else {
      raw = await window.dpsData?.getBattleDetail?.(row.id);
    }
    let detailObj = raw;
    // globalThis.uiDebug?.log?.("getBattleDetail", detailObj);

    if (typeof raw === "string") detailObj = this.safeParseJSON(raw, {});
    if (!detailObj || typeof detailObj !== "object") detailObj = {};

    const skills = [];
    let totalDmg = 0;

    let totalTimes = 0;
    let totalCrit = 0;
    let totalParry = 0;
    let totalBack = 0;
    let totalPerfect = 0;
    let totalDouble = 0;
    let totalMultiHitCount = 0;
    let totalMultiHitDamage = 0;
    let totalMultiHitHits = 0;
    let totalRegen = 0;
    let totalSmite = 0;
    let totalPowershard = 0;

    const pushSkill = ({
      codeKey,
      name,
      time,
      dmg,
      crit = 0,
      parry = 0,
      back = 0,
      frontal = 0,
      perfect = 0,
      double = 0,
      smite = 0,
      powershard = 0,
      regen = 0,
      multiHitCount = 0,
      multiHitDamage = 0,
      multiHitHits = 0,
      minDmg = 0,
      maxDmg = 0,
      countForTotals = true,
      job = "",
      actorId = null,
      isDot = false,
      hitTimestamps = null,
      specs = null,
    }) => {
      const dmgInt = Math.trunc(Number(String(dmg ?? "").replace(/,/g, ""))) || 0;
      if (dmgInt <= 0) {
        return;
      }

      const t = Number(time) || 0;

      totalDmg += dmgInt;
      totalRegen += Number(regen) || 0;
      if (countForTotals) {
        totalTimes += t;
        totalCrit += Number(crit) || 0;
        totalParry += Number(parry) || 0;
        totalBack += Number(back) || 0;
        totalPerfect += Number(perfect) || 0;
        totalDouble += Number(double) || 0;
        totalSmite += Number(smite) || 0;
        totalPowershard += Number(powershard) || 0;
        totalMultiHitCount += Number(multiHitCount) || 0;
        totalMultiHitDamage += Number(multiHitDamage) || 0;
        totalMultiHitHits += Number(multiHitHits) || 0;
      }
      skills.push({
        code: String(codeKey),
        name,
        time: t,
        crit: Number(crit) || 0,
        parry: Number(parry) || 0,
        back: Number(back) || 0,
        frontal: Number(frontal) || 0,
        perfect: Number(perfect) || 0,
        double: Number(double) || 0,
        smite: Number(smite) || 0,
        powershard: Number(powershard) || 0,
        regen: Number(regen) || 0,
        multiHitCount: Number(multiHitCount) || 0,
        multiHitDamage: Number(multiHitDamage) || 0,
        multiHitHits: Number(multiHitHits) || 0,
        minDmg: Number(minDmg) || 0,
        maxDmg: Number(maxDmg) || 0,
        dmg: dmgInt,
        job,
        actorId,
        isDot,
        hitTimestamps: Array.isArray(hitTimestamps) ? hitTimestamps : [],
        specs: Array.isArray(specs) ? specs : null,
      });
    };

    const detailSkills = Array.isArray(detailObj?.skills) ? detailObj.skills : null;
    const attackerIdSet = !backendFiltered && Array.isArray(attackerIds) && attackerIds.length > 0
      ? new Set(attackerIds.map(Number))
      : null;
    if (detailSkills) {
      for (const value of detailSkills) {
        if (!value || typeof value !== "object") continue;
        // Filter by selected player when viewing history (skip when backend already filtered)
        if (attackerIdSet) {
          const skillActorId = Number(value.actorId);
          if (!Number.isFinite(skillActorId) || !attackerIdSet.has(skillActorId)) continue;
        }
        const code = String(value.code ?? "");
        const nameRaw = typeof value.name === "string" ? value.name.trim() : "";
        const translatedName = (this.i18n?.getSkillName?.(code, nameRaw) ?? nameRaw).replace(/^Theostone:/, "Theo:");
        const baseName =
          translatedName ||
          this.i18n?.format?.("skills.fallback", { code }, `Skill ${code}`) ||
          `Skill ${code}`;
        const dotName =
          this.i18n?.format?.("skills.dot", { name: baseName }, `${baseName} - DOT`) ||
          `${baseName} - DOT`;
        const isDot = !!value.isDot;
        const actorId = Number(value.actorId);

        pushSkill({
          codeKey: isDot ? `${code}-dot` : code,
          name: isDot ? dotName : baseName,
          time: value.time,
          dmg: value.dmg,
          crit: value.crit,
          parry: value.parry,
          back: value.back,
          frontal: value.frontal,
          perfect: value.perfect,
          double: value.double,
          smite: value.smite,
          powershard: value.powershard,
          regen: value.regen,
          multiHitCount: value.multiHitCount,
          multiHitDamage: value.multiHitDamage,
          multiHitHits: value.multiHitHits,
          minDmg: value.minDmg,
          maxDmg: value.maxDmg,
          job: value.job ?? "",
          countForTotals: !isDot,
          actorId: Number.isFinite(actorId) ? actorId : null,
          isDot,
          hitTimestamps: Array.isArray(value.hitTimestamps) ? value.hitTimestamps : [],
          specs: Array.isArray(value.specs) ? value.specs : null,
        });
      }
    } else {
      for (const [code, value] of Object.entries(detailObj)) {
        if (!value || typeof value !== "object") continue;

        const nameRaw = typeof value.skillName === "string" ? value.skillName.trim() : "";
        const translatedName = (this.i18n?.getSkillName?.(code, nameRaw) ?? nameRaw).replace(/^Theostone:/, "Theo:");
        const baseName =
          translatedName ||
          this.i18n?.format?.("skills.fallback", { code }, `Skill ${code}`) ||
          `Skill ${code}`;
        const dotName =
          this.i18n?.format?.("skills.dot", { name: baseName }, `${baseName} - DOT`) ||
          `${baseName} - DOT`;

        // 일반 피해
        pushSkill({
          codeKey: code,
          name: baseName,
          time: value.times,
          dmg: value.damageAmount,
          crit: value.critTimes,
          parry: value.parryTimes,
          back: value.backTimes,
          frontal: value.frontalTimes,
          perfect: value.perfectTimes,
          double: value.doubleTimes,
          regen: value.healAmount,
        });

        // 도트피해
        if (Number(String(value.dotDamageAmount ?? "").replace(/,/g, "")) > 0) {
          pushSkill({
            codeKey: `${code}-dot`, // 유니크키
            name: dotName,
            time: value.dotTimes,
            dmg: value.dotDamageAmount,
            countForTotals: false,
          });
        }
      }
    }


    if (Number.isFinite(Number(maxSkills)) && Number(maxSkills) > 0 && skills.length > Number(maxSkills)) {
      skills.sort((a, b) => (Number(b?.dmg) || 0) - (Number(a?.dmg) || 0));
      skills.length = Number(maxSkills);
    }
    const pct = (num, den) => {
      if (den <= 0) return 0;
      return Math.round((num / den) * 1000) / 10;
    };
    const perActorStatsMap = new Map();
    for (const skill of skills) {
      if (!Number.isFinite(Number(skill.actorId))) continue;
      const actorId = Number(skill.actorId);
      const entry =
        perActorStatsMap.get(actorId) || {
          actorId,
          job: skill.job ?? "",
          totalDmg: 0,
          totalTimes: 0,
          totalCrit: 0,
          totalParry: 0,
          totalBack: 0,
          totalPerfect: 0,
          totalDouble: 0,
          totalHits: 0,
          totalSmite: 0,
          totalPowershard: 0,
          totalRegen: 0,
          multiHitCount: 0,
          multiHitDamage: 0,
        };
      entry.totalDmg += Number(skill.dmg) || 0;
      entry.totalRegen += Number(skill.regen) || 0;
      entry.multiHitCount += Number(skill.multiHitCount) || 0;
      entry.multiHitDamage += Number(skill.multiHitDamage) || 0;
      if (!skill.isDot) {
        entry.totalHits += Number(skill.time) || 0;
        entry.totalTimes += Number(skill.time) || 0;
        entry.totalCrit += Number(skill.crit) || 0;
        entry.totalParry += Number(skill.parry) || 0;
        entry.totalBack += Number(skill.back) || 0;
        entry.totalPerfect += Number(skill.perfect) || 0;
        entry.totalDouble += Number(skill.double) || 0;
        entry.totalSmite += Number(skill.smite) || 0;
        entry.totalPowershard += Number(skill.powershard) || 0;
      }
      if (!entry.job && skill.job) {
        entry.job = skill.job;
      }
      perActorStatsMap.set(actorId, entry);
    }
    const fallbackContribution = Number(row?.damageContribution);
    const baseTotalDamage = Number.isFinite(Number(totalTargetDamage))
      ? Number(totalTargetDamage)
      : Number(detailObj?.totalTargetDamage);
    const contributionPct =
      Number.isFinite(baseTotalDamage) && baseTotalDamage > 0
        ? (totalDmg / baseTotalDamage) * 100
        : fallbackContribution;
    const battleTimeMsRaw = Number(detailObj?.battleTime);
    const combatTime = Number.isFinite(battleTimeMsRaw)
      ? this.formatBattleTime(battleTimeMsRaw)
      : this.battleTime?.getCombatTimeText?.() ?? "00:00";

    const perActorStats = [...perActorStatsMap.values()]
      .map((entry) => ({
        actorId: entry.actorId,
        job: entry.job,
        totalDmg: entry.totalDmg,
        totalTimes: entry.totalTimes,
        totalCrit: entry.totalCrit,
        totalParry: entry.totalParry,
        totalBack: entry.totalBack,
        totalPerfect: entry.totalPerfect,
        totalDouble: entry.totalDouble,
        totalSmite: entry.totalSmite,
        totalPowershard: entry.totalPowershard,
        totalHits: entry.totalHits,
        totalRegen: entry.totalRegen,
        multiHitCount: entry.multiHitCount,
        multiHitDamage: entry.multiHitDamage,
        contributionPct:
          Number.isFinite(baseTotalDamage) && baseTotalDamage > 0
            ? (entry.totalDmg / baseTotalDamage) * 100
            : 0,
        totalCritPct: pct(entry.totalCrit, entry.totalTimes),
        totalParryPct: pct(entry.totalParry, entry.totalTimes),
        totalBackPct: pct(entry.totalBack, entry.totalTimes),
        totalPerfectPct: pct(entry.totalPerfect, entry.totalTimes),
        totalDoublePct: pct(entry.totalDouble, entry.totalTimes),
        totalSmitePct: pct(entry.totalSmite, entry.totalTimes),
        totalPowershardPct: pct(entry.totalPowershard, entry.totalTimes),
        combatTime,
      }))
      .sort((a, b) => b.totalDmg - a.totalDmg);

    // Process healing (DMG/HEAL toggle): filter by selected member, resolve skill
    // names, and compute heal-mode stat totals. Mirrors the damage skill pipeline.
    const healSkillsRaw = Array.isArray(detailObj?.healSkills) ? detailObj.healSkills : [];
    const healSkillsOut = [];
    let totalHeal = 0;
    let healTicks = 0;
    let healHotTicks = 0;
    const healActors = new Set();
    for (const v of healSkillsRaw) {
      if (!v || typeof v !== "object") continue;
      const aId = Number(v.actorId);
      if (attackerIdSet && (!Number.isFinite(aId) || !attackerIdSet.has(aId))) continue;
      const code = String(v.code ?? "");
      const nameRaw = typeof v.name === "string" ? v.name.trim() : "";
      const name = (this.i18n?.getSkillName?.(code, nameRaw) ?? nameRaw) || `Skill ${code}`;
      const amt = Number(v.dmg) || 0;
      const ticks = Number(v.time) || 0;
      const isHot = !!v.isDot;
      if (amt <= 0) continue;
      totalHeal += amt;
      healTicks += ticks;
      if (isHot) healHotTicks += ticks;
      if (Number.isFinite(aId)) healActors.add(aId);
      healSkillsOut.push({
        actorId: Number.isFinite(aId) ? aId : null,
        code: Number.isFinite(Number(code)) ? Number(code) : v.code,
        name,
        dmg: amt,
        time: ticks,
        isDot: isHot,
        crit: 0, parry: 0, back: 0, frontal: 0, perfect: 0, double: 0, smite: 0, powershard: 0,
        regen: 0, multiHitCount: 0, multiHitDamage: 0, multiHitHits: 0,
        minDmg: 0, maxDmg: 0, job: v.job ?? "", specs: null, hitTimestamps: [],
      });
    }
    const healBattleMs = Number.isFinite(battleTimeMsRaw) ? battleTimeMsRaw : 0;
    const healPerSecText = healBattleMs > 0
      ? `${this.formatAbbreviatedNumber(totalHeal / healBattleMs * 1000)}`
      : "-";

    return {
      totalDmg,
      totalHeal,
      healTicks,
      healHotTicks,
      healSkillCount: healSkillsOut.length,
      healPerSecText,
      contributionPct,
      totalCritPct: pct(totalCrit, totalTimes),
      totalParryPct: pct(totalParry, totalTimes),
      totalBackPct: pct(totalBack, totalTimes),
      totalPerfectPct: pct(totalPerfect, totalTimes),
      totalDoublePct: pct(totalDouble, totalTimes),
      totalSmitePct: pct(totalSmite, totalTimes),
      totalPowershardPct: pct(totalPowershard, totalTimes),
      totalHits: totalTimes,
      multiHitCount: totalMultiHitCount,
      multiHitDamage: totalMultiHitDamage,
      multiHitPct: totalTimes > 0 ? Math.round((totalMultiHitHits / totalTimes) * 1000) / 10 : 0,
      totalRegen,
      combatTime,
      battleTimeMs: Number.isFinite(battleTimeMsRaw) ? battleTimeMsRaw : 0,
      maxHp: Number(detailObj?.maxHp) || 0,

      skills,
      // Per-actor/skill healing (DMG/HEAL toggle), filtered to the selected member
      // and name-resolved (see processing above).
      healSkills: healSkillsOut,
      showSkillIcons,
      perActorStats,
      showCombinedTotals: !attackerIds || attackerIds.length === 0,
      pingHistory: Array.isArray(detailObj?.pingHistory) ? detailObj.pingHistory : [],
    };
  }

  bindHeaderButtons() {
    this.logoBtn = document.querySelector(".bossIcon");
    this.collapseBtn?.addEventListener("click", () => {
      this.listSortDirection = this.listSortDirection === "asc" ? "desc" : "asc";
      this.renderCurrentRows();

      const iconName =
        this.listSortDirection === "asc" ? "arrow-down-wide-narrow" : "arrow-up-wide-narrow";
      const iconEl =
        this.collapseBtn.querySelector("svg") || this.collapseBtn.querySelector("[data-lucide]");
      if (!iconEl) {
        return;
      }

      iconEl.setAttribute("data-lucide", iconName);
      window.lucide?.createIcons?.({ root: this.collapseBtn });
    });
    this.resetBtn?.addEventListener("click", () => {
      this.refreshDamageData({ reason: "manual refresh" });
    });
    this.suspendBtn?.addEventListener("click", () => {
      this._setCaptureSuspended(!this._captureSuspended);
    });
    this.lockBtn?.addEventListener("click", () => {
      this._setOverlayLocked(!this._overlayLocked);
    });
    this.targetModeBtn?.addEventListener("click", () => {
      const modes = ["lastHitByMe", "bossTargets", "trainTargets", "allTargets"];
      const currentIndex = modes.indexOf(this.targetSelection);
      const nextMode = modes[(currentIndex + 1) % modes.length];
      console.log("[Target Mode Toggle]", {
        from: this.targetSelection,
        to: nextMode,
      });
      this.setTargetSelection(nextMode, {
        persist: true,
        syncBackend: true,
        reason: "header toggle",
      });
      if (!this.isCollapse) {
        this.fetchDps();
      }
    });
    this.metricToggleBtn?.addEventListener("click", () => {
      // DPS -> total damage -> both -> DPS.
      const order = ["dps", "totalDamage", "both"];
      const nextMode = order[(order.indexOf(this.displayMode) + 1) % order.length];
      this.setDisplayMode(nextMode, { persist: true });
      this.renderCurrentRows();
    });
    this.logoBtn?.addEventListener("click", () => {
      this.captureMainMeterScreenshot();
    });
    this.logoBtn?.setAttribute("data-no-drag", "true");

    // Click on boss name area → open Details for current mob (all players) or history
    // The target name is a plain label now: no click target and no data-no-drag,
    // so the whole top of the window is grab-and-drag surface. Details is opened
    // from a player row, History from its own header button.

    const historyBtn = document.querySelector(".historyBtn");
    if (historyBtn) {
      // History is part of the Details surface, not the overlay: opening it
      // here would make the 30px-tall meter grow to swallow a fight list. It
      // goes to the Details window, which then swaps between the list and a
      // fight in place. The in-overlay panel remains the fallback for when
      // that window cannot be reached at all.
      const openHistory = () => {
        this.openDetailsSurface({ kind: "history" }, () => this.historyUI?.open?.());
      };
      historyBtn.addEventListener("click", () => {
        if (this.isWindowDragging) return;
        openHistory();
      });
      historyBtn.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          openHistory();
        }
      });
      historyBtn.setAttribute("data-no-drag", "true");
    }
  }

  setAccountState(text) {
    if (this.accountStateEl) this.accountStateEl.textContent = text;
  }

  // Reflects whatever the backend reports. Called on open, after sign-out, and
  // from the "account-changed" event the device-grant poll emits when the
  // player approves or declines in the browser.
  async refreshAccountPanel(result) {
    if (!this.accountStateEl) return;
    if (result && result.connected === false && result.error) {
      const msg = String(result.error);
      this.setAccountState(
        window.i18n?.format?.("settings.account.failed", { error: msg }, `Sign-in failed: ${msg}`)
      );
      if (this.accountCodeBox) this.accountCodeBox.style.display = "none";
      if (this.accountConnectBtn) this.accountConnectBtn.disabled = false;
      return;
    }

    // The last known answer first, so the panel is right the moment it opens;
    // the server's can take seconds, and nothing else waits on it.
    if (!result) {
      try {
        const seen = await window.javaBridge?.accountStatusCached?.();
        if (seen) this.paintAccount(seen.who);
      } catch {}
    }

    let who = null;
    try {
      who = await window.javaBridge?.accountStatus?.();
    } catch {
      who = null;
    }
    this.paintAccount(who);
  }

  paintAccount(who) {
    const signedIn = !!who;
    // A sign-in waiting for approval in the browser keeps its code and its
    // "waiting" line; its outcome arrives on "account-changed".
    if (!signedIn && this.accountCodeBox?.style.display === "block") return;
    if (this.accountCodeBox && signedIn) this.accountCodeBox.style.display = "none";
    if (this.accountConnectBtn) {
      this.accountConnectBtn.disabled = false;
      this.accountConnectBtn.style.display = signedIn ? "none" : "";
    }
    if (this.accountSignOutBtn) {
      this.accountSignOutBtn.style.display = signedIn ? "" : "none";
    }

    if (!signedIn) {
      this.setAccountState(window.i18n?.t?.("settings.account.signedOut", "Not signed in"));
      return;
    }
    const name = who.displayName || who.display_name || "";
    let label = window.i18n?.format?.(
      "settings.account.signedIn",
      { name },
      `Signed in as ${name}`
    );
    if (who.supporter) {
      label += " · " + window.i18n?.t?.("settings.account.supporter", "Supporter");
    }
    this.setAccountState(label);
  }

  setupSettingsPanel() {
    this.settingsPanel = document.querySelector(".settingsPanel");
    this.settingsClose = document.querySelector(".settingsClose");
    this.settingsBtn = document.querySelector(".settingsBtn");
    this.lockedIp = document.querySelector(".lockedIp");
    this.lockedPort = document.querySelector(".lockedPort");
    this.localActorIdInput = document.querySelector(".localActorIdInput");
    this.allTargetsWindowDropdownBtn = document.querySelector(".allTargetsWindowDropdownBtn");
    this.allTargetsWindowDropdownMenu = document.querySelector(".allTargetsWindowDropdownMenu");
    this.targetWindowDropdownBtn = document.querySelector(".targetWindowDropdownBtn");
    this.targetWindowDropdownMenu = document.querySelector(".targetWindowDropdownMenu");
    this.trainSelectionModeDropdownBtn = document.querySelector(".trainSelectionModeDropdownBtn");
    this.trainSelectionModeDropdownMenu = document.querySelector(".trainSelectionModeDropdownMenu");
    this.defaultMeterModeDropdownBtn = document.querySelector(".defaultMeterModeDropdownBtn");
    this.defaultMeterModeDropdownMenu = document.querySelector(".defaultMeterModeDropdownMenu");
    this.resetDetectBtn = document.querySelector(".resetDetectBtn");
    this.autoDetectDeviceCheckbox = document.querySelector(".autoDetectDeviceCheckbox");
    this.deviceDropdownBtn = document.querySelector(".deviceDropdownBtn");
    this.deviceDropdownMenu = document.querySelector(".deviceDropdownMenu");
    this.characterNameInput = document.querySelector(".characterNameInput");
    this.showSuspendBtnCheckbox = document.querySelector(".showSuspendBtnCheckbox");
    this.bossLogsCheckbox = document.querySelector(".bossLogsCheckbox");
    this.debugLoggingCheckbox = document.querySelector(".debugLoggingCheckbox");
    this.showPingCheckbox = document.querySelector(".showPingCheckbox");
    this.saveRawPacketsCheckbox = document.querySelector(".saveRawPacketsCheckbox");
    this.sendLogsBtn = document.querySelector(".sendLogsBtn");
    this.sendLogsStatus = document.querySelector(".sendLogsStatus");
    this.pinMeToTopCheckbox = document.querySelector(".pinMeToTopCheckbox");
    this.detailsMonitorDropdownBtn = document.querySelector(".detailsMonitorDropdownBtn");
    this.detailsMonitorDropdownMenu = document.querySelector(".detailsMonitorDropdownMenu");
    this.detailsMonitorHint = document.querySelector(".detailsMonitorHint");
    this.meterLayoutDropdownBtn = document.querySelector(".meterLayoutDropdownBtn");
    this.meterLayoutDropdownMenu = document.querySelector(".meterLayoutDropdownMenu");
    this.playerNamesBoldCheckbox = document.querySelector(".playerNamesBoldCheckbox");
    this.showSupporterColorsCheckbox = document.querySelector(".showSupporterColorsCheckbox");
    this.autoUploadCheckbox = document.querySelector(".autoUploadCheckbox");
    this.accountStateEl = document.querySelector(".accountState");
    this.accountHintEl = document.querySelector(".accountHint");
    this.accountConnectBtn = document.querySelector(".accountConnectBtn");
    this.accountSignOutBtn = document.querySelector(".accountSignOutBtn");
    this.accountCodeBox = document.querySelector(".accountCodeBox");
    this.accountCodeEl = document.querySelector(".accountCode");
    this.playerDpsBoldCheckbox = document.querySelector(".playerDpsBoldCheckbox");
    this.meterOpacityInput = document.querySelector(".meterOpacityInput");
    this.meterOpacityValue = document.querySelector(".meterOpacityValue");
    this.bossNameSizeInput = document.querySelector(".bossNameSizeInput");
    this.bossNameSizeValue = document.querySelector(".bossNameSizeValue");
    this.windowOpacityInput = document.querySelector(".windowOpacityInput");
    this.windowOpacityValue = document.querySelector(".windowOpacityValue");
    this.discordButton = document.querySelector(".discordButton");
    this.supportWidget = document.querySelector(".supportWidget");
    this.supportButton = document.querySelector(".supportButton");
    this.supportModal = document.querySelector("#supportModal");
    this.supportModalTitle = document.querySelector("#supportModalTitle");
    this.supportModalClose = document.querySelector(".supportModalClose");
    this.supportQrImage = document.querySelector(".supportQrImage");
    this.supportPrimaryButton = document.querySelector(".supportPrimaryButton");
    this.supportCopyStatus = document.querySelector(".supportCopyStatus");
    this.supportActionButtons = Array.from(document.querySelectorAll(".supportIconButton"));
    this.kofiButton = document.querySelector(".kofiButton");
    this.kofiWidget = document.querySelector(".kofiWidget");
    this.quitButton = document.querySelector(".quitButton");
    this.settingsVersionValue = document.querySelector(".settingsVersionValue");
    this.settingsVersionLink = document.querySelector(".settingsVersionLink");
    this.languageDropdownBtn = document.querySelector(".languageDropdownBtn");
    this.languageDropdownMenu = document.querySelector(".languageDropdownMenu");
    this.themeDropdownBtn = document.querySelector(".themeDropdownBtn");
    this.themeDropdownMenu = document.querySelector(".themeDropdownMenu");
    this.settingsSelections = {
      language: "en",
      theme: this.theme,
      defaultMeterMode: "bossTargets",
      allTargetsWindowMs: "120000",
      trainSelectionMode: "all",
      targetSelectionWindowMs: "5000",
    };

    const storedName = this.safeGetStorage(this.storageKeys.userName) || "";
    const storedAllTargetsWindowMs = this.safeGetSetting(this.storageKeys.allTargetsWindowMs) ||
      this.safeGetStorage(this.storageKeys.allTargetsWindowMs) ||
      "120000";
    const storedTrainSelectionMode = this.safeGetSetting(this.storageKeys.trainSelectionMode) ||
      this.safeGetStorage(this.storageKeys.trainSelectionMode) ||
      "all";
    const storedTargetSelectionWindowMs = this.safeGetSetting(this.storageKeys.targetSelectionWindowMs) ||
      this.safeGetStorage(this.storageKeys.targetSelectionWindowMs) ||
      "5000";
    let storedMeterOpacity = this.safeGetSetting(this.storageKeys.meterFillOpacity) ||
      this.safeGetStorage(this.storageKeys.meterFillOpacity);
    if (this.safeGetSetting("dpsMeter.migration.opacityReset1") !== "done") {
      storedMeterOpacity = "80";
      this.safeSetSetting(this.storageKeys.meterFillOpacity, "80");
      this.safeSetSetting("dpsMeter.migration.opacityReset1", "done");
    }
    const storedDebugLogging = this.safeGetSetting(this.storageKeys.debugLogging) === "true";
    const storedPinMeToTop = this.safeGetSetting(this.storageKeys.pinMeToTop) === "true";
    // Default on: a supporter's gold name is the thing they paid for, so it
    // should be visible unless a viewer has deliberately turned it off.
    this.showSupporterColors =
      this.safeGetSetting(this.storageKeys.showSupporterColors) !== "false";
    const mainPlayerNamesBoldSetting = this.safeGetSetting(this.storageKeys.mainPlayerNamesBold);
    const storedMainPlayerNamesBold = mainPlayerNamesBoldSetting !== "false";
    const mainPlayerDpsBoldSetting = this.safeGetSetting(this.storageKeys.mainPlayerDpsBold);
    const storedMainPlayerDpsBold = mainPlayerDpsBoldSetting !== "false";
    const storedDefaultMeterMode = this.safeGetSetting(this.storageKeys.defaultMeterMode) || "bossTargets";
    const storedTargetSelection = this.safeGetStorage(this.storageKeys.targetSelection);
    const storedLanguage = this.safeGetStorage(this.storageKeys.language);
    const storedTheme = this.safeGetSetting(this.storageKeys.theme);

    this.setUserName(storedName, { persist: false, syncBackend: true });
    this.setOnlyShowUser(false, { persist: false });
    this.setDebugLogging(storedDebugLogging, { persist: false, syncBackend: true });
    this.setPinMeToTop(storedPinMeToTop, { persist: false });
    this.setBetaUi(this.safeGetSetting(this.storageKeys.betaUi) !== "false", { persist: false });
    const storedSlimMode = this.safeGetSetting(this.storageKeys.slimMode) === "true";
    this.setSlimMode(storedSlimMode, { persist: false });
    this.setMainPlayerNamesBold(storedMainPlayerNamesBold, { persist: false });
    this.setMainPlayerDpsBold(storedMainPlayerDpsBold, { persist: false });
    if (mainPlayerNamesBoldSetting === null || mainPlayerNamesBoldSetting === undefined || mainPlayerNamesBoldSetting === "") {
      this.safeSetSetting(this.storageKeys.mainPlayerNamesBold, "true");
    }
    if (mainPlayerDpsBoldSetting === null || mainPlayerDpsBoldSetting === undefined || mainPlayerDpsBoldSetting === "") {
      this.safeSetSetting(this.storageKeys.mainPlayerDpsBold, "true");
    }
    const validModes = ["bossTargets", "lastHitByMe", "allTargets", "trainTargets"];
    const normalizedDefaultMode = validModes.includes(storedDefaultMeterMode)
      ? storedDefaultMeterMode : "bossTargets";
    this.settingsSelections.defaultMeterMode = normalizedDefaultMode;
    this.setTargetSelection(normalizedDefaultMode, {
      persist: false,
      syncBackend: true,
      reason: "default meter mode setting",
    });
    this.applyTheme(storedTheme || this.theme, { persist: false });
    if (storedLanguage) {
      this.i18n?.setLanguage?.(storedLanguage, { persist: false });
    }

    if (this.characterNameInput) {
      this.characterNameInput.value = this.USER_NAME;
      // The player's own word on their name: saved when they leave the field
      // or press Enter, sent as a manual change (the backend takes it even
      // after the game has named a character, unlike a remembered name), and
      // broadcast so the meter window adopts it too. The game's own record of
      // who is playing still replaces it on the next zone change if it differs.
      this.characterNameInput.addEventListener("keydown", (event) => {
        if (event.key === "Enter") event.target.blur();
      });
      // Saved after a pause in typing, on Enter or leaving the field, and when
      // the window closes: saving on "change" alone lost a name typed just
      // before closing Settings (issue #13). Compared with what was last
      // saved, not with the detected name, so typing the detected name still
      // saves it as the player's own.
      let savedName = String(this.safeGetSetting(this.storageKeys.userName) || "").trim();
      let saveTimer = null;
      const saveTypedName = () => {
        clearTimeout(saveTimer);
        saveTimer = null;
        const name = String(this.characterNameInput.value || "").trim();
        if (name === savedName) return;
        savedName = name;
        this.setUserName(name, { persist: true, syncBackend: true, manual: true });
        this.safeSetSetting(this.storageKeys.userName, name);
      };
      this.characterNameInput.addEventListener("input", () => {
        clearTimeout(saveTimer);
        saveTimer = setTimeout(saveTypedName, 800);
      });
      this.characterNameInput.addEventListener("change", (event) => {
        event.target.value = String(event.target?.value || "").trim();
        saveTypedName();
      });
      window.addEventListener("pagehide", () => {
        if (saveTimer) saveTypedName();
      });
      window.addEventListener("beforeunload", () => {
        if (saveTimer) saveTypedName();
      });
    }
    if (this.localActorIdInput) {
      this.localActorIdInput.value = this.localPlayerId ? String(this.localPlayerId) : "";
      this.localActorIdInput.addEventListener("input", (event) => {
        const digits = String(event.target?.value || "").replace(/[^0-9]/g, "");
        event.target.value = digits;
      });
      this.localActorIdInput.addEventListener("change", (event) => {
        const value = String(event.target?.value || "").trim();
        if (!value) {
          // User cleared the ID — reset so auto-detection can take over
          this.localPlayerId = null;
          window.javaBridge?.bindLocalActorId?.(0);
          return;
        }
        this.localPlayerId = Number(value);
        window.javaBridge?.bindLocalActorId?.(value);
        if (this.USER_NAME) {
          window.javaBridge?.bindLocalNickname?.(value, this.USER_NAME);
        }
        this.setUserName(this.USER_NAME, { persist: true, syncBackend: true });
      });
    }

    const allowedWindows = ["30000", "60000", "120000", "180000", "300000"];
    const selectedWindow = allowedWindows.includes(String(storedAllTargetsWindowMs))
      ? String(storedAllTargetsWindowMs)
      : "120000";
    this.settingsSelections.allTargetsWindowMs = selectedWindow;
    this.safeSetSetting(this.storageKeys.allTargetsWindowMs, selectedWindow);
    window.javaBridge?.setAllTargetsWindowMs?.(selectedWindow);

    const allowedTargetWindows = ["5000", "10000", "15000", "20000", "30000"];
    const selectedTargetWindow = allowedTargetWindows.includes(String(storedTargetSelectionWindowMs))
      ? String(storedTargetSelectionWindowMs)
      : "5000";
    this.settingsSelections.targetSelectionWindowMs = selectedTargetWindow;
    this.safeSetSetting(this.storageKeys.targetSelectionWindowMs, selectedTargetWindow);
    window.javaBridge?.setTargetSelectionWindowMs?.(selectedTargetWindow);

    const allowedModes = ["all", "highestDamage"];
    const selectedMode = allowedModes.includes(String(storedTrainSelectionMode))
      ? String(storedTrainSelectionMode)
      : "all";
    this.trainSelectionMode = selectedMode;
    this.settingsSelections.trainSelectionMode = selectedMode;
    this.safeSetSetting(this.storageKeys.trainSelectionMode, selectedMode);
    window.javaBridge?.setTrainSelectionMode?.(selectedMode);

    if (this.bossLogsCheckbox) {
      const storedBossLogs = this.safeGetSetting(this.storageKeys.bossLogs) === "true";
      this.bossLogsCheckbox.checked = storedBossLogs;
      this.bossLogsCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.bossLogs, String(isChecked));
        window.javaBridge?.setBossLogsEnabled?.(isChecked);
      });
    }
    if (this.debugLoggingCheckbox) {
      this.debugLoggingCheckbox.checked = this.debugLoggingEnabled;
      this.debugLoggingCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.setDebugLogging(isChecked, { persist: true, syncBackend: true });
      });
    }
    if (this.showPingCheckbox) {
      this.showPingCheckbox.checked = this.showPing;
      this.showPingCheckbox.addEventListener("change", (event) => {
        this.showPing = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.showPing, String(this.showPing));
      });
    }
    this.showTotalDpsCheckbox = document.querySelector(".showTotalDpsCheckbox");
    if (this.showTotalDpsCheckbox) {
      this.showTotalDpsCheckbox.checked = this.showTotalDps;
      this.showTotalDpsCheckbox.addEventListener("change", (event) => {
        this.showTotalDps = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.showTotalDps, String(this.showTotalDps));
        this.renderCurrentRows();
      });
    }
    this.roundDpsCheckbox = document.querySelector(".roundDpsCheckbox");
    if (this.roundDpsCheckbox) {
      this.roundDpsCheckbox.checked = this.roundDps;
      this.roundDpsCheckbox.addEventListener("change", (event) => {
        this.roundDps = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.roundDps, String(this.roundDps));
        this.renderCurrentRows();
      });
    }
    this.autoHideMeterCheckbox = document.querySelector(".autoHideMeterCheckbox");
    if (this.autoHideMeterCheckbox) {
      const storedAutoHide = this.safeGetSetting(this.storageKeys.autoHideMeter) !== "false";
      this.autoHideMeterCheckbox.checked = storedAutoHide;
      this.autoHideMeterCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.autoHideMeter, String(isChecked));
        window.javaBridge?.setAutoHideMeter?.(isChecked);
      });
    }
    // Suspend button setting
    if (this.showSuspendBtnCheckbox) {
      const storedShow = this.safeGetSetting(this.storageKeys.showSuspendBtn) === "true";
      this.showSuspendBtnCheckbox.checked = storedShow;
      this._applySuspendBtnVisibility(storedShow);
      this.showSuspendBtnCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.showSuspendBtn, String(isChecked));
        this._applySuspendBtnVisibility(isChecked);
        if (!isChecked) {
          // When hiding the button, also resume if suspended
          this._setCaptureSuspended(false);
        }
      });
    }
    this.initOverlayLock();
    // Restore suspend state from backend on load
    this._captureSuspended = !!window.javaBridge?.isCaptureSuspended?.();
    this._updateSuspendBtnIcon();
    this._updateSuspendStatusMessage();

    this.initPlayerLimitDropdown();
    if (this.saveRawPacketsCheckbox) {
      const storedSaveRaw = this.safeGetSetting(this.storageKeys.saveRawPackets) === "true";
      this.saveRawPacketsCheckbox.checked = storedSaveRaw;
      this.saveRawPacketsCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.saveRawPackets, String(isChecked));
        window.javaBridge?.setSaveRawPackets?.(isChecked);
      });
    }
    this.initSendLogs();
    if (this.pinMeToTopCheckbox) {
      this.pinMeToTopCheckbox.checked = this.pinMeToTop;
      this.pinMeToTopCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.setPinMeToTop(isChecked, { persist: true });
      });
    }
    if (this.accountConnectBtn) {
      this.accountConnectBtn.addEventListener("click", async () => {
        this.accountConnectBtn.disabled = true;
        try {
          const prompt = await window.javaBridge?.accountBeginLink?.();
          if (prompt?.userCode) {
            // Shown, not hidden: the browser may have failed to open, and the
            // code is the only way back into the flow if it did.
            if (this.accountCodeEl) this.accountCodeEl.textContent = prompt.userCode;
            if (this.accountCodeBox) this.accountCodeBox.style.display = "block";
            this.setAccountState(
              window.i18n?.t?.("settings.account.working", "Waiting for approval…")
            );
          }
        } catch (err) {
          const msg = typeof err === "string" ? err : err?.message || String(err);
          this.setAccountState(
            window.i18n?.format?.("settings.account.failed", { error: msg }, `Sign-in failed: ${msg}`)
          );
          this.accountConnectBtn.disabled = false;
        }
      });
    }
    if (this.accountSignOutBtn) {
      this.accountSignOutBtn.addEventListener("click", async () => {
        await window.javaBridge?.accountSignOut?.();
        this.refreshAccountPanel();
      });
    }
    this.refreshAccountPanel();

    // Off unless turned on: it shows others what you are playing. Offered
    // only when this build has a Discord application to show it under.
    const discordCheckbox = document.querySelector(".discordActivityCheckbox");
    if (discordCheckbox) {
      discordCheckbox.checked = this.safeGetSetting(this.storageKeys.discordActivity) === "true";
      discordCheckbox.addEventListener("change", (event) => {
        this.safeSetSetting(this.storageKeys.discordActivity, String(!!event.target?.checked));
      });
      Promise.resolve(window.javaBridge?.discordActivityAvailable?.())
        .then((ok) => {
          const group = document.querySelector(".discordActivityGroup");
          if (group && ok) group.style.display = "";
          if (ok && window.A2_VIEW === "main") this.maybeShowDiscordPromo();
        })
        .catch(() => {});
    }

    if (this.autoUploadCheckbox) {
      // Off unless turned on: an upload publishes a fight.
      this.autoUploadCheckbox.checked =
        this.safeGetSetting(this.storageKeys.autoUpload) === "true";
      this.autoUploadCheckbox.addEventListener("change", (event) => {
        this.safeSetSetting(this.storageKeys.autoUpload, String(!!event.target?.checked));
      });
    }

    if (this.showSupporterColorsCheckbox) {
      this.showSupporterColorsCheckbox.checked = this.showSupporterColors;
      this.showSupporterColorsCheckbox.addEventListener("change", (event) => {
        this.showSupporterColors = !!event.target?.checked;
        this.safeSetSetting(
          this.storageKeys.showSupporterColors,
          String(this.showSupporterColors)
        );
      });
    }
    if (this.playerNamesBoldCheckbox) {
      this.playerNamesBoldCheckbox.checked = this.mainPlayerNamesBold;
      this.playerNamesBoldCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.setMainPlayerNamesBold(isChecked, { persist: true });
      });
    }
    if (this.playerDpsBoldCheckbox) {
      this.playerDpsBoldCheckbox.checked = this.mainPlayerDpsBold;
      this.playerDpsBoldCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.setMainPlayerDpsBold(isChecked, { persist: true });
      });
    }
    if (this.meterOpacityInput && this.meterOpacityValue) {
      const defaultOpacity = this.getDefaultMeterFillOpacity();
      const hasStoredOpacity =
        storedMeterOpacity !== null &&
        storedMeterOpacity !== undefined &&
        String(storedMeterOpacity).trim() !== "";
      const resolvedOpacity = hasStoredOpacity
        ? this.normalizeMeterOpacity(storedMeterOpacity, defaultOpacity)
        : defaultOpacity;
      this.applyMeterFillOpacity(resolvedOpacity, { persist: false });
      this.meterOpacityInput.value = String(resolvedOpacity);
      this.meterOpacityValue.textContent = `${resolvedOpacity}%`;
      const stopDrag = (event) => event.stopPropagation();
      this.meterOpacityInput.addEventListener("mousedown", stopDrag);
      this.meterOpacityInput.addEventListener("touchstart", stopDrag, { passive: true });
      this.meterOpacityInput.addEventListener("input", (event) => {
        const value = Number(event.target?.value);
        const next = this.normalizeMeterOpacity(value, defaultOpacity);
        this.meterOpacityValue.textContent = `${next}%`;
        this.applyMeterFillOpacity(next, { persist: true });
      });
    }

    // Target name size
    if (this.bossNameSizeInput && this.bossNameSizeValue) {
      const defaultBossNameSize = this.getDefaultBossNameSize();
      const storedBossNameSize = this.safeGetSetting(this.storageKeys.bossNameSize);
      const resolvedBossNameSize =
        storedBossNameSize !== null && String(storedBossNameSize).trim() !== ""
          ? this.normalizeBossNameSize(storedBossNameSize, defaultBossNameSize)
          : defaultBossNameSize;
      this.applyBossNameSize(resolvedBossNameSize, { persist: false });
      this.bossNameSizeInput.value = String(resolvedBossNameSize);
      this.bossNameSizeValue.textContent = `${resolvedBossNameSize}px`;
      const stopBossNameDrag = (event) => event.stopPropagation();
      this.bossNameSizeInput.addEventListener("mousedown", stopBossNameDrag);
      this.bossNameSizeInput.addEventListener("touchstart", stopBossNameDrag, { passive: true });
      this.bossNameSizeInput.addEventListener("input", (event) => {
        const next = this.normalizeBossNameSize(event.target?.value, defaultBossNameSize);
        this.bossNameSizeValue.textContent = `${next}px`;
        this.applyBossNameSize(next, { persist: true });
      });
    }

    // Window opacity
    if (this.windowOpacityInput && this.windowOpacityValue) {
      const storedWindowOpacity = this.safeGetStorage(this.storageKeys.windowOpacity);
      const resolvedWindowOpacity = storedWindowOpacity !== null && String(storedWindowOpacity).trim() !== ""
        ? Math.max(0, Math.min(100, Math.round(Number(storedWindowOpacity))))
        : 40;
      this.applyWindowOpacity(resolvedWindowOpacity, { persist: false });
      this.windowOpacityInput.value = String(resolvedWindowOpacity);
      this.windowOpacityValue.textContent = `${resolvedWindowOpacity}%`;
      const stopDrag = (event) => event.stopPropagation();
      this.windowOpacityInput.addEventListener("mousedown", stopDrag);
      this.windowOpacityInput.addEventListener("touchstart", stopDrag, { passive: true });
      this.windowOpacityInput.addEventListener("input", (event) => {
        const value = Number(event.target?.value);
        const next = Math.max(0, Math.min(100, Math.round(value)));
        this.windowOpacityValue.textContent = `${next}%`;
        this.applyWindowOpacity(next, { persist: true });
      });
    }

    // Mirror each slider's position into a CSS custom property so the track
    // can paint a filled portion. Presentation only — no setting reads it.
    const syncRangeFill = (input) => {
      if (!input) return;
      const min = Number(input.min);
      const max = Number(input.max);
      const value = Number(input.value);
      if (!Number.isFinite(min) || !Number.isFinite(max) || !Number.isFinite(value) || max <= min) {
        return;
      }
      const pct = Math.max(0, Math.min(100, ((value - min) / (max - min)) * 100));
      input.style.setProperty("--range-pct", `${pct}%`);
    };
    // Both panels: the Details settings menu uses the same slider geometry, so
    // its tracks read the same property.
    const isRange = (el) => el?.tagName === "INPUT" && el.type === "range";
    [this.settingsPanel, this.detailsPanel].forEach((root) => {
      if (!root) return;
      root.querySelectorAll('input[type="range"]').forEach(syncRangeFill);
      root.addEventListener("input", (event) => {
        if (isRange(event.target)) syncRangeFill(event.target);
      });
    });

    this.setupKeybindButtons();

    const currentLanguage = this.i18n?.getLanguage?.() || storedLanguage || "en";
    this.settingsSelections.language = currentLanguage;
    this.settingsSelections.theme = this.theme;

    this.detailsMonitor = this.getDetailsMonitorSetting();
    this.refreshMonitorList().then(() => {
      this.initializeSettingsDropdowns();
      // Re-open the Details window if it was left enabled last session.
      if (this.detailsMonitor !== "off" && window.A2_VIEW !== "details") {
        this.applyDetailsMonitor(this.detailsMonitor, { persist: false });
      }
    });

    this.initializeSettingsDropdowns();

    this.settingsBtn?.addEventListener("click", () => {
      // Settings is its own window now, so the overlay no longer grows to
      // ~820px to contain it.
      if (window.A2_VIEW === "main") {
        window.javaBridge?.openSettingsWindow?.();
        return;
      }
      this.refreshMonitorList().then(() => this.initializeSettingsDropdowns());
      this.toggleSettingsPanel();
    });

    this.settingsClose?.addEventListener("click", () => this.closeSettingsPanel());

    const advancedToggle = document.querySelector(".settingsAdvancedToggle");
    const advancedBody = document.querySelector(".settingsAdvancedBody");
    advancedToggle?.addEventListener("click", () => {
      const isOpen = advancedToggle.classList.toggle("isOpen");
      if (advancedBody) advancedBody.style.display = isOpen ? "" : "none";
    });

    this.resetDetectBtn?.addEventListener("click", () => {
      window.javaBridge?.resetAutoDetection?.();
      this.refreshConnectionInfo();
    });

    // Device selection: auto-detect checkbox + manual device dropdown
    this._autoDetectDevice = !this.safeGetSetting("dpsMeter.manualDevice");
    if (this.autoDetectDeviceCheckbox) {
      this.autoDetectDeviceCheckbox.checked = this._autoDetectDevice;
      this._updateDeviceDropdownState();
      this.autoDetectDeviceCheckbox.addEventListener("change", () => {
        this._autoDetectDevice = this.autoDetectDeviceCheckbox.checked;
        this._updateDeviceDropdownState();
        if (this._autoDetectDevice) {
          window.javaBridge?.setManualDevice?.("");
          this.refreshConnectionInfo();
        } else {
          this._loadDeviceDropdown();
        }
      });
    }

    document.querySelector(".resetAllSettingsBtn")?.addEventListener("click", () => {
      this.resetAllSettings();
    });

    this.discordButton?.addEventListener("click", () => {
      window.javaBridge?.openBrowser?.("https://discord.gg/Aion2Global");
    });

    this.supportButton?.addEventListener("click", () => {
      this.openSupportModal();
    });
    this.supportModalClose?.addEventListener("click", () => this.closeSupportModal());
    this.supportModal?.addEventListener("click", (event) => {
      if (event.target === this.supportModal) {
        this.closeSupportModal();
      }
    });
    document.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      if (this.supportModal?.classList.contains("isOpen")) {
        this.closeSupportModal();
      }
    });
    this.supportActionButtons?.forEach((button) => {
      button.addEventListener("click", () => this.handleSupportAction(button));
    });

    this.kofiButton?.addEventListener("click", () => {
      window.javaBridge?.openBrowser?.("https://ko-fi.com/W7W51T1YW9");
    });

    this.settingsVersionLink?.addEventListener("click", () => {
      window.javaBridge?.openBrowser?.("https://github.com/taengu/AION2-DPS-Meter/releases");
    });

    // Quit is wired in startApp, before anything that can be slow.

    this.updateSettingsVersion();
    this.updateSupportVisibility(currentLanguage);
    this.updateSupportPrimaryAction(currentLanguage);
    this.updateSupportQrImage(this.supportPrimaryButton?.dataset.support || "afdian");
  }

  isChineseLanguage(lang) {
    return String(lang || "").startsWith("zh");
  }

  updateSupportVisibility() {
    if (this.supportWidget) {
      this.supportWidget.style.display = "flex";
    }
    if (this.kofiWidget) {
      this.kofiWidget.style.display = "none";
    }
  }

  getSupportIconSvg(type) {
    const iconByType = {
      afdian:
        '<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M13.5 2 5 13h5l-1.5 9L19 10h-5.5L13.5 2z" fill="currentColor"/></svg>',
      kofi:
        '<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M4 7h12v7a5 5 0 0 1-5 5H9a5 5 0 0 1-5-5V7z" fill="currentColor"/><path d="M16 9h1.5a2.5 2.5 0 0 1 0 5H16V9z" fill="currentColor" opacity="0.75"/><path d="M6 5h8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" fill="none"/></svg>',
      wechat:
        '<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M9 4c-3.87 0-7 2.69-7 6 0 1.9 1.03 3.59 2.62 4.69L4 19l3.66-1.85A8.4 8.4 0 0 0 9 17c3.87 0 7-2.69 7-6s-3.13-7-7-7z" fill="currentColor"/><path d="M16.5 10.5c3.04 0 5.5 2.01 5.5 4.5 0 1.42-.79 2.69-2.02 3.52L20.5 22l-2.79-1.41c-.39.08-.79.12-1.21.12-3.04 0-5.5-2.01-5.5-4.5s2.46-4.5 5.5-4.5z" fill="currentColor" opacity="0.78"/></svg>',
      paypal:
        '<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M7.2 3.2h7.2c3.2 0 5.3 1.9 4.9 4.7-.42 2.85-2.8 4.58-6.1 4.58h-2.4l-.75 4.35H5.9L7.2 3.2z" fill="currentColor"/><path d="M9.3 5.6h4.05c1.3 0 2.05.74 1.83 1.82-.22 1.13-1.2 1.84-2.49 1.84H8.55L9.3 5.6z" fill="currentColor" opacity="0.55"/><path d="M10.55 9.05h2.52c1.95 0 3.2 1.14 2.93 2.86-.3 1.92-1.95 3.12-4.19 3.12h-2.38l.56-3.18h2.2c.75 0 1.23-.4 1.33-1 .1-.56-.3-.93-1.02-.93h-2.15l.2-.87z" fill="#0b2f63" opacity="0.45"/></svg>',
    };
    return iconByType[type] || "";
  }

  updateSupportPrimaryAction(lang) {
    if (!this.supportPrimaryButton) return;
    const isChinese = this.isChineseLanguage(lang);
    const nextSupport = isChinese ? "afdian" : "kofi";
    const nextUrl = isChinese
      ? "https://afdian.com/a/hiddencube"
      : "https://ko-fi.com/hiddencube";
    const nextLabel = isChinese ? "爱发电" : "Ko-fi";
    const nextIcon = this.getSupportIconSvg(isChinese ? "afdian" : "kofi");
    this.supportPrimaryButton.dataset.support = nextSupport;
    this.supportPrimaryButton.dataset.url = nextUrl;
    const label = this.supportPrimaryButton.querySelector(".supportLabel");
    const icon = this.supportPrimaryButton.querySelector(".supportIcon");
    if (label) label.textContent = nextLabel;
    if (icon) icon.innerHTML = nextIcon;
    this.supportPrimaryButton.setAttribute("aria-label", nextLabel);
    const i18nLabel = isChinese ? "support.aria.afdian" : "support.aria.kofi";
    this.supportPrimaryButton.dataset.i18nAriaLabel = i18nLabel;
  }

  openSupportModal() {
    if (!this.supportModal) return;
    this.supportModal.classList.add("isOpen");
    this.supportModal.setAttribute("aria-hidden", "false");
    if (this.supportCopyStatus) {
      this.supportCopyStatus.textContent = "";
    }
  }

  closeSupportModal() {
    if (!this.supportModal) return;
    this.supportModal.classList.remove("isOpen");
    this.supportModal.setAttribute("aria-hidden", "true");
  }

  handleSupportAction(button) {
    if (!button) return;
    const supportType = button.dataset.support;
    const url = button.dataset.url;
    const copyValue = button.dataset.copy;
    const qrType = button.dataset.qr || supportType;

    if (qrType && this.supportQrImages?.[qrType]) {
      this.updateSupportQrImage(qrType);
    }

    if (url) {
      const externalOnly = supportType === "paypal" || supportType === "afdian" || supportType === "kofi";
      this.openExternalLink(url, { externalOnly });
    }

    if (copyValue) {
      const messageKey = `support.copy.${supportType}`;
      const fallback = `Copied ${supportType?.toUpperCase?.() || "address"}`;
      this.copySupportValue(copyValue, this.i18n?.t?.(messageKey, fallback) || fallback);
    }
  }

  openExternalLink(url, { externalOnly = false } = {}) {
    if (!url) return;
    window.javaBridge?.openBrowser?.(url);
    if (externalOnly) return;
    try {
      window.open(url, "_blank", "noopener");
    } catch {
      // ignore
    }
  }

  copySupportValue(value, message) {
    if (!value) return;
    const showStatus = (text) => {
      if (!this.supportCopyStatus) return;
      this.supportCopyStatus.textContent = text;
    };
    const attemptLegacyCopy = () => {
      try {
        const textarea = document.createElement("textarea");
        textarea.value = value;
        textarea.style.position = "fixed";
        textarea.style.opacity = "0";
        document.body.appendChild(textarea);
        textarea.select();
        const success = document.execCommand("copy");
        document.body.removeChild(textarea);
        if (success) showStatus(message);
      } catch {
        // ignore
      }
    };
    if (navigator.clipboard?.writeText) {
      navigator.clipboard
        .writeText(value)
        .then(() => showStatus(message))
        .catch(() => attemptLegacyCopy());
      return;
    }
    attemptLegacyCopy();
  }

  updateSupportQrImage(type) {
    if (!this.supportQrImage) return;
    const src = this.supportQrImages?.[type];
    if (!src) return;
    this.supportQrImage.src = src;
    this.updateSupportTitle(type);
    this.supportActionButtons?.forEach((button) => {
      const match = button.dataset.support === type || button.dataset.qr === type;
      button.classList.toggle("isActive", match);
    });
  }

  updateSupportTitle(type) {
    if (!this.supportModalTitle) return;
    const currentLanguage = this.i18n?.getLanguage?.();
    const isChinese = this.isChineseLanguage(currentLanguage);
    const isWeChat = type === "wechat";
    const titleKey = isWeChat ? "support.titleWechat" : "support.title";
    const fallback = isWeChat
      ? "Support the author on WeChat"
      : isChinese
        ? "Support the author on Afdian"
        : "Support the author on Ko-fi";
    this.supportModalTitle.textContent = this.i18n?.t?.(titleKey, fallback) || fallback;
  }

  initializeSettingsDropdowns() {
    const previewThemeVars = (themeId) => {
      const root = document.documentElement;
      const previous = root.dataset.theme;
      root.dataset.theme = themeId;
      const computed = getComputedStyle(root);
      const textColor = computed.getPropertyValue("--text-color").trim() || "#ffffff";
      const nameShadow = computed.getPropertyValue("--player-name-shadow").trim() || "none";
      const rowFill = computed.getPropertyValue("--row-fill").trim() || "#2f2f2f";
      root.dataset.theme = previous || "aion2";
      return { textColor, nameShadow, rowFill };
    };

    const closeAll = () => {
      document.querySelectorAll(".settingsDropdownMenu.isOpen").forEach((menu) => {
        menu.classList.remove("isOpen");
      });
    };

    const setupDropdown = (
      button,
      menu,
      options,
      currentValue,
      onSelect,
      { decorateItem = null, decorateButton = null } = {}
    ) => {
      if (!button || !menu) return;
      const optionList = Array.isArray(options) ? options : [];
      menu.innerHTML = "";

      optionList.forEach((opt) => {
        const item = document.createElement("button");
        item.type = "button";
        item.className = "settingsDropdownItem";
        item.dataset.value = opt.value;
        item.textContent = opt.label;
        if (String(opt.value) === String(currentValue)) {
          item.classList.add("isActive");
        }
        decorateItem?.(item, opt.value);
        item.addEventListener("click", () => {
          onSelect?.(opt.value);
          menu.classList.remove("isOpen");
          this.initializeSettingsDropdowns();
        });
        menu.appendChild(item);
      });

      const selected = optionList.find((opt) => String(opt.value) === String(currentValue)) || optionList[0];
      const textEl = button.querySelector(".settingsDropdownText");
      if (textEl) {
        textEl.textContent = selected?.label || "-";
      }
      button.style.background = "";
      button.style.color = "";
      decorateButton?.(button, selected?.value);
      button.onclick = (event) => {
        event.stopPropagation();
        const wasOpen = menu.classList.contains("isOpen");
        closeAll();
        menu.classList.toggle("isOpen", !wasOpen);
      };
    };

    if (!this._settingsDropdownOutsideBound) {
      document.addEventListener("click", (event) => {
        if (!event.target?.closest?.(".settingsDropdownWrapper")) {
          closeAll();
        }
      });
      this._settingsDropdownOutsideBound = true;
    }

    const languageOptions = [
      { value: "en", label: "English" },
      { value: "de", label: "Deutsch" },
      { value: "es", label: "Español" },
      { value: "fr", label: "Français" },
      { value: "ja", label: "日本語" },
      { value: "ko", label: "한국어" },
      { value: "pt", label: "Português" },
      { value: "ru", label: "Русский" },
      { value: "zh-Hant", label: "繁體中文" },
      { value: "zh-Hans", label: "简体中文" },
    ];

    const themeOptions = [
      { value: "aion2", label: this.i18n?.t("settings.theme.options.aion2", "AION2") },
      { value: "asmodian", label: this.i18n?.t("settings.theme.options.asmodian", "Asmodian") },
      { value: "cogni", label: this.i18n?.t("settings.theme.options.cogni", "Cogni") },
      { value: "elyos", label: this.i18n?.t("settings.theme.options.elyos", "Elyos") },
      { value: "ember", label: this.i18n?.t("settings.theme.options.ember", "Ember") },
      { value: "fera", label: this.i18n?.t("settings.theme.options.fera", "Fera") },
      { value: "frost", label: this.i18n?.t("settings.theme.options.frost", "Frost") },
      { value: "natura", label: this.i18n?.t("settings.theme.options.natura", "Natura") },
      { value: "obsidian", label: this.i18n?.t("settings.theme.options.obsidian", "Obsidian") },
      { value: "varian", label: this.i18n?.t("settings.theme.options.varian", "Varian") },
    ];

    themeOptions.sort((a, b) => a.label.localeCompare(b.label));

    const targetWindowOptions = [
      { value: "5000", label: this.i18n?.t("settings.targetWindow.options.5s", "5 seconds") },
      { value: "10000", label: this.i18n?.t("settings.targetWindow.options.10s", "10 seconds") },
      { value: "15000", label: this.i18n?.t("settings.targetWindow.options.15s", "15 seconds") },
      { value: "20000", label: this.i18n?.t("settings.targetWindow.options.20s", "20 seconds") },
      { value: "30000", label: this.i18n?.t("settings.targetWindow.options.30s", "30 seconds") },
    ];

    const allTargetsWindowOptions = [
      { value: "30000", label: this.i18n?.t("settings.allTargetsWindow.options.30s", "30 seconds") },
      { value: "60000", label: this.i18n?.t("settings.allTargetsWindow.options.1m", "1 minute") },
      { value: "120000", label: this.i18n?.t("settings.allTargetsWindow.options.2m", "2 minutes") },
      { value: "180000", label: this.i18n?.t("settings.allTargetsWindow.options.3m", "3 minutes") },
      { value: "300000", label: this.i18n?.t("settings.allTargetsWindow.options.5m", "5 minutes") },
    ];

    const defaultMeterModeOptions = [
      { value: "lastHitByMe", label: "TARGET" },
      { value: "bossTargets", label: "BOSS" },
      { value: "allTargets", label: "ALL" },
      { value: "trainTargets", label: "TRAIN" },
    ];

    const trainModeOptions = [
      { value: "all", label: this.i18n?.t("settings.trainingMode.options.all", "All") },
      {
        value: "highestDamage",
        label: this.i18n?.t("settings.trainingMode.options.highestDamage", "Highest Damage"),
      },
    ];

    setupDropdown(
      this.languageDropdownBtn,
      this.languageDropdownMenu,
      languageOptions,
      this.settingsSelections.language,
      (value) => {
        if (!value) return;
        this.settingsSelections.language = value;
        this.safeSetStorage(this.storageKeys.language, value);
        this.i18n?.setLanguage?.(value, { persist: true });
      }
    );

    setupDropdown(
      this.themeDropdownBtn,
      this.themeDropdownMenu,
      themeOptions,
      this.settingsSelections.theme,
      (value) => {
        if (!value) return;
        this.settingsSelections.theme = value;
        this.applyTheme(value, { persist: true });
      },
      {
        // Only the colours are set here; size and weight come from the
        // stylesheet so the theme control matches every other dropdown.
        decorateItem: (item, value) => {
          const colors = previewThemeVars(value);
          item.style.background = colors.rowFill;
          item.style.opacity = "1";
          item.style.color = colors.textColor;
          item.style.textShadow = colors.nameShadow;
        },
        decorateButton: (button, value) => {
          const colors = previewThemeVars(value);
          button.style.background = colors.rowFill;
          button.style.opacity = "1";
          button.style.color = colors.textColor;
          button.style.textShadow = colors.nameShadow;
          const textEl = button.querySelector(".settingsDropdownText");
          if (textEl) {
            textEl.style.textShadow = colors.nameShadow;
          }
        },
      }
    );

    setupDropdown(
      this.targetWindowDropdownBtn,
      this.targetWindowDropdownMenu,
      targetWindowOptions,
      this.settingsSelections.targetSelectionWindowMs,
      (value) => {
        if (!value) return;
        this.settingsSelections.targetSelectionWindowMs = value;
        this.safeSetSetting(this.storageKeys.targetSelectionWindowMs, value);
        window.javaBridge?.setTargetSelectionWindowMs?.(value);
        if (!this.isCollapse) this.fetchDps();
      }
    );

    setupDropdown(
      this.allTargetsWindowDropdownBtn,
      this.allTargetsWindowDropdownMenu,
      allTargetsWindowOptions,
      this.settingsSelections.allTargetsWindowMs,
      (value) => {
        if (!value) return;
        this.settingsSelections.allTargetsWindowMs = value;
        this.safeSetSetting(this.storageKeys.allTargetsWindowMs, value);
        window.javaBridge?.setAllTargetsWindowMs?.(value);
        if (!this.isCollapse) this.fetchDps();
      }
    );

    setupDropdown(
      this.trainSelectionModeDropdownBtn,
      this.trainSelectionModeDropdownMenu,
      trainModeOptions,
      this.settingsSelections.trainSelectionMode,
      (value) => {
        if (!value) return;
        this.settingsSelections.trainSelectionMode = value;
        this.trainSelectionMode = value;
        this.safeSetSetting(this.storageKeys.trainSelectionMode, value);
        window.javaBridge?.setTrainSelectionMode?.(value);
        if (!this.isCollapse) this.fetchDps();
      }
    );

    // Monitor picker. Rebuilt every time the settings panel opens so hot-plugged
    // displays appear without a restart.
    {
      const monitors = Array.isArray(this.monitorList) ? this.monitorList : [];
      const offLabel = this.i18n?.t?.("settings.detailsMonitor.off", "Off") ?? "Off";
      const detailsMonitorOptions = [{ value: "off", label: offLabel }].concat(
        monitors.map((m, i) => ({ value: String(m.index), label: this.monitorLabel(m, i) }))
      );
      const current = this.detailsMonitor ?? this.getDetailsMonitorSetting();
      setupDropdown(
        this.detailsMonitorDropdownBtn,
        this.detailsMonitorDropdownMenu,
        detailsMonitorOptions,
        detailsMonitorOptions.some((o) => o.value === current) ? current : "off",
        (value) => {
          this.applyDetailsMonitor(value, { persist: true });
        }
      );
      if (monitors.length <= 1) {
        this.setDetailsMonitorHint(
          this.i18n?.t?.(
            "settings.detailsMonitor.single",
            "Only one display detected — connect a second screen to use this."
          ) ?? "Only one display detected — connect a second screen to use this."
        );
      }
    }

    // One control for both axes of the main window: which skin, and how dense.
    // They were two separate toggles, which made four states the user had to
    // assemble themselves.
    const meterLayoutOptions = [
      { value: "beta", label: this.i18n?.t?.("settings.meterLayout.beta", "Beta UI") ?? "Beta UI" },
      { value: "betaSlim", label: this.i18n?.t?.("settings.meterLayout.betaSlim", "Beta Slim") ?? "Beta Slim" },
      { value: "classic", label: this.i18n?.t?.("settings.meterLayout.classic", "Classic UI") ?? "Classic UI" },
      { value: "classicSlim", label: this.i18n?.t?.("settings.meterLayout.classicSlim", "Classic Slim") ?? "Classic Slim" },
    ];
    setupDropdown(
      this.meterLayoutDropdownBtn,
      this.meterLayoutDropdownMenu,
      meterLayoutOptions,
      this.getMeterLayout(),
      (value) => {
        if (!value) return;
        this.setMeterLayout(value, { persist: true });
      }
    );

    setupDropdown(
      this.defaultMeterModeDropdownBtn,
      this.defaultMeterModeDropdownMenu,
      defaultMeterModeOptions,
      this.settingsSelections.defaultMeterMode,
      (value) => {
        if (!value) return;
        this.settingsSelections.defaultMeterMode = value;
        this.safeSetSetting(this.storageKeys.defaultMeterMode, value);
        this.setTargetSelection(value, {
          persist: true,
          syncBackend: true,
          reason: "default meter mode changed",
        });
        if (!this.isCollapse) this.fetchDps();
      }
    );
  }

  setupDetailsPanelSettings() {
    this.detailsOpacityInput = document.querySelector(".detailsOpacityInput");
    this.detailsOpacityValue = document.querySelector(".detailsOpacityValue");
    this.detailsFontSizeInput = document.querySelector(".detailsFontSizeInput");
    this.detailsFontSizeValue = document.querySelector(".detailsFontSizeValue");
    this.detailsIconSizeInput = document.querySelector(".detailsIconSizeInput");
    this.detailsIconSizeValue = document.querySelector(".detailsIconSizeValue");
    this.detailsSettingsBtn = document.querySelector(".detailsSettingsBtn");
    this.detailsSettingsMenu = document.querySelector(".detailsSettingsMenu");
    this.detailsIncludeMeterCheckbox = document.querySelector(".detailsIncludeMeterCheckbox");
    this.detailsSaveScreenshotCheckbox = document.querySelector(".detailsSaveScreenshotCheckbox");
    this.detailsScreenshotFolderRow = document.querySelector(".detailsSettingsFolder");
    this.detailsScreenshotFolderPath = document.querySelector(".detailsSettingsFolderPath");
    this.detailsScreenshotFolderBtn = document.querySelector(".detailsSettingsFolderBtn");
    this.detailsColumnToggles = document.querySelectorAll(".detailsColumnToggle");

    const storedOpacity = this.safeGetSetting(this.storageKeys.detailsBackgroundOpacity);
    const initialOpacity = this.parseDetailsOpacity(storedOpacity);
    this.setDetailsBackgroundOpacity(initialOpacity, { persist: false });

    const storedFontSize = this.safeGetSetting(this.storageKeys.detailsFontSize);
    const initialFontSize = this.parseDetailsFontSize(storedFontSize);
    this.setDetailsFontSize(initialFontSize, { persist: false });

    const storedIconSize = this.safeGetSetting(this.storageKeys.detailsIconSize);
    const initialIconSize = this.parseDetailsIconSize(storedIconSize);
    this.setDetailsIconSize(initialIconSize, { persist: false });

    const storedIncludeMeter =
      this.safeGetSetting(this.storageKeys.detailsIncludeMeterScreenshot) === "true";
    const storedSaveToFolder =
      this.safeGetSetting(this.storageKeys.detailsSaveScreenshotToFolder) === "true";
    const storedFolder = this.safeGetSetting(this.storageKeys.detailsScreenshotFolder);
    this.includeMainMeterScreenshot = storedIncludeMeter;
    this.saveScreenshotToFolder = storedSaveToFolder;
    this.screenshotFolder = storedFolder || this.getDefaultScreenshotFolder();

    if (this.detailsIncludeMeterCheckbox) {
      this.detailsIncludeMeterCheckbox.checked = this.includeMainMeterScreenshot;
      this.detailsIncludeMeterCheckbox.addEventListener("change", (event) => {
        this.includeMainMeterScreenshot = !!event.target?.checked;
        this.safeSetSetting(
          this.storageKeys.detailsIncludeMeterScreenshot,
          String(this.includeMainMeterScreenshot)
        );
      });
    }
    if (this.detailsSaveScreenshotCheckbox) {
      this.detailsSaveScreenshotCheckbox.checked = this.saveScreenshotToFolder;
      this.detailsSaveScreenshotCheckbox.addEventListener("change", (event) => {
        this.saveScreenshotToFolder = !!event.target?.checked;
        if (this.saveScreenshotToFolder && !this.screenshotFolder) {
          this.screenshotFolder = this.getDefaultScreenshotFolder();
        }
        this.safeSetSetting(
          this.storageKeys.detailsSaveScreenshotToFolder,
          String(this.saveScreenshotToFolder)
        );
        if (this.screenshotFolder) {
          this.safeSetSetting(this.storageKeys.detailsScreenshotFolder, this.screenshotFolder);
        }
        this.updateScreenshotFolderDisplay();
      });
    }
    if (this.detailsScreenshotFolderBtn) {
      this.detailsScreenshotFolderBtn.addEventListener("click", async () => {
        const selected = await window.javaBridge?.chooseScreenshotFolder?.(
          this.screenshotFolder || this.getDefaultScreenshotFolder()
        );
        if (!selected || typeof selected !== "string") return;
        this.screenshotFolder = selected;
        this.safeSetSetting(this.storageKeys.detailsScreenshotFolder, this.screenshotFolder);
        this.updateScreenshotFolderDisplay();
      });
    }
    this.updateScreenshotFolderDisplay();

    const storedHiddenColumns = this.safeGetSetting(this.storageKeys.detailsHiddenColumns);
    const storedSeenColumns = this.safeGetSetting(this.storageKeys.detailsSeenColumns);
    const hiddenColumns = new Set();
    // Columns added in v2.0.4 — default hidden for both new and upgrading users
    const NEW_COLUMNS_DEFAULT_HIDDEN = ["powershard", "regen"];
    if (typeof storedHiddenColumns === "string" && storedHiddenColumns.trim()) {
      const parsedHidden = this.safeParseJSON(storedHiddenColumns, []);
      if (Array.isArray(parsedHidden)) {
        parsedHidden.forEach((column) => {
          if (typeof column === "string" && column.trim()) {
            hiddenColumns.add(column);
          }
        });
      }
    }
    // Track which columns the user has seen in their settings UI. Any column
    // not yet seen defaults to hidden, regardless of upgrade state.
    const seenColumns = new Set();
    if (typeof storedSeenColumns === "string" && storedSeenColumns.trim()) {
      const parsedSeen = this.safeParseJSON(storedSeenColumns, []);
      if (Array.isArray(parsedSeen)) {
        parsedSeen.forEach((c) => { if (typeof c === "string") seenColumns.add(c); });
      }
    }
    NEW_COLUMNS_DEFAULT_HIDDEN.forEach((col) => {
      if (!seenColumns.has(col)) hiddenColumns.add(col);
    });
    const applyDetailsColumnVisibility = () => {
      if (!this.detailsPanel) return;
      const columns = ["hit", "dmg", "dmgpct", "mhit", "mdmg", "crit", "parry", "perfect", "double", "back", "powershard", "regen", "mindmg", "avgdmg", "maxdmg"];
      columns.forEach((column) => {
        this.detailsPanel.classList.toggle(`hide-col-${column}`, hiddenColumns.has(column));
      });
      this.detailsUI?.updateGridColumns?.();
    };
    if (this.detailsColumnToggles && this.detailsColumnToggles.length) {
      this.detailsColumnToggles.forEach((toggle) => {
        const column = toggle.dataset.column;
        if (!column) return;
        toggle.checked = !hiddenColumns.has(column);
        toggle.addEventListener("change", (event) => {
          const isVisible = !!event.target?.checked;
          if (isVisible) {
            hiddenColumns.delete(column);
          } else {
            hiddenColumns.add(column);
          }
          // Mark as seen so the default-hidden behaviour doesn't override the user's choice
          seenColumns.add(column);
          this.safeSetSetting(this.storageKeys.detailsHiddenColumns, JSON.stringify([...hiddenColumns]));
          this.safeSetSetting(this.storageKeys.detailsSeenColumns, JSON.stringify([...seenColumns]));
          applyDetailsColumnVisibility();
        });
      });
    }
    applyDetailsColumnVisibility();

    if (this.detailsOpacityInput) {
      this.detailsOpacityInput.value = String(Math.round(initialOpacity * 100));
      const stopDrag = (event) => event.stopPropagation();
      this.detailsOpacityInput.addEventListener("mousedown", stopDrag);
      this.detailsOpacityInput.addEventListener("touchstart", stopDrag, { passive: true });
      this.detailsOpacityInput.addEventListener("input", (event) => {
        const nextValue = Number(event.target?.value);
        const nextOpacity = Number.isFinite(nextValue) ? nextValue / 100 : 1;
        this.setDetailsBackgroundOpacity(nextOpacity, { persist: true });
      });
    }

    if (this.detailsFontSizeInput) {
      this.detailsFontSizeInput.value = String(Math.round(initialFontSize));
      const stopDrag = (event) => event.stopPropagation();
      this.detailsFontSizeInput.addEventListener("mousedown", stopDrag);
      this.detailsFontSizeInput.addEventListener("touchstart", stopDrag, { passive: true });
      this.detailsFontSizeInput.addEventListener("input", (event) => {
        const nextValue = Number(event.target?.value);
        if (!Number.isFinite(nextValue)) return;
        this.setDetailsFontSize(nextValue, { persist: true });
      });
    }

    if (this.detailsIconSizeInput) {
      this.detailsIconSizeInput.value = String(Math.round(initialIconSize));
      const stopDrag = (event) => event.stopPropagation();
      this.detailsIconSizeInput.addEventListener("mousedown", stopDrag);
      this.detailsIconSizeInput.addEventListener("touchstart", stopDrag, { passive: true });
      this.detailsIconSizeInput.addEventListener("input", (event) => {
        const nextValue = Number(event.target?.value);
        if (!Number.isFinite(nextValue)) return;
        this.setDetailsIconSize(nextValue, { persist: true });
      });
    }

    this.detailsSettingsBtn?.addEventListener("click", (event) => {
      event.stopPropagation();
      this.toggleDetailsSettingsMenu(event);
    });

    this.detailsSettingsMenu?.addEventListener("click", (event) => {
      event.stopPropagation();
    });

    this.detailsClose?.addEventListener("click", () => {
      this.closeDetailsSettingsMenu();
    });

    document.addEventListener("click", (event) => {
      if (!this.detailsSettingsMenu?.classList.contains("isOpen")) {
        return;
      }
      const target = event.target;
      if (
        this.detailsSettingsMenu?.contains(target) ||
        this.detailsSettingsBtn?.contains(target)
      ) {
        return;
      }
      this.closeDetailsSettingsMenu();
    });
  }

  setupConsoleDebugging() {
    if (this._consoleDebuggingEnabled) {
      return;
    }
    this._consoleDebuggingEnabled = true;

    window.addEventListener("error", (event) => {
      console.error("[UI Error]", event.error || event.message, event);
    });

    window.addEventListener("unhandledrejection", (event) => {
      console.error("[UI Promise Rejection]", event.reason || event);
    });

    document.addEventListener("click", (event) => {
      const target = event.target;
      if (!target || typeof target.closest !== "function") return;
      const menuTarget =
        target.closest("[role='menu']") ||
        target.closest(".detailsSettingsMenu") ||
        target.closest(".detailsDropdownMenu") ||
        target.closest(".settingsPanel");
      if (!menuTarget) return;
      const menuClass = menuTarget.className || menuTarget.getAttribute?.("role") || "menu";
      const targetLabel =
        target.getAttribute?.("aria-label") ||
        target.getAttribute?.("data-i18n") ||
        target.textContent?.trim() ||
        target.tagName;
      console.log("[UI Menu Click]", { menu: menuClass, target: targetLabel });
    });
  }

  parseDetailsOpacity(value) {
    if (value === null || value === undefined || value === "") {
      return 0.8;
    }
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) {
      return 0.8;
    }
    return Math.min(1, Math.max(0, parsed));
  }

  getDefaultDetailsFontSize() {
    const rootSize = getComputedStyle(document.documentElement).getPropertyValue("--font");
    const parsed = Number.parseFloat(rootSize);
    if (Number.isFinite(parsed)) {
      return parsed;
    }
    return 16;
  }

  parseDetailsFontSize(value) {
    if (value === null || value === undefined || value === "") {
      return this.getDefaultDetailsFontSize();
    }
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) {
      return this.getDefaultDetailsFontSize();
    }
    return Math.min(this.DETAILS_FONT_SIZE_MAX, Math.max(this.DETAILS_FONT_SIZE_MIN, parsed));
  }


  parseDetailsIconSize(value) {
    if (value === null || value === undefined || value === "") {
      return 36;
    }
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) {
      return 36;
    }
    return Math.min(this.DETAILS_ICON_SIZE_MAX, Math.max(this.DETAILS_ICON_SIZE_MIN, parsed));
  }

  setDetailsBackgroundOpacity(opacity, { persist = false } = {}) {
    const clamped = Math.min(1, Math.max(0, opacity));
    if (this.detailsPanel) {
      this.detailsPanel.style.setProperty("--details-bg-opacity", clamped);
    }
    if (this.detailsOpacityValue) {
      this.detailsOpacityValue.textContent = `${Math.round(clamped * 100)}%`;
    }
    if (this.detailsOpacityInput && document.activeElement !== this.detailsOpacityInput) {
      this.detailsOpacityInput.value = String(Math.round(clamped * 100));
    }
    if (persist) {
      this.safeSetSetting(this.storageKeys.detailsBackgroundOpacity, String(clamped));
    }
  }

  setDetailsFontSize(size, { persist = false } = {}) {
    const clamped = Math.min(this.DETAILS_FONT_SIZE_MAX, Math.max(this.DETAILS_FONT_SIZE_MIN, size));
    if (this.detailsPanel) {
      this.detailsPanel.style.setProperty("--details-font-size", `${clamped}px`);
    }
    if (this.detailsFontSizeValue) {
      this.detailsFontSizeValue.textContent = `${Math.round(clamped)}px`;
    }
    if (this.detailsFontSizeInput && document.activeElement !== this.detailsFontSizeInput) {
      this.detailsFontSizeInput.value = String(Math.round(clamped));
    }
    if (persist) {
      this.safeSetSetting(this.storageKeys.detailsFontSize, String(clamped));
    }
  }


  setDetailsIconSize(size, { persist = false } = {}) {
    const clamped = Math.min(this.DETAILS_ICON_SIZE_MAX, Math.max(this.DETAILS_ICON_SIZE_MIN, size));
    if (this.detailsPanel) {
      this.detailsPanel.style.setProperty("--details-skill-icon-size", `${clamped}px`);
    }
    const tooltipSize = Math.round(Math.min(28, Math.max(16, clamped * 0.56)));
    document.documentElement.style.setProperty("--tooltip-skill-icon-size", `${tooltipSize}px`);
    if (this.detailsIconSizeValue) {
      this.detailsIconSizeValue.textContent = `${Math.round(clamped)}px`;
    }
    if (this.detailsIconSizeInput && document.activeElement !== this.detailsIconSizeInput) {
      this.detailsIconSizeInput.value = String(Math.round(clamped));
    }
    if (persist) {
      this.safeSetSetting(this.storageKeys.detailsIconSize, String(clamped));
    }
  }

  getDefaultScreenshotFolder() {
    return (
      window.javaBridge?.getDefaultScreenshotFolder?.() ||
      this.safeGetSetting(this.storageKeys.detailsScreenshotFolder) ||
      ""
    );
  }

  updateScreenshotFolderDisplay() {
    if (!this.detailsScreenshotFolderRow) return;
    this.detailsScreenshotFolderRow.classList.toggle("isHidden", !this.saveScreenshotToFolder);
    if (this.detailsScreenshotFolderPath) {
      this.detailsScreenshotFolderPath.textContent =
        this.screenshotFolder || this.getDefaultScreenshotFolder() || "-";
    }
  }

  buildScreenshotFilename() {
    const now = new Date();
    const pad = (value) => String(value).padStart(2, "0");
    const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}_${pad(
      now.getHours()
    )}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
    return `AION2_DPS_${stamp}.png`;
  }

  toggleDetailsSettingsMenu(event) {
    if (!this.detailsSettingsMenu) return;
    if (this.detailsSettingsMenu.classList.contains("isOpen")) {
      this.closeDetailsSettingsMenu();
      return;
    }
    this.openDetailsSettingsMenu(event);
  }

  openDetailsSettingsMenu(event) {
    if (!this.detailsSettingsMenu) return;
    this.detailsSettingsMenu.classList.add("isOpen");
  }

  closeDetailsSettingsMenu() {
    if (!this.detailsSettingsMenu) return;
    this.detailsSettingsMenu.classList.remove("isOpen");
  }

  setupKeybindButtons() {
    const MOD_ALT = 1;
    const MOD_CONTROL = 2;
    const MOD_SHIFT = 4;
    const MOD_WIN = 8;

    const vkKeyName = (keyCode) => {
      if (keyCode >= 65 && keyCode <= 90) return String.fromCharCode(keyCode);
      if (keyCode >= 48 && keyCode <= 57) return String(keyCode - 48);
      if (keyCode >= 112 && keyCode <= 123) return `F${keyCode - 111}`;
      if (keyCode >= 96 && keyCode <= 105) return `Num${keyCode - 96}`;
      const names = {
        8: "Backspace", 9: "Tab", 13: "Enter", 19: "Pause", 20: "CapsLock",
        27: "Esc", 32: "Space", 33: "PgUp", 34: "PgDn", 35: "End", 36: "Home",
        37: "Left", 38: "Up", 39: "Right", 40: "Down",
        45: "Insert", 46: "Delete",
        106: "Num*", 107: "Num+", 109: "Num-", 110: "Num.", 111: "Num/",
        144: "NumLock", 145: "ScrollLock",
        186: ";", 187: "=", 188: ",", 189: "-", 190: ".", 191: "/",
        192: "`", 219: "[", 220: "\\", 221: "]", 222: "'",
      };
      return names[keyCode] || `Key${keyCode}`;
    };

    const formatBinding = (mods, keyCode) => {
      if (!mods && !keyCode) return "—";
      const parts = [];
      if (mods & MOD_CONTROL) parts.push("Ctrl");
      if (mods & MOD_ALT) parts.push("Alt");
      if (mods & MOD_SHIFT) parts.push("Shift");
      if (mods & MOD_WIN) parts.push("Win");
      if (keyCode) parts.push(vkKeyName(keyCode));
      return parts.join("+") || "—";
    };

    const isModifierKeyCode = (kc) =>
      kc === 16 || kc === 17 || kc === 18 || kc === 91 || kc === 92 ||
      kc === 93 || kc === 224;

    const reloadBtn = document.querySelector(".reloadKeybindBtn");
    const toggleBtn = document.querySelector(".toggleKeybindBtn");
    const lockKeyBtn = document.querySelector(".lockKeybindBtn");

    this.refreshKeybindLabels = () => {
      const reloadLabel = window.javaBridge?.getCurrentHotKey?.() || "";
      const toggleLabel = window.javaBridge?.getCurrentToggleWindowHotKey?.() || "";
      if (reloadBtn) {
        reloadBtn.querySelector(".keybindText").textContent = reloadLabel || "Ctrl+Alt+R";
      }
      if (toggleBtn) {
        toggleBtn.querySelector(".keybindText").textContent = toggleLabel || "Ctrl+Alt+Up";
      }
      if (lockKeyBtn) {
        lockKeyBtn.querySelector(".keybindText").textContent =
          window.javaBridge?.getCurrentLockHotKey?.() || "Ctrl+Alt+L";
      }
    };
    this.refreshKeybindLabels();

    let activeRecording = null;

    const stopRecording = () => {
      if (!activeRecording) return;
      activeRecording.btn.classList.remove("recording");
      activeRecording = null;
    };

    const startRecording = (btn, type) => {
      stopRecording();
      btn.classList.add("recording");
      btn.querySelector(".keybindText").textContent =
        this.i18n?.t?.("settings.keybind.pressKeys", "Press keys...") ?? "Press keys...";
      activeRecording = { btn, type };
    };

    const handleKeybindClick = (btn, type) => {
      if (activeRecording?.btn === btn) {
        stopRecording();
        this.refreshKeybindLabels();
        return;
      }
      startRecording(btn, type);
    };

    reloadBtn?.addEventListener("click", () => handleKeybindClick(reloadBtn, "reload"));
    toggleBtn?.addEventListener("click", () => handleKeybindClick(toggleBtn, "toggle"));
    lockKeyBtn?.addEventListener("click", () => handleKeybindClick(lockKeyBtn, "lock"));

    document.addEventListener("keydown", (event) => {
      if (!activeRecording) return;
      event.preventDefault();
      event.stopPropagation();

      if (event.key === "Escape") {
        stopRecording();
        this.refreshKeybindLabels();
        return;
      }

      if (isModifierKeyCode(event.keyCode)) {
        const parts = [];
        if (event.ctrlKey) parts.push("Ctrl");
        if (event.altKey) parts.push("Alt");
        if (event.shiftKey) parts.push("Shift");
        if (event.metaKey) parts.push("Win");
        activeRecording.btn.querySelector(".keybindText").textContent =
          parts.length ? parts.join("+") + "+..." : (this.i18n?.t?.("settings.keybind.pressKeys", "Press keys...") ?? "Press keys...");
        return;
      }

      let mods = 0;
      if (event.ctrlKey) mods |= MOD_CONTROL;
      if (event.altKey) mods |= MOD_ALT;
      if (event.shiftKey) mods |= MOD_SHIFT;
      if (event.metaKey) mods |= MOD_WIN;

      const modCount = ((mods & MOD_CONTROL) ? 1 : 0) +
        ((mods & MOD_ALT) ? 1 : 0) +
        ((mods & MOD_SHIFT) ? 1 : 0) +
        ((mods & MOD_WIN) ? 1 : 0);

      if (modCount < 1) {
        activeRecording.btn.querySelector(".keybindText").textContent =
          this.i18n?.t?.("settings.keybind.needModifiers", "Need 1+ modifier") ?? "Need 1+ modifier";
        setTimeout(() => {
          if (activeRecording) {
            activeRecording.btn.querySelector(".keybindText").textContent =
              this.i18n?.t?.("settings.keybind.pressKeys", "Press keys...") ?? "Press keys...";
          }
        }, 1200);
        return;
      }

      const vk = event.keyCode;
      const label = formatBinding(mods, vk);
      const { type } = activeRecording;

      stopRecording();

      if (type === "reload") {
        window.javaBridge?.setHotkey?.(mods, vk);
        if (reloadBtn) reloadBtn.querySelector(".keybindText").textContent = label;
      } else if (type === "toggle") {
        window.javaBridge?.setToggleWindowHotkey?.(mods, vk);
        if (toggleBtn) toggleBtn.querySelector(".keybindText").textContent = label;
      } else if (type === "lock") {
        window.javaBridge?.setLockHotkey?.(mods, vk);
        if (lockKeyBtn) lockKeyBtn.querySelector(".keybindText").textContent = label;
      }
    }, true);

    document.addEventListener("click", (event) => {
      if (!activeRecording) return;
      if (event.target?.closest?.(".keybindBtn")) return;
      stopRecording();
      this.refreshKeybindLabels();
    });
  }

  toggleSettingsPanel() {
    if (!this.settingsPanel) return;
    const isOpen = this.settingsPanel.classList.toggle("isOpen");
    if (isOpen) {
      this.pinnedDetailsRowId = null;
      this.hoveredDetailsRowId = null;
      this.detailsUI?.close?.({ keepPinned: false });
      this.refreshConnectionInfo();
      this.refreshKeybindLabels?.();
      this._loadDeviceDropdown();
    }
  }

  closeSettingsPanel() {
    this.settingsPanel?.classList.remove("isOpen");
  }

  // `manual` marks a name the player typed, which the backend accepts even
  // after the game has named the character (see the name field's handler).
  setUserName(name, { persist = false, syncBackend = false, manual = false } = {}) {
    const previousName = this.USER_NAME;
    const trimmed = String(name ?? "").trim();
    this.USER_NAME = trimmed;
    if (this.characterNameInput && document.activeElement !== this.characterNameInput) {
      this.characterNameInput.value = trimmed;
    }
    if (persist) {
      localStorage.setItem(this.storageKeys.userName, trimmed);
    }
    if (syncBackend) {
      window.javaBridge?.setCharacterName?.(trimmed, manual);
    }
    // A typed correction is not a character switch: the backend has already
    // put the name on the entity that is you, so keep the fight on screen
    // rather than resetting it.
    if (previousName && previousName !== trimmed && !manual) {
      const cachedId = this.getRecentLocalIdForName(trimmed);
      if (cachedId) {
        this.refreshDamageData({ reason: "local name update" });
        this.applyLocalPlayerIdUpdate(cachedId, "local name update cached id");
        return;
      }
      this.localPlayerId = null;
      if (this.localActorIdInput && document.activeElement !== this.localActorIdInput) {
        this.localActorIdInput.value = "";
      }
      this.refreshDamageData({ reason: "local name update" });
      this.reinitTargetSelection("local name update");
    }
    if (!this.isCollapse) {
      this.fetchDps();
    }
    this.refreshSettingsPanelIfOpen();
  }

  setOnlyShowUser(enabled, { persist = false } = {}) {
    this.onlyShowUser = !!enabled;
    if (persist) {
      localStorage.setItem(this.storageKeys.onlyShowUser, String(this.onlyShowUser));
    }
    if (!this.isCollapse) {
      this.fetchDps();
    }
  }

  setDebugLogging(enabled, { persist = false, syncBackend = false } = {}) {
    this.debugLoggingEnabled = !!enabled;
    if (this.debugLoggingCheckbox && document.activeElement !== this.debugLoggingCheckbox) {
      this.debugLoggingCheckbox.checked = this.debugLoggingEnabled;
    }
    if (persist) {
      this.safeSetSetting(this.storageKeys.debugLogging, String(this.debugLoggingEnabled));
    }
    if (syncBackend) {
      window.javaBridge?.setDebugLoggingEnabled?.(this.debugLoggingEnabled);
    }
  }

  setPinMeToTop(enabled, { persist = false } = {}) {
    this.pinMeToTop = !!enabled;
    if (this.pinMeToTopCheckbox && document.activeElement !== this.pinMeToTopCheckbox) {
      this.pinMeToTopCheckbox.checked = this.pinMeToTop;
    }
    if (persist) {
      this.safeSetSetting(this.storageKeys.pinMeToTop, String(this.pinMeToTop));
    }
    this.renderCurrentRows();
  }

  // The "details" window runs this same bundle. Rather than a second app, it
  // reuses the panel that already exists in index.html: the overlay chrome is
  // hidden by CSS and the panel is held open on the whole fight.
  /** "off" or a monitor index as a string. */
  getDetailsMonitorSetting() {
    const raw = this.safeGetSetting(this.storageKeys.detailsMonitor);
    if (raw === null || raw === undefined || String(raw).trim() === "") return "off";
    return String(raw);
  }

  async refreshMonitorList() {
    try {
      const list = await window.javaBridge?.listMonitors?.();
      this.monitorList = Array.isArray(list) ? list : [];
    } catch {
      this.monitorList = [];
    }
    return this.monitorList;
  }

  monitorLabel(monitor, position) {
    const n = Number.isFinite(position) ? position + 1 : Number(monitor?.index) + 1;
    const w = Math.round(Number(monitor?.width) || 0);
    const h = Math.round(Number(monitor?.height) || 0);
    const notes = [];
    if (monitor?.isPrimary) {
      notes.push(this.i18n?.t?.("settings.detailsMonitor.primary", "primary") ?? "primary");
    }
    // Where the screen physically sits — a resolution alone does not tell the
    // user which of their monitors they just selected.
    const side = String(monitor?.side || "");
    if (side) {
      notes.push(this.i18n?.t?.(`settings.detailsMonitor.side.${side}`, side) ?? side);
    }
    const suffix = notes.length ? ` (${notes.join(", ")})` : "";
    return `${n} — ${w}×${h}${suffix}`;
  }

  /**
   * Apply the "Show Details on monitor" setting. Opening is best-effort: if the
   * chosen display has been unplugged since it was saved, fall back to off
   * rather than leaving a window stranded off-screen.
   */
  async applyDetailsMonitor(value, { persist = false } = {}) {
    const next = value === "off" || value === null || value === undefined ? "off" : String(value);
    this.detailsMonitor = next;
    if (persist) {
      this.safeSetSetting(this.storageKeys.detailsMonitor, next);
    }
    if (next === "off") {
      await window.javaBridge?.closeDetailsWindow?.();
      this.setDetailsMonitorHint("");
      return;
    }
    const index = Number(next);
    const monitors = await this.refreshMonitorList();
    if (!Number.isFinite(index) || !monitors.some((m) => Number(m.index) === index)) {
      this.setDetailsMonitorHint(
        this.i18n?.t?.("settings.detailsMonitor.missing", "That display is not connected.") ??
          "That display is not connected."
      );
      await window.javaBridge?.closeDetailsWindow?.();
      this.detailsMonitor = "off";
      if (persist) this.safeSetSetting(this.storageKeys.detailsMonitor, "off");
      return;
    }
    try {
      await window.javaBridge?.openDetailsWindow?.(index);
      this.setDetailsMonitorHint("");
    } catch (err) {
      this.setDetailsMonitorHint(String(err?.message || err || ""));
    }
  }

  setDetailsMonitorHint(text) {
    if (!this.detailsMonitorHint) return;
    this.detailsMonitorHint.textContent = text || "";
    this.detailsMonitorHint.style.display = text ? "" : "none";
  }

  /**
   * Hand a Details view to the standalone Details window.
   *
   * Every route into Details from the overlay — a meter row, a fight picked in
   * History — goes through here, so there is exactly one Details surface and
   * the overlay never grows to contain the panel. The backend creates the
   * window if it is not up yet and places it where the user last left it.
   *
   * `fallback` is the old in-overlay behaviour, run when the window cannot be
   * reached at all (bridge without the command, window build failed). Showing
   * the panel in the overlay beats showing nothing.
   */
  openDetailsSurface(request, fallback) {
    const runFallback = () => {
      try { fallback?.(); } catch (err) { console.error("[A2Tools] details fallback failed", err); }
    };
    // A Details window is already the destination, so it renders in place
    // rather than asking for yet another window. The overlay and the History
    // window are both senders: History in particular must route, or picking a
    // fight would replace the list instead of opening beside it.
    if (window.A2_VIEW === "details") {
      runFallback();
      return;
    }
    const send = window.javaBridge?.requestDetailsView;
    if (typeof send !== "function") {
      runFallback();
      return;
    }
    let result;
    try {
      result = send.call(window.javaBridge, request);
    } catch (err) {
      console.error("[A2Tools] requestDetailsView failed", err);
      runFallback();
      return;
    }
    Promise.resolve(result).catch((err) => {
      console.error("[A2Tools] requestDetailsView failed", err);
      runFallback();
    });
  }

  // The "settings" window runs this same bundle with only the settings panel
  // visible, so its markup and wiring are reused rather than duplicated.
  enterSettingsWindowMode() {
    document.body.classList.add("isSettingsWindow");
    this.refreshMonitorList().then(() => this.initializeSettingsDropdowns());
    this._loadDeviceDropdown();
    this.settingsPanel?.classList.add("isOpen");
    // Closing is wired in startApp, before anything that can be slow.
    requestAnimationFrame(() => {
      requestAnimationFrame(() => window.javaBridge?.toolWindowReady?.("settings"));
    });
  }

  // The Battle History window: this bundle again, showing only the history
  // panel. It is a browser the user leaves open — picking a fight opens that
  // fight in a window of its own and the list stays exactly as it was, so
  // several fights can be read side by side.
  enterHistoryWindowMode() {
    document.body.classList.add("isHistoryWindow");
    const openList = () => this.historyUI?.open?.();
    openList();
    // open() reads a cache that an async prefetch fills, and this window is
    // created precisely in order to show the list — so it renders before its
    // own prefetch lands and would otherwise sit empty forever. Re-render once
    // the data is genuinely there. Safe to re-open blindly: this resolves in
    // the first moments of the window, before anyone can scroll or filter.
    Promise.resolve(window.javaBridge?.refreshFightHistory?.())
      .then(() => openList())
      .catch(() => {});

    // Frameless, so the panel's own × closes the window rather than just
    // hiding the list — there is nothing behind it here.
    document.querySelector(".historyClose")?.addEventListener("click", (event) => {
      event.stopPropagation();
      window.javaBridge?.closeToolWindow?.();
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape") window.javaBridge?.closeToolWindow?.();
    });

    // Nothing sits behind the list, so if something closes it, put it back.
    if (this._historyWindowTimer) clearInterval(this._historyWindowTimer);
    this._historyWindowTimer = setInterval(() => {
      if (!this.historyUI?.isOpen?.()) openList();
    }, 1000);

    requestAnimationFrame(() => {
      requestAnimationFrame(() => window.javaBridge?.toolWindowReady?.("history"));
    });
  }

  enterDetailsWindowMode() {
    document.body.classList.add("isDetailsWindow");
    // Two kinds of window run this mode: the single live "details" window, which
    // is the one the monitor setting governs, and a "details-<fightId>" window
    // showing one saved fight, of which there can be several.
    const label = window.__TAURI__?.window?.getCurrentWindow?.()?.label || "details";
    const isFightWindow = label.startsWith("details-");
    if (isFightWindow) document.body.classList.add("isFightWindow");

    // Frameless window, so the panel header supplies the close control.
    document.querySelector(".detailsWindowClose")?.addEventListener("click", () => {
      if (isFightWindow) {
        // One of several, and nothing to do with the monitor setting.
        window.javaBridge?.closeToolWindow?.();
        return;
      }
      // Closing the live window also turns the setting off, otherwise it would
      // reopen on next launch.
      this.safeSetSetting(this.storageKeys.detailsMonitor, "off");
      window.javaBridge?.closeDetailsWindow?.();
    });
    const openAll = () => {
      this.detailsUI?.open?.(null, { defaultTargetAll: true, pin: true, force: true });
    };
    // Confirm on the screen itself which monitor this is. Backed by the
    // placement the backend actually performed, not by what was requested.
    window.__TAURI__?.event?.listen?.("details-placed", (event) => {
      const badge = document.querySelector(".detailsMonitorBadge");
      if (!badge) return;
      const p = event?.payload || {};
      const numEl = badge.querySelector(".detailsMonitorBadgeNum");
      const textEl = badge.querySelector(".detailsMonitorBadgeText");
      if (numEl) numEl.textContent = String(p.number ?? "");
      if (textEl) {
        const label = this.i18n?.t?.("settings.detailsMonitor.badge", "Monitor") ?? "Monitor";
        const primary = p.isPrimary
          ? ` · ${this.i18n?.t?.("settings.detailsMonitor.primary", "primary") ?? "primary"}`
          : "";
        textEl.textContent = `${label} ${p.number ?? ""} · ${p.width}×${p.height}${primary}`;
      }
      badge.classList.remove("isVisible");
      void badge.offsetWidth; // restart the fade if it fires twice
      badge.classList.add("isVisible");
    });

    // Views routed here from the overlay (a meter row, a fight from History).
    // The request arrives two ways and only one of them can be relied on: a
    // window that was created to serve the request has no listener attached
    // when it is emitted, so it pulls the parked request on startup instead.
    // The seq stamp makes applying it twice a no-op.
    this._lastDetailsRequestSeq = 0;
    this._detailsRequestsInFlight = 0;
    const applyRequest = async (payload) => {
      if (!payload || typeof payload !== "object") return;
      const seq = Number(payload.seq) || 0;
      if (seq && seq <= this._lastDetailsRequestSeq) return;
      this._lastDetailsRequestSeq = seq;
      // Hold off the keep-open watchdog: a fight has to be loaded from disk
      // before the panel opens, and the panel is closed until it lands.
      this._detailsRequestsInFlight += 1;
      try {
        if (payload.kind === "fight") {
          const raw = await window.javaBridge?.getFightDetails?.(String(payload.fightId || ""));
          const record = typeof raw === "string" ? this.safeParseJSON(raw, null) : raw;
          if (!record) {
            openAll();
            return;
          }
          this.detailsUI?.resetDetailsMode?.();
          await this.detailsUI?.openHistoryFight?.(record);
          return;
        }
        const row = payload.row && typeof payload.row === "object" ? payload.row : null;
        await this.detailsUI?.open?.(row, {
          force: true,
          pin: true,
          defaultTargetAll: !!payload.defaultTargetAll,
          defaultTargetId: payload.defaultTargetId ?? null,
        });
      } catch (err) {
        console.error("[A2Tools] details request failed", err);
      } finally {
        this._detailsRequestsInFlight = Math.max(0, this._detailsRequestsInFlight - 1);
      }
    };
    const listening = window.__TAURI__?.event?.listen?.(
      "details-request",
      (event) => { void applyRequest(event?.payload); }
    );

    // A fight window has a saved fight waiting for it, so showing live combat
    // first would only flash the wrong content. The fight branch of
    // applyRequest falls back to openAll() if the record cannot be loaded, so
    // this cannot strand the window empty.
    if (!isFightWindow) openAll();
    // Tell the backend to reveal the window now that the panel has painted.
    // Sequenced after the listener so a request emitted meanwhile is not lost.
    Promise.resolve(listening)
      .catch(() => {})
      .then(() => window.javaBridge?.takePendingDetailsRequest?.())
      .then((pending) => (pending ? applyRequest(pending) : undefined))
      .catch(() => {})
      .then(() => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => window.javaBridge?.detailsWindowReady?.());
        });
      });
    // The panel can be closed from inside (back button, escape). In this window
    // there is nothing behind it, so re-assert rather than leave a blank screen.
    // History counts as content: it is the other panel that legitimately fills
    // this window, and reopening Details over it would fight the back button.
    // Only the live window: openAll() shows current combat, which in a window
    // opened to display one saved fight would quietly replace that fight.
    if (!isFightWindow) {
      if (this._detailsWindowTimer) clearInterval(this._detailsWindowTimer);
      this._detailsWindowTimer = setInterval(() => {
        if (this._detailsRequestsInFlight > 0) return;
        if (!this.detailsPanel?.classList?.contains("open")) openAll();
      }, 1000);
    }
  }

  getMeterLayout() {
    const base = this.betaUi ? "beta" : "classic";
    return this.slimMode ? `${base}Slim` : base;
  }

  setMeterLayout(value, { persist = false } = {}) {
    const beta = String(value).startsWith("beta");
    const slim = String(value).endsWith("Slim");
    this.setBetaUi(beta, { persist });
    this.setSlimMode(slim, { persist });
  }

  // Beta UI is the redesigned main window and is the default. Switching it off
  // adds body.legacyUi, which activates the pre-redesign skin in styles.css.
  setBetaUi(enabled, { persist = false } = {}) {
    this.betaUi = !!enabled;
    document.body.classList.toggle("legacyUi", !this.betaUi);
    // Both skins show the same placeholder text; only the type scale and the
    // uppercase transform differ, so the fitted size has to be recomputed.
    this.fitBossName();
    if (persist) {
      this.safeSetSetting(this.storageKeys.betaUi, String(this.betaUi));
    }
  }

  setSlimMode(enabled, { persist = false } = {}) {
    this.slimMode = !!enabled;
    document.querySelector(".meter")?.classList.toggle("slim", this.slimMode);
    if (persist) {
      this.safeSetSetting(this.storageKeys.slimMode, String(this.slimMode));
    }
  }

  setMainPlayerNamesBold(enabled, { persist = false } = {}) {
    this.mainPlayerNamesBold = !!enabled;
    if (this.playerNamesBoldCheckbox && document.activeElement !== this.playerNamesBoldCheckbox) {
      this.playerNamesBoldCheckbox.checked = this.mainPlayerNamesBold;
    }
    document.body?.classList.toggle("mainPlayerNamesBold", this.mainPlayerNamesBold);
    if (persist) {
      this.safeSetSetting(this.storageKeys.mainPlayerNamesBold, String(this.mainPlayerNamesBold));
    }
  }

  setMainPlayerDpsBold(enabled, { persist = false } = {}) {
    this.mainPlayerDpsBold = !!enabled;
    if (this.playerDpsBoldCheckbox && document.activeElement !== this.playerDpsBoldCheckbox) {
      this.playerDpsBoldCheckbox.checked = this.mainPlayerDpsBold;
    }
    document.body?.classList.toggle("mainPlayerDpsBold", this.mainPlayerDpsBold);
    if (persist) {
      this.safeSetSetting(this.storageKeys.mainPlayerDpsBold, String(this.mainPlayerDpsBold));
    }
  }

  setTargetSelection(mode, { persist = false, syncBackend = false, reason = "update" } = {}) {
    const previousSelection = this.targetSelection;
    this.targetSelection = ["bossTargets", "lastHitByMe", "allTargets", "trainTargets"].includes(mode)
      ? mode
       : "lastHitByMe";
    if (persist) {
      this.safeSetStorage(this.storageKeys.targetSelection, String(this.targetSelection));
    }
    if (syncBackend) {
      window.javaBridge?.setTargetSelection?.(this.targetSelection);
    }
    if (previousSelection !== this.targetSelection) {
      this.logDebug(
        `Target selection changed: "${previousSelection}" -> "${this.targetSelection}" (reason: ${reason}).`
      );
      this._lastTargetSelection = this.targetSelection;
    }
    // Leaving Boss mode drops "Waiting for a boss" at once, rather than on
    // the next update from the backend.
    if (this.targetSelection !== "bossTargets" && this._waitingForBoss) {
      this._waitingForBoss = false;
      this.updateConnectionStatusUi();
    }
    this.updateTargetModeButton();
  }

  applyTheme(themeId, { persist = false } = {}) {
    const normalized = this.availableThemes.includes(themeId) ? themeId : this.availableThemes[0];
    this.theme = normalized;
    document.documentElement.dataset.theme = normalized;
    if (this.settingsSelections) {
      this.settingsSelections.theme = normalized;
    }
    if (persist) {
      this.safeSetSetting(this.storageKeys.theme, normalized);
    }
  }

  setDisplayMode(mode, { persist = false } = {}) {
    this.displayMode = mode === "totalDamage" || mode === "both" ? mode : "dps";
    if (persist) {
      this.safeSetStorage(this.storageKeys.displayMode, this.displayMode);
    }
    this.updateDisplayToggleLabel();
  }

  updateDisplayToggleLabel() {
    if (!this.metricToggleBtn) return;
    const [labelKey, labelFallback, ariaKey, ariaFallback] = {
      totalDamage: ["header.display.total", "DMG", "header.display.ariaDamage", "Showing total damage"],
      both: ["header.display.both", "BOTH", "header.display.ariaBoth", "Showing DPS and total damage"],
      dps: ["header.display.dps", "DPS", "header.display.ariaDps", "Showing DPS"],
    }[this.displayMode];
    const label = this.i18n?.t(labelKey, labelFallback) ?? labelFallback;
    const ariaLabel = this.i18n?.t(ariaKey, ariaFallback) ?? ariaFallback;
    this.metricToggleBtn.textContent = label;
    this.metricToggleBtn.setAttribute("aria-label", ariaLabel);
  }

  // Boss remaining-HP bar. There is no live boss current-HP packet, so remaining
  // is derived from spawn-time max HP minus the damage the meter has tracked
  // against this target. Hidden unless a single boss target with known max HP.
  updateBossHpBar(maxHp, totalDamage, currentHp) {
    if (!this.elBossHpBar) return;
    const max = Number(maxHp) || 0;
    if (max <= 0) {
      if (this.elBossHpBar.style.display !== "none") {
        this.elBossHpBar.style.display = "none";
      }
      return;
    }
    // Prefer the real current-HP feed when we have one (>= 0); otherwise fall back
    // to deriving remaining = max - tracked damage.
    const live = Number(currentHp);
    const remaining =
      Number.isFinite(live) && live >= 0
        ? Math.min(max, Math.max(0, live))
        : Math.max(0, max - Math.max(0, Number(totalDamage) || 0));
    const pct = Math.max(0, Math.min(100, (remaining / max) * 100));
    if (this.elBossHpBar.style.display === "none") {
      this.elBossHpBar.style.display = "";
    }
    if (this.elBossHpFill) {
      this.elBossHpFill.style.width = `${pct.toFixed(1)}%`;
    }
    // Color shifts as the boss is whittled down.
    this.elBossHpBar.classList.toggle("isMid", pct > 25 && pct <= 50);
    this.elBossHpBar.classList.toggle("isLow", pct <= 25);
    if (this.elBossHpText) {
      // Split to the two ends of the band: the percentage rides the fill,
      // the absolute HP sits on the track. A single centred label was
      // getting sliced in half by the fill's leading edge at mid HP.
      const pctEl = this.elBossHpText.querySelector(".bossHpPct");
      const amtEl = this.elBossHpText.querySelector(".bossHpAmt");
      if (pctEl && amtEl) {
        pctEl.textContent = `${Math.round(pct)}%`;
        amtEl.textContent = `${this.formatAbbreviatedNumber(remaining)} / ${this.formatAbbreviatedNumber(max)}`;
      } else {
        this.elBossHpText.textContent = `${this.formatAbbreviatedNumber(remaining)} · ${Math.round(pct)}%`;
      }
    }
  }

  formatAbbreviatedNumber(value) {
    const n = Number(value);
    if (!Number.isFinite(n)) return "-";
    const abs = Math.abs(n);
    const units = [
      { value: 1e12, suffix: "t" },
      { value: 1e9, suffix: "b" },
      { value: 1e6, suffix: "m" },
      { value: 1e3, suffix: "k" },
    ];
    for (const unit of units) {
      if (abs >= unit.value) {
        const scaled = (n / unit.value).toFixed(2);
        const trimmed = scaled.replace(/\.?0+$/, "");
        return `${trimmed}${unit.suffix}`;
      }
    }
    return this.dpsFormatter.format(n);
  }

  // DPS to the nearest thousand: 1,012,326 reads as "1,012k", 554,874 as "555k".
  // Same rule as the combat power beside the name, so the two numbers on a row
  // are at the same precision. Below 500 the abbreviation would collapse to
  // "0k", so show the raw figure there instead. Turned off by the "Round DPS"
  // setting, which falls back to the exact figure.
  formatDpsThousands(value) {
    const n = Number(value);
    if (!Number.isFinite(n)) return "-";
    if (!this.roundDps) return this.dpsFormatter.format(Math.round(n));
    const thousands = Math.round(n / 1000);
    return thousands > 0
      ? `${this.dpsFormatter.format(thousands)}k`
      : this.dpsFormatter.format(Math.round(n));
  }

  refreshDamageData({ reason = "refresh" } = {}) {
    this.refreshPending = true;
    this.refreshPendingStartedAt = this.nowMs();
    this.lastSnapshot = null;
    this.lastJson = null;
    this.lastTargetMode = "";
    this.lastTargetName = "";
    this.lastTargetId = 0;
    this._lastRenderedListSignature = "";
    this._lastRenderedTargetLabel = "";
    this._lastRenderedRowsSummary = null;
    this.pinnedDetailsRowId = null;
    this.hoveredDetailsRowId = null;
    this.setWindowDragFreeze(false);
    this.setMeterHoverFreeze(false);
    this.detailsUI?.close?.({ keepPinned: false });
    this.lastSnapshot = [];
    this._lastRenderedRowsSummary = null;
    this._lastRenderedListSignature = "";
    this.meterUI?.onResetMeterUi?.();
    this.renderCurrentRows();

    if (this.elBossName) {
      this.elBossName.textContent = this.getDefaultTargetLabel(this.targetSelection);
    }

    const lastParsedAtMs = Number(window.javaBridge?.getLastParsedAtMs?.());
    if (Number.isFinite(lastParsedAtMs) && lastParsedAtMs > 0) {
      const idleMs = Date.now() - lastParsedAtMs;
      if (idleMs > 30_000) {
        window.javaBridge?.resetAutoDetection?.();
      }
    }

    window.javaBridge?.resetDps?.();
    window.javaBridge?.restartTargetSelection?.();
    this.logDebug(`Damage data refreshed (${reason}).`);
  }

  // Settings are edited in a separate window, so this window has to be told
  // when one changes or it keeps rendering with the value it read at startup.
  //
  // Every window loads the same document, so the control for the setting exists
  // here too, already wired to the handler that applies it. Rather than
  // duplicate that logic, set the control to the incoming value and fire the
  // same event a click would — one code path for local and remote changes.
  //
  // Only the options that change what the meter draws are listed. Custom
  // dropdowns (theme, layout, player limit) are not native inputs and need
  // their own handling, so they are deliberately absent.
  applyRemoteSettingChange(key, value) {
    // A name typed in the Settings window. That window already told the
    // backend; this one only has to stop believing the old name, or it would
    // push the old one straight back.
    if (key === this.storageKeys.userName) {
      const name = String(value ?? "").trim();
      if (name === this.USER_NAME) return;
      this.USER_NAME = name;
      if (this.characterNameInput && document.activeElement !== this.characterNameInput) {
        this.characterNameInput.value = name;
      }
      this.renderCurrentRows();
      return;
    }
    const selector = REMOTE_APPLIED_SETTING_CONTROLS[key];
    if (!selector) return;
    const control = document.querySelector(selector);
    if (!control) return;

    // Bail when the value already matches — this is what stops the echo. The
    // handler below writes the setting straight back, and the backend only
    // broadcasts real changes, so the round trip ends here.
    if (control.type === "checkbox") {
      const next = value !== "false";
      if (control.checked === next) return;
      control.checked = next;
    } else {
      if (String(control.value) === String(value)) return;
      control.value = value;
    }
    control.dispatchEvent(new Event("change", { bubbles: true }));
    if (control.type === "range") {
      control.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }

  getMetricForRow(row) {
    if (this.displayMode === "totalDamage") {
      const totalDamage = Number(row?.totalDamage) || 0;
      return {
        value: totalDamage,
        text: this.formatAbbreviatedNumber(totalDamage),
      };
    }
    const dps = Number(row?.dps) || 0;
    const dpsText = `${this.formatDpsThousands(dps)}${this.i18n?.t("meter.dpsSuffix", "/s") ?? "/s"}`;
    if (this.displayMode === "both") {
      // "408k (13k/s)", as a player asked: damage leads, so the bars follow it.
      const totalDamage = Number(row?.totalDamage) || 0;
      return { value: totalDamage, text: `${this.formatAbbreviatedNumber(totalDamage)} (${dpsText})` };
    }
    return { value: dps, text: dpsText };
  }

  updateMeterTotalBar(rows) {
    if (!this.meterTotalBar) return;
    if (!this.showTotalDps || !Array.isArray(rows) || rows.length <= 1) {
      this.meterTotalBar.style.display = "none";
      return;
    }
    const totalDmg = rows.reduce((sum, r) => sum + (Number(r?.totalDamage) || 0), 0);
    const totalDps = rows.reduce((sum, r) => sum + (Number(r?.dps) || 0), 0);
    this.meterTotalBar.style.display = "";
    if (this.meterTotalDpsEl) {
      // Matches the per-row readout directly above it; a full-precision total
      // over abbreviated rows reads as two different units.
      this.meterTotalDpsEl.textContent = `${this.formatDpsThousands(totalDps)}${this.i18n?.t("meter.dpsSuffix", "/s") ?? "/s"}`;
    }
    if (this.meterTotalDmgEl) {
      this.meterTotalDmgEl.textContent = this.formatAbbreviatedNumber(totalDmg);
    }
  }

  // "Send logs to dev": the newest packet captures, for a bug report. Packet
  // logs are raw game traffic, names included, so it asks first, every time.
  initSendLogs() {
    const btn = this.sendLogsBtn;
    const status = this.sendLogsStatus;
    if (!btn || btn.dataset.wired) return;
    btn.dataset.wired = "1";
    const t = (key, fallback, vars) =>
      vars ? (window.i18n?.format?.(key, vars, fallback) ?? fallback)
           : (window.i18n?.t?.(key, fallback) ?? fallback);
    const show = (text) => {
      if (!status) return;
      status.removeAttribute("data-i18n");
      status.textContent = text;
    };
    btn.addEventListener("click", async () => {
      const ok = window.confirm(t("settings.sendLogs.confirm",
        "Send your 3 newest packet logs to the A2 Tools developer?\n\n" +
        "Packet logs are raw game traffic recorded while packet logging was on, " +
        "including character names. Only the developer can open them, and they " +
        "are deleted after 30 days."));
      if (!ok) return;
      btn.disabled = true;
      show(t("settings.sendLogs.sending", "Sending..."));
      try {
        const result = await window.javaBridge?.sendLogsToDev?.();
        const code = result?.code || "?";
        show(t("settings.sendLogs.sent",
          `Sent. Your report code is ${code}: give it to the developer on Discord.`,
          { code }));
      } catch (err) {
        const msg = String(err?.message || err || "");
        show(t("settings.sendLogs.failed", `Could not send: ${msg}`, { error: msg }));
      } finally {
        btn.disabled = false;
      }
    });
  }

  initPlayerLimitDropdown() {
    const wrapper = document.querySelector(".playerLimitDropdownWrapper");
    if (!wrapper) return;
    const btn = wrapper.querySelector(".playerLimitDropdownBtn");
    const menu = wrapper.querySelector(".playerLimitDropdownMenu");
    const textEl = btn?.querySelector(".settingsDropdownText");
    if (!btn || !menu || !textEl) return;

    const options = [4, 5, 6, 7, 8, 10, 12];
    textEl.textContent = String(this.playerLimit);

    for (const val of options) {
      const item = document.createElement("div");
      item.className = "settingsDropdownItem";
      item.textContent = String(val);
      item.dataset.value = String(val);
      if (val === this.playerLimit) item.classList.add("isActive");
      item.addEventListener("click", () => {
        this.playerLimit = val;
        this.safeSetSetting(this.storageKeys.playerLimit, String(val));
        textEl.textContent = String(val);
        menu.querySelectorAll(".settingsDropdownItem").forEach((el) =>
          el.classList.toggle("isActive", el.dataset.value === String(val))
        );
        menu.style.display = "none";
        this.renderCurrentRows();
      });
      menu.appendChild(item);
    }

    btn.addEventListener("click", () => {
      // The menu starts hidden by the stylesheet with no inline display, so
      // test for "open" rather than "closed" or the first click does nothing.
      menu.style.display = menu.style.display === "flex" ? "none" : "flex";
    });
    document.addEventListener("click", (e) => {
      if (!wrapper.contains(e.target)) menu.style.display = "none";
    });
  }

  renderCurrentRows() {
    if (this.isCollapse) return;
    let rowsToRender = Array.isArray(this.lastSnapshot) ? this.lastSnapshot : [];
    if (this.lastTargetMode === "trainTargets" && this.isLocalUserIdentified()) {
      rowsToRender = rowsToRender.filter((row) => row.name === this.USER_NAME);
    }
    const rowsSummary = this.getRowsSummary(rowsToRender);
    if (rowsSummary.listSignature !== this._lastRenderedListSignature) {
      const reasons = this.describeRowsChange(rowsSummary, this._lastRenderedRowsSummary);
      if (!Array.isArray(this.lastSnapshot) || this.lastSnapshot.length === 0) {
        reasons.push("no snapshot available");
      } else {
        reasons.push("renderCurrentRows refresh");
      }
      this.logDebug(
        `Meter list changed (${rowsToRender.length} rows). reason: ${
          reasons.join("; ") || "list membership changed"
        }.`
      );
      this._lastRenderedListSignature = rowsSummary.listSignature;
      this._lastRenderedRowsSummary = rowsSummary;
    }
    this.updateMeterTotalBar(rowsToRender);
    this.meterUI?.updateFromRows?.(rowsToRender);
  }

  getDefaultDetailsOpenOptions() {
    const numericLastTargetId = Number(this.lastTargetId);
    const hasConcreteTarget = Number.isFinite(numericLastTargetId) && numericLastTargetId > 0;
    const fallbackAllTargets =
      this.lastTargetMode === "lastHitByMe" &&
      !hasConcreteTarget &&
      !this.lastTargetName;
    const isTrainTargets = this.lastTargetMode === "trainTargets";
    const shouldDefaultAllTrainTargets = isTrainTargets && this.trainSelectionMode === "all";
    const shouldDefaultTrainTarget = isTrainTargets && this.trainSelectionMode === "highestDamage";
    const defaultTargetId = shouldDefaultTrainTarget
      ? (hasConcreteTarget ? numericLastTargetId : null)
      : hasConcreteTarget
        ? numericLastTargetId
        : null;
    return {
      defaultTargetAll: fallbackAllTargets || shouldDefaultAllTrainTargets || (!hasConcreteTarget && this.lastTargetMode === "allTargets"),
      defaultTargetId,
    };
  }

  refreshConnectionInfo({ skipSettingsRefresh = false } = {}) {
    if (!this.lockedIp || !this.lockedPort) return;
    const raw = window.javaBridge?.getConnectionInfo?.();
    if (typeof raw !== "string") {
      this.lockedIp.textContent = "-";
      this.lockedPort.textContent = "-";
      if (this.localActorIdInput && document.activeElement !== this.localActorIdInput) {
        this.localActorIdInput.value = "";
      }
      this.isDetectingPort = this.aionRunning;
      this.updateConnectionStatusUi();
      if (!skipSettingsRefresh) {
        this.refreshSettingsPanelIfOpen();
      }
      return;
    }
    const info = this.safeParseJSON(raw, {});

    // Show Npcap error if present
    const pcapErr = typeof info?.pcapError === "string" ? info.pcapError.trim() : "";
    if (pcapErr) {
      this.lockedIp.textContent = "";
      this.lockedPort.textContent = pcapErr;
      this.lockedPort.classList.add("isPcapError");
      this.updateConnectionStatusUi();
      if (!skipSettingsRefresh) this.refreshSettingsPanelIfOpen();
      return;
    }
    this.lockedPort.classList.remove("isPcapError");

    const deviceName = typeof info?.device === "string" && info.device.trim() ? info.device : "";
    const rawIp = info?.ip || "-";
    const ip =
      deviceName ||
      (rawIp === "127.0.0.1" || rawIp === "::1"
        ? this.i18n?.t("connection.loopback", "Local Loopback") ?? "Local Loopback"
        : rawIp);
    const hasPort = Number.isFinite(Number(info?.port));
    this.isDetectingPort = this.aionRunning && !hasPort;
    const port = hasPort
      ? String(info.port)
      : this.isDetectingPort
        ? this.i18n?.t("connection.detecting", "Detecting AION2 connection...")
        : this.i18n?.t("connection.auto", "Auto");
    this.lockedIp.textContent = ip;
    this.lockedPort.textContent = port;
    // Keep the device dropdown text in sync with the currently locked device
    if (this._autoDetectDevice && deviceName && this.deviceDropdownBtn) {
      const textEl = this.deviceDropdownBtn.querySelector(".settingsDropdownText");
      if (textEl) textEl.textContent = deviceName;
    }
    const localPlayerId = Number(info?.localPlayerId);
    this.localPlayerId = Number.isFinite(localPlayerId) && localPlayerId > 0
      ? Math.trunc(localPlayerId)
      : null;
    if (this.localActorIdInput && document.activeElement !== this.localActorIdInput) {
      this.localActorIdInput.value = this.localPlayerId ? String(this.localPlayerId) : "";
    }
    // Never under the player's cursor: they may be typing a new name.
    // This window's own name first: it changes the moment the player saves
    // one, while the backend's copy here is a poll up to 3 s old and would
    // put the old name back. The game's name reaches USER_NAME through
    // syncCharacterNameFromGame, so nothing is lost.
    if (this.characterNameInput && document.activeElement !== this.characterNameInput) {
      const nickname = String(this.USER_NAME || info?.characterName || "").trim();
      this.characterNameInput.value = nickname;
    }
    // Do NOT reinitTargetSelection() when the backend-reported local id changes:
    // the local player churns entity ids mid-fight, and reinit → reset_combat
    // wipes all combat data and every teammate nickname. The "you" highlight
    // follows this.localPlayerId on the next dps render with no reset.
    this.updateConnectionStatusUi();
    if (!skipSettingsRefresh) {
      this.refreshSettingsPanelIfOpen();
    }
  }

  refreshSettingsPanelIfOpen() {
    if (!this.settingsPanel?.classList.contains("isOpen")) return;
    this.refreshConnectionInfo({ skipSettingsRefresh: true });
    this.updateSettingsVersion();
  }

  _updateDeviceDropdownState() {
    const disabled = this._autoDetectDevice;
    if (this.deviceDropdownBtn) this.deviceDropdownBtn.disabled = disabled;
    if (disabled && this.deviceDropdownMenu) this.deviceDropdownMenu.classList.remove("isOpen");
  }

  // The device list comes from the backend asynchronously, so fill the dropdown
  // once it has arrived rather than from whatever the cache held at the time.
  _loadDeviceDropdown() {
    const populate = () =>
      this._populateDeviceDropdown(this.safeGetSetting("dpsMeter.manualDevice") || null);
    const load = window.javaBridge?.loadAvailableDevices?.();
    if (!load) {
      populate();
      return;
    }
    load.then(populate, populate);
  }

  _populateDeviceDropdown(currentDevice) {
    if (!this.deviceDropdownBtn || !this.deviceDropdownMenu) return;
    const raw = window.javaBridge?.getAvailableDevices?.();
    const devices = typeof raw === "string" ? this.safeParseJSON(raw, []) : [];
    if (!Array.isArray(devices) || devices.length === 0) return;
    const options = devices.map((d) => ({ value: d, label: d }));
    // When auto-detect is on, show the currently locked device; otherwise show the manual selection
    const connRaw = window.javaBridge?.getConnectionInfo?.();
    const connInfo = typeof connRaw === "string" ? this.safeParseJSON(connRaw, {}) : {};
    const lockedDevice = typeof connInfo?.device === "string" && connInfo.device.trim() ? connInfo.device : "";
    // Unticking auto-detect leaves capture where it is until a device is picked,
    // so show that device rather than whichever happens to be listed first.
    const selected = this._autoDetectDevice
      ? (lockedDevice || currentDevice || devices[0])
      : (currentDevice || lockedDevice || devices[0]);
    this.deviceDropdownMenu.innerHTML = "";
    options.forEach((opt) => {
      const item = document.createElement("button");
      item.type = "button";
      item.className = "settingsDropdownItem";
      item.dataset.value = opt.value;
      item.textContent = opt.label;
      if (opt.value === selected) item.classList.add("isActive");
      item.addEventListener("click", () => {
        window.javaBridge?.setManualDevice?.(opt.value);
        this.deviceDropdownMenu.classList.remove("isOpen");
        const textEl = this.deviceDropdownBtn.querySelector(".settingsDropdownText");
        if (textEl) textEl.textContent = opt.label;
        this.deviceDropdownMenu.querySelectorAll(".settingsDropdownItem").forEach((el) =>
          el.classList.toggle("isActive", el.dataset.value === opt.value)
        );
        this.refreshConnectionInfo();
      });
      this.deviceDropdownMenu.appendChild(item);
    });
    const textEl = this.deviceDropdownBtn.querySelector(".settingsDropdownText");
    if (textEl) textEl.textContent = selected;
    this.deviceDropdownBtn.onclick = (event) => {
      if (this.deviceDropdownBtn.disabled) return;
      event.stopPropagation();
      // Close other dropdowns
      document.querySelectorAll(".settingsDropdownMenu.isOpen").forEach((menu) => {
        if (menu !== this.deviceDropdownMenu) menu.classList.remove("isOpen");
      });
      this.deviceDropdownMenu.classList.toggle("isOpen");
    };
  }

  updateSettingsVersion() {
    if (!this.settingsVersionValue) return;
    const rawVersion = String(window.dpsData?.getVersion?.() || "").trim();
    const normalized = rawVersion.replace(/^v/i, "");
    this.settingsVersionValue.textContent = normalized ? `v${normalized}` : "-";
    // The backend version fetch is async; retry briefly if it wasn't ready yet.
    if (!normalized && !this._versionRetryScheduled) {
      this._versionRetryScheduled = true;
      setTimeout(() => {
        this._versionRetryScheduled = false;
        this.updateSettingsVersion();
      }, 300);
    }
  }

  fitBossName() {
    const el = this.elBossName;
    if (!el) return;
    const container = el.parentElement;
    if (!container) return;
    // The stylesheet owns the boss-name size; this only ever shrinks a long
    // name to fit. It used to start from a hard-coded 18px (16 slim) and write
    // that as an inline style, which silently overrode whatever the CSS asked
    // for — so the header label ignored the design's type scale entirely.
    el.style.fontSize = "";
    const base = Number.parseFloat(getComputedStyle(el).fontSize);
    if (!Number.isFinite(base) || base <= 0) return;
    if (el.scrollWidth <= container.clientWidth) return;
    const minFs = Math.max(7, base * 0.72);
    for (let fs = base - 0.5; fs >= minFs; fs -= 0.5) {
      el.style.fontSize = `${fs}px`;
      if (el.scrollWidth <= container.clientWidth) return;
    }
  }

  updatePing(pushedMs) {
    if (!this.pingEl) return;
    if (!this.showPing) {
      this.pingEl.classList.remove("isVisible");
      return;
    }
    const ms = typeof pushedMs === "number" ? pushedMs : window.javaBridge?.getPingMs?.();
    if (typeof ms !== "number" || ms < 0) {
      this.pingEl.classList.remove("isVisible");
      return;
    }
    const textEl = this.pingEl.querySelector(".pingText");
    if (textEl) textEl.textContent = `${ms}ms`;
    else this.pingEl.textContent = `${ms}ms`;
    this.pingEl.classList.add("isVisible");
    this.pingEl.classList.remove("ping-good", "ping-warn", "ping-high", "ping-bad");
    this.pingEl.classList.add(
      ms < 100 ? "ping-good" : ms < 200 ? "ping-warn" : ms < 225 ? "ping-high" : "ping-bad"
    );
  }

  updateConnectionStatusUi() {
    if (!this.battleTimeRoot || !this.analysisStatusEl) return;
    if (!this.aionRunning) {
      this.applyConnectionStatusOverride(
        this.i18n?.t("battleTime.notRunning", "AION2 not running") ?? "AION2 not running"
      );
      return;
    }
    if (this.isDetectingPort) {
      this.applyConnectionStatusOverride(
        this.i18n?.t("connection.detecting", "Detecting AION2 connection...") ??
          "Detecting AION2 connection..."
      );
      return;
    }
    if (this._waitingForBoss && this.targetSelection === "bossTargets" && !this._captureSuspended) {
      this.applyConnectionStatusOverride(
        this.i18n?.t("battleTime.waitingBoss", "Waiting for a boss") ?? "Waiting for a boss"
      );
      return;
    }
    this.clearConnectionStatusOverride();
  }

  applyConnectionStatusOverride(text) {
    this._connectionStatusOverride = true;
    this.battleTimeRoot.classList.add("isVisible", "state-idle");
    this.analysisStatusEl.textContent = text;
    this.analysisStatusEl.style.display = "";
  }

  clearConnectionStatusOverride() {
    if (!this._connectionStatusOverride) return;
    this._connectionStatusOverride = false;
    this.battleTimeRoot.classList.remove("state-idle");
    if (!this._battleTimeVisible) {
      this.battleTimeRoot.classList.remove("isVisible");
    }
    if (this._captureSuspended) {
      this.analysisStatusEl.textContent =
        this.i18n?.t?.("battleTime.suspended", "App suspended") ?? "App suspended";
    } else {
      this.analysisStatusEl.textContent =
        this.i18n?.t("battleTime.analysing", "Ready - monitoring combat...") ??
        "Ready - monitoring combat...";
    }
    this.analysisStatusEl.style.removeProperty("display");
  }

  getRowsSummary(rows) {
    const safeRows = Array.isArray(rows) ? rows : [];
    const ids = safeRows.map((row) => String(row?.id ?? "")).sort();
    const names = safeRows.map((row) => String(row?.name ?? "")).sort();
    return {
      count: safeRows.length,
      ids,
      names,
      listSignature: ids.join("|"),
    };
  }

  describeRowsChange(nextSummary, previousSummary) {
    const reasons = [];
    if (!previousSummary) {
      reasons.push("initial meter render");
      return reasons;
    }
    if (previousSummary.count !== nextSummary.count) {
      reasons.push(`row count ${previousSummary.count} -> ${nextSummary.count}`);
    }
    const idsChanged =
      previousSummary.ids.length !== nextSummary.ids.length ||
      previousSummary.ids.some((id, index) => id !== nextSummary.ids[index]);
    if (idsChanged) {
      reasons.push("row ids changed");
    }
    const namesChanged =
      previousSummary.names.length !== nextSummary.names.length ||
      previousSummary.names.some((name, index) => name !== nextSummary.names[index]);
    if (namesChanged && !idsChanged) {
      reasons.push("row names changed");
    }
    return reasons;
  }

  logDebug(message) {
    if (!message) return;
    try {
      window.javaBridge?.logDebug?.(String(message));
    } catch (e) {
      globalThis.uiDebug?.log?.("logDebug blocked", { message: String(message), error: String(e) });
    }
  }

  isLocalUserIdentified() {
    const name = String(this.USER_NAME ?? "").trim();
    const localId = Number(this.localPlayerId);
    return Boolean(name) && Number.isFinite(localId) && localId > 0;
  }

  getDefaultTargetLabel(targetMode = "") {
    if (targetMode === "bossTargets") {
      return this.i18n?.t("target.boss", "Boss Targets") ?? "Boss Targets";
    }
    if (targetMode === "allTargets") {
      return this.i18n?.t("target.all", "All Targets") ?? "All Targets";
    }
    if (targetMode === "trainTargets") {
      if (!this.isLocalUserIdentified()) {
        return this.i18n?.t("target.identifying", "Identifying you...") ?? "Identifying you...";
      }
      return this.i18n?.t("target.train", "Training Scarecrow") ?? "Training Scarecrow";
    }
    return this.i18n?.t("header.title", "A2Tools DPS Meter") ?? "A2Tools DPS Meter";
  }

  getTargetLabel({ targetId = 0, targetName = "", targetMode = "", dungeonId = 0 } = {}) {
    // In a party instance the title is the boss being fought, and the dungeon
    // between pulls. It used to stay on the dungeon throughout, so a whole run
    // read "Urugugu Canyon" past every boss. The modes that track no single
    // target (all targets, training) keep the dungeon.
    const dungeonLabel = Number(dungeonId) > 0
      ? (this.i18n?.getDungeonLabel?.(Number(dungeonId)) ?? "")
      : "";
    if (dungeonLabel) {
      const tracksOne = targetMode !== "allTargets" && targetMode !== "trainTargets";
      const numericId = Number(targetId);
      if (tracksOne && Number.isFinite(numericId) && numericId > 0) {
        const cleanName = typeof targetName === "string" ? targetName.trim() : "";
        const bossName = this.i18n?.getNpcName?.(numericId, cleanName) ?? cleanName;
        if (bossName) return bossName;
      }
      return dungeonLabel;
    }
    if (targetMode === "trainTargets" && !this.isLocalUserIdentified()) {
      return this.i18n?.t("target.identifying", "Identifying you...") ?? "Identifying you...";
    }
    if (targetMode === "allTargets" || targetMode === "trainTargets") {
      return this.getDefaultTargetLabel(targetMode);
    }
    if (targetMode === "bossTargets" && (!Number(targetId) || Number(targetId) <= 0) && !targetName) {
      return this.getDefaultTargetLabel(targetMode);
    }
    if (targetMode === "lastHitByMe" && (!Number(targetId) || Number(targetId) <= 0) && !targetName) {
      // Only prompt to identify when we genuinely don't know who the player is.
      // Once identified, an empty target (idle / just after a reset or zone change)
      // should fall back to the neutral mode label, not keep showing "Identifying...".
      if (!this.isLocalUserIdentified()) {
        return this.i18n?.t("target.identifying", "Identifying you...") ?? "Identifying you...";
      }
      return this.getDefaultTargetLabel(targetMode);
    }
    const numericTargetId = Number(targetId);
    const cleanTargetName = typeof targetName === "string" ? targetName.trim() : "";
    if (Number.isFinite(numericTargetId) && numericTargetId > 0) {
      const localizedName = this.i18n?.getNpcName?.(numericTargetId, cleanTargetName) ?? cleanTargetName;
      return localizedName || `Mob #${numericTargetId}`;
    }
    if (cleanTargetName) {
      return cleanTargetName;
    }
    return this.getDefaultTargetLabel(targetMode);
  }

  updateTargetModeButton() {
    if (!this.targetModeBtn) return;
    const isBossTargets = this.targetSelection === "bossTargets";
    const isAllTargets = this.targetSelection === "allTargets";
    const isTrainTargets = this.targetSelection === "trainTargets";
    this.targetModeBtn.classList.toggle("isAllTargets", isAllTargets);
    this.targetModeBtn.classList.toggle("isTrainTargets", isTrainTargets);
    this.targetModeBtn.textContent = isBossTargets ? "BOSS" : isAllTargets ? "ALL" : isTrainTargets ? "TRAIN" : "TARGET";
    const ariaLabel = isBossTargets
      ? "Boss targets mode"
      : isAllTargets
        ? "All targets mode"
        : isTrainTargets
          ? "Train targets mode"
          : "Target mode";
    this.targetModeBtn.setAttribute("aria-label", ariaLabel);
  }

  refreshBossLabel() {
    if (!this.elBossName) return;
    if (this.lastTargetName || this.lastTargetId) {
      return;
    }
    this.elBossName.textContent = this.getTargetLabel({
      targetMode: this.lastTargetMode,
      targetId: this.lastTargetId,
      targetName: this.lastTargetName,
      dungeonId: this.lastDungeonId,
    });
    this.elBossName.classList.toggle("isAllTargets", this.lastTargetMode === "allTargets");
  }

  bindDragToMoveWindow() {
    let isDragging = false;
    let startX = 0;
    let startY = 0;
    let initialStageX = 0;
    let initialStageY = 0;
    let pendingStageX = 0;
    let pendingStageY = 0;
    let lastMovedX = Number.NaN;
    let lastMovedY = Number.NaN;
    let dragRafId = null;
    let hasDragMoved = false;

    const flushMove = (force = false) => {
      dragRafId = null;
      if ((!isDragging && !force) || !window.javaBridge) return;
      const deltaSinceLastX = Math.abs(pendingStageX - lastMovedX);
      const deltaSinceLastY = Math.abs(pendingStageY - lastMovedY);
      if (!force && Number.isFinite(deltaSinceLastX) && Number.isFinite(deltaSinceLastY)) {
        if (deltaSinceLastX < 2 && deltaSinceLastY < 2) return;
      }
      if (pendingStageX === lastMovedX && pendingStageY === lastMovedY) return;
      window.javaBridge.moveWindow(pendingStageX, pendingStageY);
      lastMovedX = pendingStageX;
      lastMovedY = pendingStageY;
    };

    document.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      const targetEl = e.target?.nodeType === Node.TEXT_NODE ? e.target.parentElement : e.target;
      if (targetEl?.closest?.(".resizeHandle")) {
        return;
      }
      if (targetEl?.closest?.(".headerBtn, .footerBtn, .bossIcon")) {
        return;
      }
      if (targetEl?.closest?.(".settingsPanel, .historyPanel, .detailsBody, .detailsSettingsMenu")) {
        return;
      }
      if (targetEl?.closest?.("button, input, select, textarea, a, [data-no-drag]")) {
        return;
      }
      isDragging = true;
      hasDragMoved = false;
      this.isWindowDragging = true;
      this.deferFetchUntilDragEnd = true;
      // Don't freeze pointer events yet — defer until the 3px drag threshold
      // is crossed so that simple clicks on meter bars still fire normally.
      startX = e.screenX;
      startY = e.screenY;
      initialStageX = window.screenX;
      initialStageY = window.screenY;
      pendingStageX = initialStageX;
      pendingStageY = initialStageY;
      lastMovedX = Number.NaN;
      lastMovedY = Number.NaN;
    });

    document.addEventListener("mousemove", (e) => {
      if (!isDragging || !window.javaBridge) return;

      const deltaX = e.screenX - startX;
      const deltaY = e.screenY - startY;
      if (!hasDragMoved && (Math.abs(deltaX) > 3 || Math.abs(deltaY) > 3)) {
        hasDragMoved = true;
        this.setWindowDragFreeze(true);
        this.elList?.classList?.add?.("dragInteracting");
        // Hide heavy panels during drag to reduce per-frame repaint cost;
        // place a lightweight ghost outline so the user sees where they are.
        for (const sel of [".settingsPanel", ".detailsPanel", ".historyPanel"]) {
          const el = document.querySelector(sel);
          if (el && el.offsetParent !== null) {
            const r = el.getBoundingClientRect();
            const ghost = document.createElement("div");
            ghost.style.cssText = `position:fixed;left:${r.left}px;top:${r.top}px;width:${r.width}px;height:${r.height}px;`
              + "background:rgba(0,0,0,0.35);border:1px solid rgba(255,255,255,0.12);border-radius:8px;pointer-events:none;z-index:9999;";
            document.body.appendChild(ghost);
            el.style.visibility = "hidden";
            el._dragHidden = true;
            el._dragGhost = ghost;
          }
        }
      }
      pendingStageX = initialStageX + deltaX;
      pendingStageY = initialStageY + deltaY;

      if (dragRafId !== null) return;
      dragRafId = requestAnimationFrame(flushMove);
    });

    document.addEventListener("mouseup", () => {
      if (!isDragging) return;
      isDragging = false;
      this.isWindowDragging = false;
      this.setWindowDragFreeze(false);
      if (hasDragMoved) {
        this.elList?.classList?.remove?.("dragInteracting");
        this.suppressRowInteractionUntilMs = this.nowMs() + 120;
        // Restore panels hidden during drag
        for (const sel of [".settingsPanel", ".detailsPanel", ".historyPanel"]) {
          const el = document.querySelector(sel);
          if (el?._dragHidden) {
              el.style.visibility = "";
              el._dragGhost?.remove();
              delete el._dragHidden;
              delete el._dragGhost;
            }
        }
      }
      if (dragRafId !== null) {
        cancelAnimationFrame(dragRafId);
        dragRafId = null;
      }
      flushMove(true);
      if (this.deferFetchUntilDragEnd) {
        this.deferFetchUntilDragEnd = false;
        this.fetchDps();
      }
    });
  }

  bindResizeHandle() {
    this.resizeHandle = document.querySelector(".resizeHandle");
    this.meterEl = document.querySelector(".meter");
    if (!this.resizeHandle || !this.meterEl) return;

    let isResizing = false;
    let startX = 0;
    let startY = 0;
    let startWidth = 0;
    let startHeight = 0;
    const minWidth = 300;
    const minHeight = 30;

    // Screen coordinates, not client ones: a window manager that keeps windows
    // on screen (KWin) moves the window while it grows during the drag, which
    // shifts clientX/Y and made the size jump past the cursor (issue #11).
    const onMouseMove = (event) => {
      if (!isResizing) return;
      // The button came up outside the window, where no mouseup arrives; the
      // next move inside shows it. A move outside is not trusted: a fast drag
      // leaves the window before it has grown (KWin, XWayland) and those moves
      // can report no button held, which cut the resize short.
      const inside = event.clientX >= 0 && event.clientY >= 0
        && event.clientX < window.innerWidth && event.clientY < window.innerHeight;
      if ((event.buttons & 1) === 0 && inside) {
        onMouseUp();
        return;
      }
      const nextWidth = Math.max(minWidth, startWidth + (event.screenX - startX));
      const nextHeight = Math.max(minHeight, startHeight + (event.screenY - startY));
      this.meterEl.style.width = `${nextWidth}px`;
      this.meterEl.style.height = `${nextHeight}px`;
    };

    const onMouseUp = () => {
      if (!isResizing) return;
      isResizing = false;
      // Convert fixed height to min-height so the meter can still grow
      // when new rows are added, while preserving the user's minimum.
      const currentHeight = this.meterEl.style.height;
      if (currentHeight) {
        this.meterEl.style.minHeight = currentHeight;
        this.meterEl.style.height = "";
      }
    };

    this.resizeHandle.addEventListener("mousedown", (event) => {
      event.preventDefault();
      event.stopPropagation();
      const rect = this.meterEl.getBoundingClientRect();
      startWidth = rect.width;
      startHeight = rect.height;
      startX = event.screenX;
      startY = event.screenY;
      isResizing = true;
    });

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  }

  // ===== Click-through lock =====
  // Locked, the overlay lets clicks through to the game and cannot be
  // dragged; only its lock button stays clickable (the backend watches the
  // pointer, see OverlayLock in app.rs), and a hotkey toggles it too. Offered
  // only where the backend can do that: Windows, not Wayland.

  initOverlayLock() {
    this._overlayLocked = false;
    this.showLockBtnCheckbox = document.querySelector(".showLockBtnCheckbox");
    const isOverlay = window.A2_VIEW === "main";
    Promise.resolve(window.javaBridge?.overlayLockSupported?.())
      .then((supported) => {
        if (!supported) return;
        document.querySelectorAll(".lockSetting").forEach((el) => { el.style.display = ""; });
        const show = this.safeGetSetting(this.storageKeys.showLockBtn) === "true";
        if (this.showLockBtnCheckbox) {
          this.showLockBtnCheckbox.checked = show;
          this.showLockBtnCheckbox.addEventListener("change", (event) => {
            const isChecked = !!event.target?.checked;
            this.safeSetSetting(this.storageKeys.showLockBtn, String(isChecked));
            if (!isOverlay) return;
            this._applyLockBtnVisibility(isChecked);
            // Hiding the button must not leave a locked overlay behind.
            if (!isChecked) this._setOverlayLocked(false);
          });
        }
        if (!isOverlay) return;
        this._applyLockBtnVisibility(show);
        window.addEventListener("resize", () => this._sendLockBtnRect());
        // Stay locked across restarts, as the player left it.
        if (show && this.safeGetSetting(this.storageKeys.overlayLocked) === "true") {
          requestAnimationFrame(() => this._setOverlayLocked(true));
        }
      })
      .catch(() => {});
  }

  _applyLockBtnVisibility(show) {
    if (this.lockBtn) this.lockBtn.style.display = show ? "" : "none";
    this.headerBtns?.classList.toggle("hasLockBtn", !!show);
    if (show) requestAnimationFrame(() => this._sendLockBtnRect());
  }

  // Where the button is, so the backend keeps it clickable while locked.
  _sendLockBtnRect() {
    if (!this.lockBtn || this.lockBtn.style.display === "none") return;
    const r = this.lockBtn.getBoundingClientRect();
    if (!r.width || !r.height) return;
    window.javaBridge?.setLockButtonRect?.(r.left, r.top, r.width, r.height, window.devicePixelRatio || 1);
  }

  _setOverlayLocked(locked) {
    this._sendLockBtnRect();
    window.javaBridge?.setOverlayLocked?.(!!locked);
    this._onOverlayLockChanged(!!locked);
  }

  // Also called when the hotkey toggled the lock (the backend has done it).
  _onOverlayLockChanged(locked) {
    this._overlayLocked = !!locked;
    this.safeSetSetting(this.storageKeys.overlayLocked, String(this._overlayLocked));
    document.body.classList.toggle("overlayLocked", this._overlayLocked);
    if (!this.lockBtn) return;
    this.lockBtn.classList.toggle("isLocked", this._overlayLocked);
    const icon = document.createElement("i");
    icon.setAttribute("data-lucide", this._overlayLocked ? "lock" : "lock-open");
    this.lockBtn.replaceChildren(icon);
    window.lucide?.createIcons?.({ root: this.lockBtn });
  }

  _applySuspendBtnVisibility(show) {
    if (this.suspendBtn) {
      this.suspendBtn.style.display = show ? "" : "none";
    }
    if (this.headerBtns) {
      this.headerBtns.classList.toggle("hasSuspendBtn", !!show);
    }
  }

  _setCaptureSuspended(suspended) {
    this._captureSuspended = !!suspended;
    window.javaBridge?.suspendCapture?.(this._captureSuspended);
    this._updateSuspendBtnIcon();
    this._updateSuspendStatusMessage();
  }

  _updateSuspendBtnIcon() {
    if (!this.suspendBtn) return;
    const iconEl = this.suspendBtn.querySelector("i, svg");
    if (!iconEl) return;
    const iconName = this._captureSuspended ? "power-off" : "power";
    this.suspendBtn.classList.toggle("isSuspended", !!this._captureSuspended);
    if (iconEl.tagName === "I") {
      iconEl.setAttribute("data-lucide", iconName);
    } else {
      // SVG already rendered — replace with a new <i> tag and re-render
      const newIcon = document.createElement("i");
      newIcon.setAttribute("data-lucide", iconName);
      this.suspendBtn.replaceChildren(newIcon);
    }
    window.lucide?.createIcons?.({ root: this.suspendBtn });
  }

  // A one-time popup offering Discord activity, with the toggle already on.
  // Done keeps what the toggle says; closing it any other way (×, Escape, a
  // click outside) leaves it off. Not shown to anyone who has already chosen
  // either way in Settings, and never again once answered.
  maybeShowDiscordPromo() {
    const promo = document.querySelector("#discordPromo");
    if (!promo) return;
    if (this.safeGetSetting(this.storageKeys.discordPromoShown) === "true") return;
    const current = this.safeGetSetting(this.storageKeys.discordActivity);
    if (current === "true" || current === "false") return;

    const checkbox = promo.querySelector(".discordPromoCheckbox");
    let answered = false;
    const onKey = (event) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        answer(false);
      }
    };
    const answer = (enabled) => {
      if (answered) return;
      answered = true;
      this.safeSetSetting(this.storageKeys.discordActivity, String(enabled));
      this.safeSetSetting(this.storageKeys.discordPromoShown, "true");
      const settingsCheckbox = document.querySelector(".discordActivityCheckbox");
      if (settingsCheckbox) settingsCheckbox.checked = enabled;
      promo.classList.remove("isOpen");
      promo.setAttribute("aria-hidden", "true");
      document.removeEventListener("keydown", onKey, true);
    };
    promo.querySelector(".discordPromoDone")?.addEventListener("click", () => answer(!!checkbox?.checked));
    promo.querySelector(".discordPromoClose")?.addEventListener("click", () => answer(false));
    promo.addEventListener("click", (event) => {
      if (event.target === promo) answer(false);
    });

    // A moment after start, so it does not land on top of the first paint.
    setTimeout(() => {
      if (answered) return;
      if (checkbox) checkbox.checked = true;
      promo.classList.add("isOpen");
      promo.setAttribute("aria-hidden", "false");
      document.addEventListener("keydown", onKey, true);
    }, 2500);
  }

  _updateSuspendStatusMessage() {
    const el = this.analysisStatusEl || document.querySelector(".battleTime .analysisStatus");
    if (!el) return;
    if (this._captureSuspended) {
      el.textContent = this.i18n?.t?.("battleTime.suspended", "App suspended") ?? "App suspended";
      el.style.display = "";
      // Ensure the battle time bar is visible so the message shows
      if (this.battleTimeRoot) this.battleTimeRoot.classList.add("isVisible");
    } else {
      el.textContent = this.i18n?.t?.("battleTime.analysing", "Monitoring data...") ?? "Monitoring data...";
    }
  }

}

DpsApp.instance = null;

// 디버그콘솔
const setupDebugConsole = () => {
  if (globalThis.uiDebug?.log) return globalThis.uiDebug;

  const consoleDiv = document.querySelector(".console");
  if (!consoleDiv) {
    globalThis.uiDebug = { log: () => {}, clear: () => {} };
    return globalThis.uiDebug;
  }

  const safeStringify = (value) => {
    if (typeof value === "string") return value;
    if (value instanceof Error) return `${value.name}: ${value.message}`;
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  };

  const appendLine = (line) => {
    consoleDiv.style.display = "block";
    consoleDiv.innerHTML += line + "<br>";
    consoleDiv.scrollTop = consoleDiv.scrollHeight;
  };

  const originalConsole = {
    log: console.log.bind(console),
    info: console.info.bind(console),
    warn: console.warn.bind(console),
    error: console.error.bind(console),
  };

  const printToPanel = (level, args) => {
    const line = args.map(safeStringify).join(" ");
    appendLine(`[${level}] ${line}`);
  };

  if (!globalThis.__uiConsolePatched) {
    ["log", "info", "warn", "error"].forEach((level) => {
      console[level] = (...args) => {
        originalConsole[level](...args);
        printToPanel(level, args);
      };
    });
    globalThis.__uiConsolePatched = true;
  }

  globalThis.uiDebug = {
    clear() {
      consoleDiv.innerHTML = "";
    },
    log(...args) {
      printToPanel("debug", args);
    },
  };

  return globalThis.uiDebug;
};

// Keep JavaFX overlay console hidden unless explicitly re-enabled for troubleshooting.
// setupDebugConsole();
const dpsApp = DpsApp.createInstance();
window.dpsApp = dpsApp;
const debug = globalThis.uiDebug || { log: () => {}, clear: () => {} };

window.addEventListener("error", (event) => {
  debug?.log?.("window.error", {
    message: event.message,
    source: event.filename,
    line: event.lineno,
    column: event.colno,
  });
});

window.addEventListener("unhandledrejection", (event) => {
  debug?.log?.("unhandledrejection", event.reason);
});


let appStarted = false;
const startApp = async ({ forced = false } = {}) => {
  if (appStarted) return;
  appStarted = true;
  debug?.log?.("startApp", {
    readyState: document.readyState,
    hasDpsData: !!window.dpsData,
    hasJavaBridge: !!window.javaBridge,
    forced,
  });
  // Window controls answer at once: the translations below load first, and
  // until they had, Quit and closing the settings window did nothing.
  document.querySelector(".quitButton")?.addEventListener("click", () => {
    window.javaBridge?.exitApp?.();
  });
  if (window.A2_VIEW === "settings") {
    const close = () => window.javaBridge?.closeSettingsWindow?.();
    document.querySelector(".settingsClose")?.addEventListener("click", close);
    document.querySelector(".settingsWindowClose")?.addEventListener("click", close);
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape") close();
    });
  }
  try {
    await window.i18n?.init?.();
    window.lucide?.createIcons?.();
    dpsApp.start();
    if (window.A2_VIEW === "details") {
      dpsApp.enterDetailsWindowMode();
    } else if (window.A2_VIEW === "settings") {
      dpsApp.enterSettingsWindowMode();
    } else if (window.A2_VIEW === "history") {
      dpsApp.enterHistoryWindowMode();
    }
    window.javaBridge?.notifyUiReady?.();

  } catch (err) {
    debug?.log?.("startApp.error", err);
  }
};

const waitForBridgeAndStart = (attempt = 0) => {
  // JavaFX WebView injects these after loadWorker SUCCEEDED (slightly later than DOMContentLoaded)
  const ready = !!window.javaBridge && !!window.dpsData;

  debug?.log?.("waitForBridge", {
    attempt,
    readyState: document.readyState,
    hasDpsData: !!window.dpsData,
    hasJavaBridge: !!window.javaBridge,
  });

  if (ready) {
    startApp();
    return;
  }

  if (attempt >= 200) {
    debug?.log?.("waitForBridge.timeout", "Bridge not ready after 10s; forcing UI startup.");
    startApp({ forced: true });
    return;
  }

  setTimeout(() => waitForBridgeAndStart(attempt + 1), 50);
};

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", waitForBridgeAndStart, { once: true });
} else {
  waitForBridgeAndStart();
}
