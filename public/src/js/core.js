const SETTING_CHOICES = {
  defaultMeterMode: { allowed: ["bossTargets", "lastHitByMe", "allTargets", "trainTargets"], fallback: "bossTargets" },
  allTargetsWindowMs: { allowed: ["30000", "60000", "120000", "180000", "300000"], fallback: "120000" },
  // The edge the meter keeps as rows come and go. Unset, the one it kept
  // before there was a choice: its top where it places its own windows, its
  // centre on KDE Wayland, where KWin keeps it (app.rs build_features).
  meterAnchor: { allowed: ["top", "middle", "bottom"], fallback: "top" },
  // How often the backend sends DPS (app.rs DPS_RATES_MS).
  updateRateMs: { allowed: ["100", "200", "250", "500"], fallback: "200" },
  trainSelectionMode: { allowed: ["all", "highestDamage"], fallback: "all" },
  // The figure beside each player's name, in the meter and in Details.
  partyStat: { allowed: ["combatPower", "gearScore", "both", "off"], fallback: "combatPower" },
  // When the last fight leaves the screen after the zone changes.
  clearOnZone: { allowed: ["instance", "any", "never"], fallback: "instance" },
  // Seconds without combat before the meter hides; "0" is off (app.rs, #106).
  hideOutOfCombat: { allowed: ["0", "10", "20", "30", "60", "120"], fallback: "0" },
  // The number after the level in the Discord activity (presence.rs).
  discordStat: { allowed: ["combatPower", "gearScore", "off"], fallback: "off" },
  // A pattern over the meter's bars: a supporter's perk (applyBarTexture).
  // The Settings window's own colours (styles.css, data-settings-theme):
  // "meter" keeps the meter theme's accent.
  settingsTheme: { allowed: ["meter", "midnight", "graphite", "violet", "forest", "crimson", "ocean", "sapphire", "indigo", "rose", "sunset", "amber", "copper", "lime", "ice", "slate", "onyx"], fallback: "meter" },
  // The meter's font (styles.css, data-font): "meter" is its own (#98).
  font: { allowed: ["meter", "system", "inter", "exo2", "oxanium", "jetbrains", "atkinson", "lexend", "andika"], fallback: "meter" },
  barTexture: { allowed: ["none", "flames", "crystal", "glacier", "feathers", "wing", "storm", "nebula", "galaxy", "aura"], fallback: "none" },
};
// Themes whose bars differ from one another show those colours in the theme
// menu: the class colours side by side, and the place-by-place sweeps
// (styles.css: Spectrum, Aurora, Sunset).
const THEME_PREVIEWS = {
  // The vivid one: the class colours at full strength.
  "aion2-classes-vivid": "linear-gradient(90deg, #4FD1C5 0.00% 11.11%, #5F8CFF 11.11% 22.22%, #41D98A 22.22% 33.33%, #7BE35A 33.33% 44.44%, #9A6BFF 44.44% 55.56%, #E06BFF 55.56% 66.67%, #F2C15A 66.67% 77.78%, #FF9A3D 77.78% 88.89%, #E85D5D 88.89% 100.00%)",
  // The class colours as the meter draws them: about halfway between the
  // colour and the panel (its bars run 30-62%).
  "aion2-classes": "linear-gradient(90deg, #2b6b6b 0.00% 11.11%, #324a87 11.11% 22.22%, #246f4f 22.22% 33.33%, #407438 33.33% 44.44%, #4f3a87 44.44% 55.56%, #703a87 55.56% 66.67%, #796338 66.67% 77.78%, #7f512a 77.78% 88.89%, #743339 88.89% 100.00%)",
  "aion2-spectrum": "linear-gradient(90deg, #e2504e, #e2a43c, #c9cf3a, #4fcf6a, #3cbfe0, #5a6ee8, #b05ae8)",
  "aion2-aurora": "linear-gradient(90deg, #2fc9a8, #2f9fd9, #4f6fe0, #8a55e0, #c650c9, #e0508a)",
  "aion2-sunset": "linear-gradient(90deg, #e8b83c, #e8823c, #e04a3a, #d83c78, #b03cc0, #7a48d8)",
};
const pickSettingChoice = (name, value) => {
  const { allowed, fallback } = SETTING_CHOICES[name];
  return allowed.includes(String(value)) ? String(value) : fallback;
};

// The footer menu's displays: the DpsApp field and setting key, its label.
const FOOTER_DISPLAYS = [
  ["showTtk", "footerMenu.ttk", "Time to kill estimate"],
  ["showTotalDps", "footerMenu.partyTotal", "Party total"],
  ["showOdyle", "footerMenu.odyle", "Odyle energy"],
  ["showPing", "footerMenu.ping", "Ping"],
];

// The target mode menu, in the button's old cycle order: mode, the label the
// button shows, and what it counts.
const TARGET_MENU_MODES = [
  ["lastHitByMe", "TARGET", "header.targetMode.lastHitByMe", "The enemy you hit last"],
  ["bossTargets", "BOSS", "header.targetMode.bossTargets", "Bosses only"],
  ["trainTargets", "TRAIN", "header.targetMode.trainTargets", "Training dummies"],
  ["allTargets", "ALL", "header.targetMode.allTargets", "Every enemy, combined"],
];

// Settings whose change in the Settings window must redraw the meter, mapped to
// the control that applies them. See DpsApp.applyRemoteSettingChange().
const REMOTE_APPLIED_SETTING_CONTROLS = {
  "dpsMeter.roundDps": ".roundDpsCheckbox",
  "dpsMeter.showDpsSuffix": ".showDpsSuffixCheckbox",
  "dpsMeter.pinMeToTop": ".pinMeToTopCheckbox",
  "dpsMeter.mainPlayerNamesBold": ".playerNamesBoldCheckbox",
  "dpsMeter.mainPlayerDpsBold": ".playerDpsBoldCheckbox",
  "dpsMeter.bossNameSize": ".bossNameSizeInput",
  "dpsMeter.showSupporterColors": ".showSupporterColorsCheckbox",
  "dpsMeter.barTextureOffset": ".textureOffsetCheckbox",
  "dpsMeter.showSuspendBtn": ".showSuspendBtnCheckbox",
  "dpsMeter.showLockBtn": ".showLockBtnCheckbox",
  // Set from the main window by the account offer.
  "dpsMeter.autoUpload": ".autoUploadCheckbox",
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
      updateRateMs: "dpsMeter.updateRateMs",
      trainSelectionMode: "dpsMeter.trainSelectionMode",
      partyStat: "dpsMeter.partyStat",
      clearOnZone: "dpsMeter.clearOnZone",
      barTexture: "dpsMeter.barTexture",
      barTextureOffset: "dpsMeter.barTextureOffset",
      meterAnchor: "dpsMeter.meterAnchor",
      meterFillOpacity: "dpsMeter.meterFillOpacity",
      meterFillSaturation: "dpsMeter.meterFillSaturation",
      barGap: "dpsMeter.barGap",
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
      shareData: "dpsMeter.shareData",
      pinMeToTop: "dpsMeter.pinMeToTop",
      mainPlayerNamesBold: "dpsMeter.mainPlayerNamesBold",
      showSupporterColors: "dpsMeter.showSupporterColors",
      mainPlayerDpsBold: "dpsMeter.mainPlayerDpsBold",
      showPing: "dpsMeter.showPing",
      showOdyle: "dpsMeter.showOdyle",
      odyleLast: "dpsMeter.odyleLast",
      showTtk: "dpsMeter.showTtk",
      extraMetrics: "dpsMeter.extraMetrics",
      showBossStagger: "dpsMeter.showBossStagger",
      statColours: "dpsMeter.statColours",
      showDeaths: "dpsMeter.showDeaths",
      showTotalDps: "dpsMeter.showTotalDps",
      roundDps: "dpsMeter.roundDps",
      autoDetectDevice: "dpsMeter.autoDetectDevice",
      manualDevice: "dpsMeter.manualDevice",
      showDpsSuffix: "dpsMeter.showDpsSuffix",
      playerLimit: "dpsMeter.playerLimit",
      theme: "dpsMeter.theme",
      slimMode: "dpsMeter.slimMode",
      settingsTheme: "dpsMeter.settingsTheme",
      font: "dpsMeter.font",
      autoHideMeter: "dpsMeter.autoHideMeter",
      hideWhenGameClosed: "dpsMeter.hideWhenGameClosed",
      hideWhenSolo: "dpsMeter.hideWhenSolo",
      hideOutOfCombat: "dpsMeter.hideOutOfCombat",
      globalHotkeys: "dpsMeter.globalHotkeys",
      bossLogs: "dpsMeter.bossLogsEnabled",
      saveRawPackets: "dpsMeter.saveRawPackets",
      autoUpload: "dpsMeter.autoUpload",
      guestUpload: "dpsMeter.guestUpload",
      sharingAsked: "dpsMeter.sharingAsked",
      lastSeenVersion: "dpsMeter.lastSeenVersion",
      discordActivity: "dpsMeter.discordActivity",
      discordStat: "dpsMeter.discordStat",
      discordPromoShown: "dpsMeter.discordPromoShown",
      partyPresence: "dpsMeter.partyPresence",
      partyPromoShown: "dpsMeter.partyPromoShown",
      accountPromoShown: "dpsMeter.accountPromoShown",
      guestPromoShown: "dpsMeter.guestPromoShown",
      windowOpacity: "dpsMeter.windowOpacity",
      meterWidth: "dpsMeter.meterWidth",
      meterMinHeight: "dpsMeter.meterMinHeight",
      titleOpacity: "dpsMeter.titleOpacity",
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
    // Extra columns from the metric menu, loaded with the display mode.
    this.extraMetrics = [];
    this.showBossStagger = false;
    this.betaUi = true;
    this.theme = "aion2";
    this.availableThemes = [
      "aion2",
      "aion2-classes",
      "aion2-classes-vivid",
      "aion2-spectrum",
      "aion2-aurora",
      "aion2-sunset",
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
    this._barsTargetLabel = null;
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
    this.partyStat = "combatPower";
    this.barTexture = "none";
    // Whether the signed-in A2 Tools account is a supporter's (bar textures).
    this.isSupporter = false;
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
    this.hoverTooltipInFlight = false;
    this.hoverTooltipQueued = null;
    this.hoverTooltipPositionRaf = 0;
    this.hoverTooltipGeometryKey = "";
    this.hoverTooltipDismissed = true;
    this.hoverPointerWatch = 0;

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

  /** Auto-detect of the capture device: on unless unticked in Settings. A
   *  device saved by an older version, before the switch itself was saved,
   *  also means it was off. */
  readAutoDetectDevice() {
    const saved = this.safeGetSetting("dpsMeter.autoDetectDevice");
    if (saved === "false") return false;
    if (saved === "true") return true;
    return !this.safeGetSetting("dpsMeter.manualDevice");
  }

  // Whether keybinds are on: the saved choice, else this OS's default.
  globalHotkeysOn() {
    const stored = this.safeGetSetting(this.storageKeys.globalHotkeys);
    if (stored === "true" || stored === "false") return stored === "true";
    return !document.documentElement?.classList?.contains?.("linux");
  }

  safeSetSetting(key, value) {
    // A change replayed from another window is that window's to save: writing
    // it back sends it round again (applyRemoteSettingChange).
    if (this._applyingRemoteSetting) return;
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
    if (window.A2_VIEW === "settings") {
      // Opening settings must not construct or lay out the hidden combat views.
      this.showPing = this.safeGetSetting(this.storageKeys.showPing) !== "false";
      this.showOdyle = this.safeGetSetting(this.storageKeys.showOdyle) === "true";
      this.showTtk = this.safeGetSetting(this.storageKeys.showTtk) !== "false";
      this.showTotalDps = this.safeGetSetting(this.storageKeys.showTotalDps) !== "false";
      this.roundDps = this.safeGetSetting(this.storageKeys.roundDps) !== "false";
      this.showDpsSuffix = this.safeGetSetting(this.storageKeys.showDpsSuffix) !== "false";
      const savedLimit = parseInt(this.safeGetSetting(this.storageKeys.playerLimit), 10);
      this.playerLimit = Number.isFinite(savedLimit) && savedLimit >= 1 ? savedLimit : 6;
      this.setupSettingsPanel();
      this.refreshSettingsStatus();
      this.i18n?.onChange?.((lang) => {
        this.settingsSelections.language = lang;
        this.initializeSettingsDropdowns();
        this.refreshConnectionInfo();
        this.updateSupportPrimaryAction(lang);
        this.updateSupportQrImage(this.supportPrimaryButton?.dataset.support || "afdian");
      });
      return;
    }
    this.elList = document.querySelector(".list:not(.barPreviewList)");
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
    this.updatesBtn = document.querySelector(".updatesBtn");
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
      getPartyStat: () => this.partyStat,
      getJobColor: (job) => this.getJobColor(job),
      onHoverUserRow: (row, event) => {
        if (row?.isOthers || this.shouldSuppressRowInteractions()) return;
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
        if (!row || row.isOthers || this.isWindowDragging) return;
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
        return this.i18n?.t?.("battleTime.suspended", "Suspended") ?? "Suspended";
      }
      if (!this.aionRunning) {
        const text = this.i18n?.t("battleTime.notRunning", "AION2 closed") ?? "AION2 closed";
        return withBacklog(text);
      }
      if (this.isDetectingPort) {
        const text = this.i18n?.t("connection.detecting", "Connecting...") ??
          "Connecting...";
        return withBacklog(text);
      }
      if (this.battleTime?.getState?.() === "state-idle") {
        const text = this.i18n?.t("battleTime.idle", "Idle") ?? "Idle";
        return withBacklog(text);
      }
      const text = this.i18n?.t("battleTime.analysing", "Monitoring") ?? "Monitoring";
      return withBacklog(text);
    };

    this.battleTime = createBattleTimeUI({
      rootEl: document.querySelector(".battleTime"),
      tickSelector: ".tick",
      ttkSelector: ".ttk",
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

    this.applyBarGap(this.safeGetSetting(this.storageKeys.barGap), { persist: false });

    this.pingEl = document.querySelector(".pingDisplay");
    this.showPing = this.safeGetSetting(this.storageKeys.showPing) !== "false";
    // Odyle energy, pushed by the backend ("odyle-energy") once the game has
    // sent it: at login, after a server transfer, and when it changes. Off
    // unless turned on in Settings. Until then, the last value seen for this
    // character, dimmed.
    this.odyleEl = document.querySelector(".odyleDisplay");
    this.showOdyle = this.safeGetSetting(this.storageKeys.showOdyle) === "true";
    this.odyleEnergy = null;
    this.updateOdyle();
    // Time to kill beside the battle timer, for bosses; on unless turned off.
    this.showTtk = this.safeGetSetting(this.storageKeys.showTtk) !== "false";
    this.ttk = typeof createTtkEstimator === "function" ? createTtkEstimator() : null;
    // Ping is pushed immediately from PingTracker via window._dpsApp.updatePing().
    // A slow fallback poll handles edge cases (e.g. push not wired yet on startup).
    if (window.A2_VIEW !== "settings") this._pingTimer = setInterval(() => this.updatePing(), 30000);

    this.showTotalDps = this.safeGetSetting(this.storageKeys.showTotalDps) !== "false";
    // Defaults on: `!== "false"` treats "never set" as enabled.
    this.roundDps = this.safeGetSetting(this.storageKeys.roundDps) !== "false";
    // "/s" after the meter's DPS figures; on unless turned off (Advanced).
    this.showDpsSuffix = this.safeGetSetting(this.storageKeys.showDpsSuffix) !== "false";
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
      getPartyStat: () => this.partyStat,
      getJobColor: (job) => this.getJobColor(job),
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
        // Kept inside the window: in its own window the panel's side and
        // bottom padding overhang the edges by 10px, and the capture took
        // whatever was behind the window there (issue #6).
        const left = Math.max(0, Math.min(...rects.map((r) => r.left)));
        const top = Math.max(0, Math.min(...rects.map((r) => r.top)));
        const right = Math.min(window.innerWidth, Math.max(...rects.map((r) => r.right)));
        const bottom = Math.min(window.innerHeight, Math.max(...rects.map((r) => r.bottom)));
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
    // Not in the private build, which checks for nothing (build_features).
    if (window.A2_VIEW === "main") {
      Promise.resolve(window.a2BuildReady).then((build) => {
        if (build?.online !== false) this.setupUpdatesButton();
      });
    }
    this.setupConsoleDebugging();
    this.bindNativeHotkeyBridge();

    try {
      const extras = JSON.parse(this.safeGetStorage(this.storageKeys.extraMetrics) || "[]");
      this.extraMetrics = Array.isArray(extras) ? extras : [];
    } catch {
      this.extraMetrics = [];
    }
    this.showBossStagger = this.safeGetStorage(this.storageKeys.showBossStagger) === "true";
    // Each metric in its own colour (styles.css); on unless turned off.
    this.statColours = this.safeGetStorage(this.storageKeys.statColours) !== "false";
    document.body.classList.toggle("noStatColours", !this.statColours);
    // The death count badge (meter.js); off unless turned on.
    this.showDeaths = this.safeGetStorage(this.storageKeys.showDeaths) === "true";
    document.body.classList.toggle("noDeaths", !this.showDeaths);
    const storedDisplayMode = this.safeGetStorage(this.storageKeys.displayMode);
    // "both" was damage with DPS beside it: now damage, with DPS as an extra.
    if (storedDisplayMode === "both" && !this.extraMetrics.includes("dps")) this.extraMetrics.push("dps");
    this.setDisplayMode(storedDisplayMode || this.displayMode, { persist: storedDisplayMode === "both" });

    // History is a browser you leave open: picking a fight launches it into a
    // window of its own so several can be compared, and the list stays put.
    // Only the in-overlay fallback closes itself, because there the list and
    // the fight would otherwise be stacked in the same 30px-row window.
    this.historyUI = window.A2_VIEW !== "settings" && typeof createHistoryUI === "function"
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

    if (window.A2_VIEW !== "settings") {
      this.startPolling();
      this.startWindowTitlePolling();
      this.fetchDps();
    }
  }

  // The green button in the title: shown while a meter update or plugin
  // updates are waiting, in place of a window popping up over the game at
  // start. A click asks about each, as those windows did.
  setupUpdatesButton() {
    const btn = this.updatesBtn;
    if (!btn) return;
    const invoke = window.__TAURI__?.core?.invoke;
    const found = { meter: null, plugins: [] };
    const render = () => {
      const lines = [];
      if (found.meter) {
        lines.push(window.i18n?.format?.("header.updates.meter", { version: found.meter }, `A2Tools DPS Meter ${found.meter} is available`)
          ?? `A2Tools DPS Meter ${found.meter} is available`);
      }
      if (found.plugins.length) {
        const list = found.plugins.map((u) => `${u.name} ${u.to}`).join(", ");
        lines.push(window.i18n?.format?.("header.updates.plugins", { list }, `Plugin updates: ${list}`)
          ?? `Plugin updates: ${list}`);
      }
      btn.hidden = !lines.length;
      const tip = lines.join("\n");
      btn.title = tip;
      btn.setAttribute("aria-label", tip);
    };
    window.ReleaseChecker?.start?.((latest) => {
      found.meter = latest;
      render();
    });
    const takePlugins = (list) => {
      found.plugins = Array.isArray(list) ? list : [];
      render();
    };
    window.__TAURI__?.event?.listen?.("plugin-updates", (e) => takePlugins(e.payload));
    invoke?.("plugin_pending_updates").then(takePlugins).catch(() => {});
    this.i18n?.onChange?.(render);
    let busy = false;
    btn.addEventListener("click", async () => {
      if (busy) return;
      busy = true;
      try {
        if (found.meter) {
          // Installing closes the meter; declined, the button stays.
          if (await window.ReleaseChecker?.offer?.()) found.meter = null;
        }
        if (found.plugins.length) {
          await invoke?.("plugin_offer_updates");
          found.plugins = [];
        }
      } catch (e) {
        console.error("[A2Tools] Update offer failed:", e);
      } finally {
        busy = false;
        render();
      }
    });
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
    } else if (running && this.isDetectingPort) {
      // The port locks a moment after the game is seen, and nothing else may
      // ask again: in Boss mode no row appears until a boss is hit, so
      // "Connecting..." stayed up. Ask on each poll until it locks.
      this.refreshConnectionInfo({ skipSettingsRefresh: true });
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
    this.updateOdyle();
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
    this.invalidateHoverTooltip();


    this.lastSnapshot = null;
    this._barsTargetLabel = null;
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
    this.ttk?.reset?.();

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
          this.i18n?.t?.("battleTime.suspended", "Suspended") ?? "Suspended";
      } else {
        this.analysisStatusEl.textContent =
          this.i18n?.t("battleTime.analysing", "Monitoring") ?? "Monitoring";
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
    this.hoverTooltipDismissed = true;
    if (this.hoverPointerWatch) {
      clearInterval(this.hoverPointerWatch);
      this.hoverPointerWatch = 0;
    }
    if (this.hoverTooltipPositionRaf) {
      cancelAnimationFrame(this.hoverTooltipPositionRaf);
      this.hoverTooltipPositionRaf = 0;
    }
    this.hoverTooltipGeometryKey = "";
    if (!this.hoverTooltipEl?.classList.contains("isVisible")) return;
    this.hoverTooltipEl.classList.remove("isVisible");
    this.hoverTooltipEl.setAttribute("aria-hidden", "true");
    this.hoverTooltipEl.innerHTML = "";
    window.javaBridge?.updateOverlaySize?.();
  }

  invalidateHoverTooltip() {
    this.hoverTooltipCacheByRowId.clear();
    this.hoverTooltipRequestSeqByRowId.clear();
    this.hoverTooltipPendingRowIds.clear();
    this.hoveredDetailsRowId = null;
    this.hideHoverTooltip();
  }

  // A target change makes cached skills stale, but the cursor is still on its
  // row: keep the shown skills until the new answer replaces them. Runs after
  // the rows are rendered, so a row that left the list is really gone.
  refreshHoverTooltipForTarget(rows) {
    const rowId = this.hoveredDetailsRowId;
    this.hoverTooltipCacheByRowId.clear();
    this.hoverTooltipRequestSeqByRowId.clear();
    this.hoverTooltipPendingRowIds.clear();
    this.hoverTooltipQueued = null;
    if (!rowId || this.hoverTooltipDismissed || !this.hoverTooltipEl?.classList.contains("isVisible")) return;
    const row = rows.find((item) => Number(item?.id) === rowId);
    if (!row || !this.elList?.querySelector?.(`.item[data-row-id="${rowId}"]`)) {
      this.hoveredDetailsRowId = null;
      this.hideHoverTooltip();
      return;
    }
    this.applyHoverTooltip(row, { forceRefresh: true, keepContent: true });
  }

  scheduleHoverTooltipPosition() {
    if (this.hoverTooltipPositionRaf) return;
    this.hoverTooltipPositionRaf = requestAnimationFrame(() => {
      this.hoverTooltipPositionRaf = 0;
      if (this.hoverTooltipEl?.classList.contains("isVisible")) this.positionHoverTooltip();
    });
  }

  openHoverDetailsRow(row, event = null) {
    if (!row || this.pinnedDetailsRowId !== null || this.shouldSuppressRowInteractions()) return;
    const rowId = Number(row?.id);
    if (!Number.isFinite(rowId) || rowId <= 0) return;
    let pointerMoved = false;
    if (event && Number.isFinite(event.clientX) && Number.isFinite(event.clientY)) {
      this.hoverMousePos ||= { x: 0, y: 0 };
      pointerMoved = this.hoverMousePos.x !== event.clientX || this.hoverMousePos.y !== event.clientY;
      this.hoverMousePos.x = event.clientX;
      this.hoverMousePos.y = event.clientY;
    }
    const isSameRow = this.hoveredDetailsRowId === rowId;
    this.hoveredDetailsRowId = rowId;
    if (this.detailsUI?.isOpen?.()) {
      return;
    }
    this.hoverTooltipDismissed = false;
    // Skip redundant tooltip renders when still hovering the same row
    if (isSameRow && this.hoverTooltipEl?.classList.contains("isVisible")) {
      if (pointerMoved) this.scheduleHoverTooltipPosition();
      return;
    }
    this.detailsUI?.close?.({ keepPinned: false });
    this.applyHoverTooltip(row);
  }

  getJobColor(job) {
    return this.jobColorMap[String(job || "")] || "";
  }

  /** A row's stagger by skill, most first, as the hover tooltip lists skills. */
  staggerTooltipDetails(row) {
    // The codes are the ones skills are shown by (normalize_skill_id).
    const name = (code) => this.i18n?.getSkillName?.(code, "")
      || (code ? `#${code}` : this.i18n?.t?.("details.stagger.unknownSkill", "Unknown skill") ?? "Unknown skill");
    const skills = Object.entries(row?.staggerSkills || {})
      .map(([code, value]) => ({ code: Number(code), value: Number(value) || 0 }))
      .filter((s) => s.value > 0)
      .sort((a, b) => b.value - a.value || a.code - b.code)
      .slice(0, 5)
      .map((s) => ({ name: name(s.code), code: s.code, dmg: s.value, job: row?.job }));
    return { skills, state: "empty", mode: "stagger" };
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
    const stateText = details?.mode === "stagger"
      ? this.i18n?.t("details.stagger.none", "No stagger gauge was worn down in this fight.") ?? "No stagger gauge was worn down in this fight."
      : this.i18n?.t(`details.hoverTooltip.${tooltipState}`, stateFallback) ?? stateFallback;
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
        const iconHtml = `<img class="skillIcon isPlaceholder" alt="" src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7" onerror="window.skillIcons&&window.skillIcons.handleImgError&&window.skillIcons.handleImgError(this)">`;
        return `<div class="hoverDetailsTooltipSkill"><span class="idx">${index + 1}.</span><span class="name">${iconHtml}<span class="skillName"${skillStyle}>${name}</span></span><span class="dmg">${dmg}</span></div>`;
      })
      .join("");

    const tooltipName = String(row?.name || "-").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    const tooltipClassIcon = row?.job ? `<img class="hoverDetailsTooltipClassIcon" src="./assets/${row.job}.png" alt="" onerror="this.style.display='none'">` : "";
    // Wrapped rather than styled on the header, so the icon keeps its own colour.
    const tooltipNameHtml = row?.isSupporter
      ? `<span class="isSupporter">${tooltipName}</span>`
      : tooltipName;
    const statsHtml = details?.mode === "stagger"
      ? `<span>${this.i18n?.t("details.stagger.title", "Stagger") ?? "Stagger"}: ${Number(row?.stagger) || 0}</span>
        <span>${this.i18n?.t("details.stats.totalDamage", "Total Damage") ?? "Total Damage"}: ${totalDamageText}</span>`
      : `<span>${this.i18n?.t("header.display.dps", "DPS") ?? "DPS"}: ${dpsText}</span>
        <span>${this.i18n?.t("details.stats.totalDamage", "Total Damage") ?? "Total Damage"}: ${totalDamageText}</span>`;
    this.hoverTooltipEl.innerHTML = `
      <div class="hoverDetailsTooltipHeader">${tooltipClassIcon}${tooltipNameHtml}</div>
      <div class="hoverDetailsTooltipStats">
        ${statsHtml}
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
    this.hoverTooltipEl.classList.add("isVisible");
    this.hoverTooltipEl.setAttribute("aria-hidden", "false");
    this.scheduleHoverTooltipPosition();
    this.watchHoverPointer();
  }

  // A row can miss its mouse-leave: the game takes the pointer while it is
  // over the meter, or the window stops taking the mouse under it. The
  // tooltip then stayed up until the row was hovered and left again. While
  // it shows, ask where the pointer really is and close it once it is off
  // the row. On Linux that isn't enough: a native Wayland window can't read
  // the pointer, and under XWayland the pointer read is the last one X saw
  // in the meter while it is over the game or a Wayland window, so still on
  // the row (Hirai, KDE, stuck through both fixes, 2026-10-24). There the
  // tooltip also closes when the pointer leaves the window, and after 4 s
  // with no pointer movement over the meter (the game holding the pointer
  // sends the meter nothing).
  watchHoverPointer() {
    if (this.hoverPointerWatch || !window.javaBridge?.pointerInWindow) return;
    if (document.documentElement?.classList?.contains?.("linux")) this.hoverIdleOnly = true;
    this.watchHoverIdle();
    this.hoverPointerWatch = setInterval(async () => {
      const rowId = this.hoveredDetailsRowId;
      const at = await window.javaBridge.pointerInWindow();
      if (!this.hoverPointerWatch) return;
      if (!at) {
        clearInterval(this.hoverPointerWatch);
        this.hoverPointerWatch = 0;
        this.hoverIdleOnly = true;
        return;
      }
      // Hovering moved on while asking: the next check sees the new row.
      if (rowId !== this.hoveredDetailsRowId) return;
      const over = document.elementFromPoint(at.x, at.y)?.closest?.(".item");
      if (rowId && Number(over?.dataset?.rowId) === rowId) return;
      this.hoveredDetailsRowId = null;
      this.hideHoverTooltip();
    }, 300);
  }

  // The fallback where the pointer can't be trusted (see watchHoverPointer):
  // set up once; it acts on Linux, or once the pointer check found it
  // unreadable.
  watchHoverIdle() {
    if (this.hoverIdleWatch) return;
    this.lastPointerMoveAt = Date.now();
    const moved = () => { this.lastPointerMoveAt = Date.now(); };
    document.addEventListener("mousemove", moved, { passive: true });
    document.documentElement?.addEventListener?.("mouseleave", () => {
      if (this.hoverIdleOnly) {
        this.hoveredDetailsRowId = null;
        this.hideHoverTooltip();
      }
    });
    this.hoverIdleWatch = setInterval(() => {
      // By the tooltip being up, not by a hovered row: on Wayland the page can
      // keep a row :hover after the pointer has gone, and leaving it then
      // cleared the row but kept the tooltip (Hirai, KDE Wayland).
      if (!this.hoverIdleOnly || !this.hoverTooltipEl?.classList.contains("isVisible")) return;
      if (Date.now() - this.lastPointerMoveAt < 4000) return;
      this.hoveredDetailsRowId = null;
      this.hideHoverTooltip();
    }, 500);
  }

  positionHoverTooltip(rowEl = null) {
    const tooltip = this.hoverTooltipEl;
    if (!tooltip) return;
    const container = tooltip.offsetParent;
    const origin = container?.getBoundingClientRect?.() || { left: 0, top: 0 };
    const screen = window.screen || {};
    // On Linux the tooltip stays inside the window: growing the window for it
    // made KWin move the meter up and left while it showed (2026-10-16).
    const inWindow = !!document.documentElement?.classList.contains("linux");
    const availableWidth = Math.max(32, inWindow
      ? window.innerWidth - origin.left
      : (screen.availLeft || 0) + (screen.availWidth || window.innerWidth) - (window.screenX || 0) - origin.left);
    const availableHeight = Math.max(32, inWindow
      ? window.innerHeight - origin.top
      : (screen.availTop || 0) + (screen.availHeight || window.innerHeight) - (window.screenY || 0) - origin.top);
    const margin = 8;
    const gap = 12;
    const limits = {
      maxWidth: `${Math.min(380, availableWidth - margin * 2)}px`,
      minWidth: `${Math.min(200, availableWidth - margin * 2)}px`,
      maxHeight: `${availableHeight - margin * 2}px`,
    };
    for (const [property, value] of Object.entries(limits)) {
      if (tooltip.style[property] !== value) tooltip.style[property] = value;
    }
    const rowBounds = rowEl?.getBoundingClientRect?.();
    const x = (this.hoverMousePos?.x ?? rowBounds?.left ?? origin.left) - origin.left;
    const y = (this.hoverMousePos?.y ?? rowBounds?.bottom ?? origin.top) - origin.top;
    const width = tooltip.offsetWidth;
    const height = tooltip.offsetHeight;
    let left = x + gap;
    let top = y + gap;
    if (left + width + margin > availableWidth) left = x - width - gap;
    if (top + height + margin > availableHeight) top = y - height - gap;
    left = Math.max(margin, Math.min(availableWidth - width - margin, left));
    top = Math.max(margin, Math.min(availableHeight - height - margin, top));
    const transform = `translate3d(${left}px, ${top}px, 0)`;
    if (tooltip.style.transform !== transform) tooltip.style.transform = transform;
    // Pointer motion changes only the composited position. Native window
    // sizing reserves the travel area and only needs content/geometry changes.
    const geometryKey = `${width}:${height}:${availableWidth}:${availableHeight}:${origin.left}:${origin.top}`;
    if (geometryKey !== this.hoverTooltipGeometryKey) {
      this.hoverTooltipGeometryKey = geometryKey;
      window.javaBridge?.updateOverlaySize?.();
    }
  }

  getHoverTooltipSnapshot() {
    // The bridge can receive a DPS event before fetchDps processes it. Use the
    // same source as getBattleDetail, including while meter rendering is paused.
    if (typeof window.dpsData?.getDpsData !== "function") return this.lastJson;
    return window.dpsData.getDpsData();
  }

  getHoverTooltipTargetContext(raw = this.getHoverTooltipSnapshot()) {
    const snapshot = this.safeParseJSON(raw, {});
    const targetIds = Array.isArray(snapshot.detailTargetIds)
      ? [...new Set(snapshot.detailTargetIds.map(Number).filter((id) => Number.isFinite(id) && id > 0))].sort((a, b) => a - b)
      : [];
    return JSON.stringify([snapshot.targetMode, snapshot.targetId, targetIds]);
  }

  applyHoverTooltip(row, { forceRefresh = false, keepContent = false } = {}) {
    const rowId = Number(row?.id);
    if (!Number.isFinite(rowId) || rowId <= 0) return;
    const rowEl = this.elList?.querySelector?.(`.item[data-row-id="${rowId}"]`);
    if (!rowEl) return;
    // Stagger mode: the row carries its stagger by skill, nothing to ask for.
    if (this.displayMode === "stagger") {
      this.renderHoverTooltip(this.staggerTooltipDetails(row), row, rowEl);
      return;
    }

    const snapshot = this.getHoverTooltipSnapshot();
    const stored = this.hoverTooltipCacheByRowId.get(rowId);
    const cached = stored?.snapshot === snapshot ? stored : null;
    if (cached) {
      this.renderHoverTooltip(cached, row, rowEl);
      if (!forceRefresh) return;
    }

    if (!cached && !keepContent) this.renderHoverTooltip({ skills: [], state: "loading" }, row, rowEl);
    if (this.hoverTooltipPendingRowIds.has(rowId)) {
      const pending = this.hoverTooltipRequestSeqByRowId.get(rowId);
      if (pending?.snapshot === snapshot) return;
      // A re-entry after a new snapshot must not revive an older row or target
      // set. Its native work still finishes before the queued replacement.
      this.hoverTooltipRequestSeqByRowId.delete(rowId);
      this.hoverTooltipPendingRowIds.delete(rowId);
    }
    // One summary at a time, latest row wins: rows swept past would otherwise
    // keep fanning out into the shared native work queue.
    if (this.hoverTooltipInFlight) {
      this.hoverTooltipQueued = { row, forceRefresh };
      return;
    }
    this.hoverTooltipInFlight = true;
    // Identity tokens cannot collide with a new request after reset/target change.
    const requestSeq = { snapshot, targetContext: this.getHoverTooltipTargetContext(snapshot) };
    this.hoverTooltipRequestSeqByRowId.set(rowId, requestSeq);
    this.hoverTooltipPendingRowIds.add(rowId);

    this.getDetails(row, { maxSkills: 5, showSkillIcons: false, summaryOnly: true })
      .then((details) => {
        if (this.hoverTooltipRequestSeqByRowId.get(rowId) !== requestSeq
          || requestSeq.targetContext !== this.getHoverTooltipTargetContext()) return;
        this.hoverTooltipPendingRowIds.delete(rowId);
        const lightweightDetails = { skills: Array.isArray(details?.skills) ? details.skills.slice(0, 5) : [], snapshot };
        this.hoverTooltipCacheByRowId.set(rowId, lightweightDetails);
        if (this.hoveredDetailsRowId !== rowId || this.hoverTooltipDismissed) return;
        // Damage updates within the same target set can continue while a slow
        // request finishes. Keep the current row's totals without restarting it.
        const currentRow = this.latestRowsById?.get(String(rowId)) || row;
        this.renderHoverTooltip(lightweightDetails, currentRow, rowEl);
      })
      .catch((error) => {
        if (this.hoverTooltipRequestSeqByRowId.get(rowId) !== requestSeq
          || requestSeq.targetContext !== this.getHoverTooltipTargetContext()) return;
        this.hoverTooltipPendingRowIds.delete(rowId);
        if (this.hoveredDetailsRowId !== rowId || this.hoverTooltipDismissed) return;
        window.javaBridge?.logToDebug?.(`Hover skill details failed: ${error?.message || error}`);
        this.renderHoverTooltip({ skills: [], state: "error" }, row, rowEl);
      })
      .finally(() => {
        if (this.hoverTooltipRequestSeqByRowId.get(rowId) === requestSeq) {
          this.hoverTooltipRequestSeqByRowId.delete(rowId);
          this.hoverTooltipPendingRowIds.delete(rowId);
        }
        this.hoverTooltipInFlight = false;
        let next = this.hoverTooltipQueued;
        this.hoverTooltipQueued = null;
        if (!next && requestSeq.targetContext !== this.getHoverTooltipTargetContext()
          && this.hoveredDetailsRowId === rowId && !this.hoverTooltipDismissed) {
          const currentRow = this.latestRowsById?.get(String(rowId));
          if (currentRow) next = { row: currentRow, forceRefresh: true };
        }
        if (next && Number(next.row?.id) === this.hoveredDetailsRowId && !this.hoverTooltipDismissed) {
          const currentRow = this.latestRowsById?.get(String(next.row.id)) || next.row;
          this.applyHoverTooltip(currentRow, { forceRefresh: next.forceRefresh, keepContent: true });
        }
      });
  }




  _isAnyPanelOpen() {
    return this.settingsPanel?.classList.contains("isOpen")
      || this.historyUI?.isOpen?.()
      || this.detailsUI?.isOpen?.();
  }

  fetchDps() {
    if (window.A2_VIEW === "settings") return;
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
      // A status message keeps the line shown: hiding it here only for
      // updateConnectionStatusUi to show it again changed the window's classes
      // every 100 ms, each change a layout and a window-size check.
      this.battleTime.setVisible(!!this._connectionStatusOverride);
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

      // As above: a status message keeps the line shown.
      this.battleTime.setVisible(shouldBeVisible || !!this._connectionStatusOverride);
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
      targetStaggerPct,
      targetIsBoss,
      dungeonId,
      targetMobCode,
      partyGeneration,
      zoneGeneration,
      instanceGeneration,
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
        this._barsTargetLabel = null;
        this._lastRenderedListSignature = "";
        this._lastRenderedRowsSummary = null;
        this.meterUI?.onResetMeterUi?.();
        return;
      }
    }

    this.lastJson = raw;
    this.applyLocalPlayerIdUpdate(localPlayerId, "backend local id update");
    // The backend's id, once it has one, is who you are. A row still under
    // your name from before an id change (a dungeon's second run, with the
    // first run's rows on screen) took you back to the old id on every
    // update, and the window asked the backend for it twice a poll
    // (2026-10-14: "ignored 1566", 228 times in two minutes).
    if (!(Number(localPlayerId) > 0)) this.updateLocalPlayerIdentity(rows);
    this._lastBattleTimeMs = battleTimeMs;
    const targetChanged = targetId !== previousTargetId || targetMode !== previousTargetMode;
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
    // Someone joined or left, or the party ended: the rows kept from the last
    // fight name the old party, so they give way to what the backend sends.
    // An empty update then clears the meter (the party ended, its fights
    // flushed) instead of being ignored below.
    const partyChanged = this._lastPartyGeneration != null && partyGeneration !== this._lastPartyGeneration;
    this._lastPartyGeneration = partyGeneration;
    if (partyChanged && rows.length === 0) {
      // None kept: the next rows (yours, a new party's) are taken as they come.
      this.lastSnapshot = null;
      this._barsTargetLabel = null;
      this._lastRenderedListSignature = "";
      this._lastRenderedRowsSummary = null;
      this.meterUI?.onResetMeterUi?.();
      return;
    }
    if (partyChanged && isOutOfCombat) {
      this.lastSnapshot = rows;
    }
    // The zone changed as "Clear the meter" says (entering or leaving a
    // dungeon by default, "go again" too): the last fight leaves the screen
    // once the backend has cleared it (its next empty update), rather than
    // staying up until the next fight. Kept on teleports inside a dungeon,
    // and always with "Never". Waits 20 s at most: a teleport mid-fight
    // clears nothing.
    const clearOn = this.safeGetSetting(this.storageKeys.clearOnZone) || SETTING_CHOICES.clearOnZone.fallback;
    const zoneMark = clearOn === "any" ? `any:${zoneGeneration}` : clearOn === "never" ? "never" : `instance:${instanceGeneration}`;
    if (this._zoneMark != null && zoneMark !== this._zoneMark && clearOn !== "never"
        && zoneMark.split(":")[0] === this._zoneMark.split(":")[0]) {
      this._zoneClearUntil = Date.now() + 20_000;
    }
    this._zoneMark = zoneMark;
    if (this._zoneClearUntil > Date.now() && rows.length === 0) {
      this._zoneClearUntil = 0;
      this.lastSnapshot = null;
      this._barsTargetLabel = null;
      this._lastRenderedListSignature = "";
      this._lastRenderedRowsSummary = null;
      this.meterUI?.onResetMeterUi?.();
      return;
    }
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
      rowsToRender = rowsToRender.filter((row) => row.isUser); // by id or name: an unnamed row is still yours
    }
    // render
    this.lastDungeonId = dungeonId;
    let nextTargetLabel = this.getTargetLabel({ targetId, targetName, targetMode, dungeonId, targetMobCode });
    // The title names the fight the bars below are from. After a zone change
    // or leaving a dungeon the backend has no target, and the title fell back
    // to the mode ("BOSS TARGETS") while the last fight's bars stayed up: keep
    // that fight's title while its bars are shown and no new target has come.
    const tracksOne = targetMode === "bossTargets" || targetMode === "lastHitByMe";
    const noTarget = !(Number(targetId) > 0) && !String(targetName || "").trim();
    if (!tracksOne) {
      this._barsTargetLabel = null;
    } else if (rowsToRender.length > 0 && !noTarget) {
      this._barsTargetLabel = nextTargetLabel;
    } else if (rowsToRender.length > 0 && noTarget && this._barsTargetLabel) {
      nextTargetLabel = this._barsTargetLabel;
    }
    if (this.elBossName) {
      if (this.elBossName.textContent !== nextTargetLabel) {
        this.elBossName.textContent = nextTargetLabel;
        this.fitBossName();
      }
      this.elBossName.classList.toggle("isAllTargets", targetMode === "allTargets");
    }
    this.updateBossHpBar(targetMaxHp, targetTotalDamage, targetCurrentHp, targetStaggerPct);
    this.updateTtk(targetIsBoss, targetId, battleTimeMs, targetMaxHp, targetTotalDamage, targetCurrentHp);
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
    if (targetChanged) this.refreshHoverTooltipForTarget(rowsToRender);
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
    const rows = this.buildRowsFromMapObject(mapObj, localPlayerId);

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
    const targetStaggerPct = Number.isFinite(Number(payload?.targetStaggerPct)) ? Number(payload.targetStaggerPct) : -1;
    const targetIsBoss = payload?.targetIsBoss === true;
    const targetMobCode = Math.trunc(Number(payload?.targetMobCode)) || 0;
    const partyGeneration = Math.trunc(Number(payload?.partyGeneration)) || 0;
    const zoneGeneration = Math.trunc(Number(payload?.zoneGeneration)) || 0;
    const instanceGeneration = Math.trunc(Number(payload?.instanceGeneration)) || 0;

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
      targetStaggerPct,
      targetIsBoss,
      dungeonId,
      targetMobCode,
      partyGeneration,
      zoneGeneration,
      instanceGeneration,
    };
  }

  // `localPlayerId`: who the backend says you are. Your row is yours by that
  // id even before it has a name: matching by name alone left an unnamed
  // "#id" row of yours unmarked, so it was neither pinned nor kept past the
  // player limit, and you dropped off a busy meter.
  buildRowsFromMapObject(mapObj, localPlayerId = null) {
    const rows = [];
    const localId = Number(localPlayerId);
    const hasLocalId = Number.isFinite(localId) && localId > 0;

    for (const [id, value] of Object.entries(mapObj || {})) {
      const numericId = Number(id);
      if (Number.isFinite(numericId) && numericId <= 0) {
        continue;
      }
      const isObj = value && typeof value === "object";

      const job = isObj ? (value.job ?? "") : "";
      const nickname = isObj ? (value.nickname ?? "") : "";
      const idText = String(id);
      // Everyone not in your party on an open-world boss, from the HP it
      // lost (dps_calculator.rs world_boss_rows).
      const isOthers = !!(isObj && value.isOthers);
      const hasNickname = !!nickname && nickname !== idText;
      const isIdentifying = !hasNickname;
      const name = isOthers
        ? this.i18n?.t?.("meter.others", "Others") ?? "Others"
        : hasNickname ? nickname : idText;

      const dpsRaw = isObj ? value.dps : value;
      const dps = Math.trunc(Number(dpsRaw)) || 0;
      const totalDamage = Math.trunc(Number(isObj ? value.amount : 0)) || 0;
      const hps = Math.trunc(Number(isObj ? value.hps : 0)) || 0;
      const healAmount = Math.trunc(Number(isObj ? value.healAmount : 0)) || 0;
      const damageReceived = Math.trunc(Number(isObj ? value.damageReceived : 0)) || 0;
      // Stagger taken off the target's gauge in the fight (combat/stagger.rs).
      const stagger = Math.trunc(Number(isObj ? value.stagger : 0)) || 0;
      // The same by skill code, for the tooltip in Stagger mode.
      const staggerSkills = isObj && value.staggerSkills && typeof value.staggerSkills === "object"
        ? value.staggerSkills : {};

      // 소수점 한자리
      const contribRaw = isObj ? Number(value.damageContribution) : NaN;
      const damageContribution = Number.isFinite(contribRaw)
        ? Math.round(contribRaw * 10) / 10
        : NaN;

      if (!Number.isFinite(dps) && healAmount <= 0) {
        continue;
      }

      // Combat power comes from the party roster packet, so it only exists for
      // players actually in your party; 0 means "unknown", not "zero CP".
      const combatPower = Math.trunc(Number(isObj ? value.combatPower : 0)) || 0;
      const gearScore = Math.trunc(Number(isObj ? value.gearScore : 0)) || 0;

      rows.push({
        id: String(id),
        name,
        job,
        dps,
        totalDamage,
        hps,
        healAmount,
        damageReceived,
        stagger,
        staggerSkills,
        damageContribution,
        combatPower,
        gearScore,
        isUser: !isOthers && ((!!name && name === this.USER_NAME) || (hasLocalId && numericId === localId)),
        isOthers,
        isIdentifying,
        // Resolved in Rust against a downloaded roster; the frontend only
        // renders it. Cosmetic only — it must not reach sorting or bar colour.
        isSupporter: !!(isObj && value.isSupporter) && this.showSupporterColors !== false,
        // A party member who runs A2Tools too (party_presence.rs).
        usesA2tools: !!(isObj && value.usesA2tools),
        deaths: isObj ? Math.max(0, Number(value.deaths) || 0) : 0,
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

  /** Pixels between player bars, 0-16, or null for the skin's own spacing. */
  // The sidebar's sections: one shown at a time, as a tablist (arrow keys,
  // Home and End move between the sections that are listed).
  initSettingsSections() {
    const items = [...document.querySelectorAll(".settingsNavItem")];
    if (!items.length) return;
    const show = (key, { focus = false } = {}) => {
      for (const item of items) {
        const on = item.dataset.section === key;
        item.classList.toggle("isActive", on);
        item.setAttribute("aria-selected", String(on));
        item.tabIndex = on ? 0 : -1;
        if (on && focus) item.focus();
      }
      for (const section of document.querySelectorAll(".settingsSection")) {
        const on = section.dataset.section === key;
        section.hidden = !on;
        section.classList.toggle("isActive", on);
      }
      const pane = document.querySelector(".settingsPane");
      if (pane) pane.scrollTop = 0;
      // Which section is open, for what shows beside it (Appearance's bar preview).
      const body = document.querySelector(".settingsBody");
      if (body) body.dataset.section = key;
      // For sections that load when first opened (What's new).
      document.dispatchEvent(new CustomEvent("settingsSectionShown", { detail: key }));
    };
    const steps = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 };
    for (const item of items) {
      item.addEventListener("click", () => show(item.dataset.section));
      item.addEventListener("keydown", (event) => {
        if (!(event.key in steps) && event.key !== "Home" && event.key !== "End") return;
        event.preventDefault();
        const listed = items.filter((el) => el.getClientRects().length > 0);
        const at = listed.indexOf(item);
        const next = event.key === "Home" ? listed[0]
          : event.key === "End" ? listed[listed.length - 1]
            : listed[(at + steps[event.key] + listed.length) % listed.length];
        if (next) show(next.dataset.section, { focus: true });
      });
    }
    // Opened on one section (the plugin dock's Settings button), or told to
    // switch to it while open.
    const known = (key) => typeof key === "string" && items.some((item) => item.dataset.section === key);
    const invoke = window.__TAURI__?.core?.invoke;
    invoke?.("take_settings_section").then((key) => { if (known(key)) show(key); }).catch(() => {});
    window.__TAURI__?.event?.listen?.("settings-section", (event) => { if (known(event.payload)) show(event.payload); });
  }

  normalizeBarGap(value) {
    if (value === null || value === undefined || String(value).trim() === "") return null;
    const numeric = Number(value);
    if (!Number.isFinite(numeric)) return null;
    return Math.min(16, Math.max(0, Math.round(numeric)));
  }

  /** The skin's spacing between bars, for the slider before it is first moved. */
  defaultBarGap() {
    return this.slimMode ? 2 : 3;
  }

  // "Spacing between player bars" (a user asked for it). Unset, the skin's
  // own spacing stands; set, it applies to every skin. The window fits its
  // rows, so it is resized after.
  applyBarGap(value, { persist } = {}) {
    const normalized = this.normalizeBarGap(value);
    const root = document.documentElement.style;
    if (normalized === null) root.removeProperty("--meter-bar-gap");
    else root.setProperty("--meter-bar-gap", `${normalized}px`);
    if (persist && normalized !== null) {
      this.safeSetSetting(this.storageKeys.barGap, String(normalized));
    }
    if (window.A2_VIEW === "main") window.javaBridge?.updateOverlaySize?.();
    return normalized;
  }

  applyMeterFillOpacity(percent, { persist } = {}) {
    const normalized = this.normalizeMeterOpacity(percent, 100);
    document.documentElement.style.setProperty("--meter-fill-opacity", String(normalized / 100));
    if (persist) {
      this.safeSetSetting(this.storageKeys.meterFillOpacity, String(normalized));
    }
  }

  // The bars' colour strength, 0% (grey) to 200%, 100% as the theme draws it.
  normalizeMeterSaturation(value, fallback = 100) {
    const numeric = Number(value);
    if (!Number.isFinite(numeric) || String(value ?? "").trim() === "") return fallback;
    return Math.min(200, Math.max(0, Math.round(numeric / 5) * 5));
  }

  applyMeterFillSaturation(percent, { persist } = {}) {
    const normalized = this.normalizeMeterSaturation(percent, 100);
    const root = document.documentElement.style;
    root.setProperty("--meter-fill-saturation", String(normalized / 100));
    // Stronger colour also turns brighter, toward neon, and white text on it
    // harder to read: above 100% the bar darkens a little as it goes, to 85%
    // at 200%, so it reads as a deeper colour instead.
    root.setProperty("--meter-fill-brightness", String(1 - Math.max(0, normalized - 100) / 100 * 0.15));
    if (persist) {
      this.safeSetSetting(this.storageKeys.meterFillSaturation, String(normalized));
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
    const numeric = Number(percent);
    const normalized = Number.isFinite(numeric) ? Math.max(0, Math.min(100, Math.round(numeric))) : 40;
    document.documentElement.style.setProperty("--window-opacity", String(normalized / 100));
    if (persist) {
      this.safeSetSetting(this.storageKeys.windowOpacity, String(normalized));
    }
    // The title bar follows the background until it is set apart.
    if (!this.titleOpacitySet()) this.syncSettingsRangeValue?.(this.titleOpacityInput, this.titleOpacityValue, normalized);
  }

  // The title bar's own opacity, or null: it follows the background's.
  titleOpacitySet() {
    const stored = this.safeGetStorage(this.storageKeys.titleOpacity);
    return stored !== null && String(stored).trim() !== "" && Number.isFinite(Number(stored));
  }

  applyTitleOpacity(percent, { persist } = {}) {
    const numeric = Number(percent);
    const normalized = Number.isFinite(numeric) ? Math.max(0, Math.min(100, Math.round(numeric))) : 40;
    document.documentElement.style.setProperty("--title-opacity", String(normalized / 100));
    if (persist) {
      this.safeSetSetting(this.storageKeys.titleOpacity, String(normalized));
    }
  }

  // The title bar's height, for the background layer behind it.
  watchTitleHeight() {
    const meter = document.querySelector(".meter");
    const header = meter?.querySelector(".header");
    if (!meter || !header || typeof ResizeObserver !== "function") return;
    const sync = () => meter.style.setProperty("--title-h", `${header.offsetHeight}px`);
    new ResizeObserver(sync).observe(header);
    sync();
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
    this._barsTargetLabel = null;
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
    { targetId = null, attackerIds = null, totalTargetDamage = null, showSkillIcons = false, maxSkills = null, summaryOnly = false } = {}
  ) {
    let raw = null;
    let backendFiltered = false;
    if (window._historyDetailsOverride) {
      raw = window._historyDetailsOverride;
    } else if (targetId && window.dpsData?.getTargetDetails) {
      const payload = Array.isArray(attackerIds) ? JSON.stringify(attackerIds) : "";
      raw = await window.dpsData.getTargetDetails(targetId, payload);
      // null is the bridge's IPC failure; an empty fight still arrives as JSON.
      if (raw === null) throw new Error(`skill details failed for target ${targetId}`);
      backendFiltered = true;
    } else {
      raw = await window.dpsData?.getBattleDetail?.(row.id, { summaryOnly });
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
      shieldBlock = 0,
      ironWall = 0,
      regeneration = 0,
      perfectBlock = 0,
      miss = 0,
      resist = 0,
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
      // A skill that only missed or was resisted still gets its row.
      if (dmgInt <= 0 && !(Number(miss) > 0 || Number(resist) > 0)) {
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
        shieldBlock: Number(shieldBlock) || 0,
        ironWall: Number(ironWall) || 0,
        regeneration: Number(regeneration) || 0,
        perfectBlock: Number(perfectBlock) || 0,
        miss: Number(miss) || 0,
        resist: Number(resist) || 0,
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
          shieldBlock: value.shieldBlock,
          ironWall: value.ironWall,
          // Saved before the rename.
          regeneration: value.regeneration ?? value.smite,
          perfectBlock: value.perfectBlock ?? value.powershard,
          miss: value.miss,
          resist: value.resist,
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


    if (summaryOnly) {
      skills.sort((a, b) => b.dmg - a.dmg);
      return { skills: skills.slice(0, 5) };
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
        crit: 0, parry: 0, back: 0, frontal: 0, perfect: 0, double: 0,
        regen: 0, multiHitCount: 0, multiHitDamage: 0, multiHitHits: 0,
        minDmg: 0, maxDmg: 0, job: v.job ?? "", specs: null,
        // When each heal landed in the fight (meter 2.0.56 on): HEAL's HPS chart.
        hitTimestamps: Array.isArray(v.hitTimestamps) ? v.hitTimestamps : [],
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
      totalHits: totalTimes,
      multiHitCount: totalMultiHitCount,
      multiHitDamage: totalMultiHitDamage,
      multiHitPct: totalTimes > 0 ? Math.round((totalMultiHitCount / totalTimes) * 1000) / 10 : 0,
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
      // Players who died in the fight (ms from its start), for the DPS chart.
      deaths: Array.isArray(detailObj?.deaths) ? detailObj.deaths : null,
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
    this.bindTargetMenu();
    this.bindMetricMenu();
    this.bindFooterMenu();
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
    if (!this.accountStateEl) return;
    this.accountStateEl.textContent = text;
    // A short state stays on one line beside its button; a long one (an
    // error saying why sign-in failed) wraps rather than being cut off.
    this.accountStateEl.title = text || "";
    this.accountStateEl.classList.toggle("isLong", String(text || "").length > 40);
  }

  // Reflects whatever the backend reports. Called on open, after sign-out, and
  // from the "account-changed" event the device-grant poll emits when the
  // player approves or declines in the browser.
  async refreshAccountPanel(result) {
    if (!this.accountStateEl) return;
    const request = this._accountPanelRequest = (this._accountPanelRequest || 0) + 1;
    const current = () => request === this._accountPanelRequest;
    // Signed in: the fights this PC uploaded as a guest became theirs.
    if (result && result.connected && Number(result.claimed) > 0) {
      this._claimedNote = window.i18n?.format?.(
        "settings.account.claimed", { count: Number(result.claimed) },
        `${Number(result.claimed)} fights uploaded from this PC are now yours.`
      );
    }
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
        if (!current()) return;
        if (seen) this.paintAccount(seen.who);
      } catch {}
    }
    if (!current()) return;

    let who = null;
    try {
      who = await window.javaBridge?.accountStatus?.();
    } catch (err) {
      if (!current()) return;
      // A token is stored but could not be checked (keyring locked, server
      // down): say why, and do not ask for a new sign-in.
      this.paintAccountUnavailable(typeof err === "string" ? err : err?.message || String(err));
      return;
    }
    if (current()) this.paintAccount(who);
  }

  paintAccountUnavailable(message) {
    if (this.accountCodeBox?.style.display === "block") return;
    if (this.accountConnectBtn) this.accountConnectBtn.style.display = "none";
    if (this.accountSignOutBtn) this.accountSignOutBtn.style.display = "";
    this.setAccountState(message);
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

    this.paintGuest(signedIn);
    if (!signedIn) {
      this.setAccountState(window.i18n?.t?.("settings.account.signedOut", "Not signed in"));
      this.setSupporter(false);
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
    this.setSupporter(who.supporter);
    if (this._claimedNote) label += " · " + this._claimedNote;
    this.setAccountState(label);
  }

  // Signed out: uploading without an account instead of to one, and, once
  // this PC has guest fights, how to claim them. Signed in: the account's.
  paintGuest(signedIn) {
    document.querySelector(".autoUploadRow")?.style.setProperty("display", signedIn ? "" : "none");
    for (const row of document.querySelectorAll(".guestUploadRow")) row.style.setProperty("display", signedIn ? "none" : "");
    document.querySelector(".privacySignedIn")?.style.setProperty("display", signedIn ? "" : "none");
    if (!this.guestClaimRow) return;
    if (signedIn) {
      this.guestClaimRow.style.display = "none";
      return;
    }
    window.javaBridge?.guestStatus?.().then((g) => {
      this.guestClaimRow.style.display = g?.hasGuest ? "" : "none";
    }).catch(() => {});
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
    this.updateRateDropdownBtn = document.querySelector(".updateRateDropdownBtn");
    this.updateRateDropdownMenu = document.querySelector(".updateRateDropdownMenu");
    this.trainSelectionModeDropdownBtn = document.querySelector(".trainSelectionModeDropdownBtn");
    this.barTextureDropdownBtn = document.querySelector(".barTextureDropdownBtn");
    this.barTextureDropdownMenu = document.querySelector(".barTextureDropdownMenu");
    this.barTextureHint = document.querySelector(".barTextureHint");
    this.textureOffsetCheckbox = document.querySelector(".textureOffsetCheckbox");
    this.clearOnZoneDropdownBtn = document.querySelector(".clearOnZoneDropdownBtn");
    this.clearOnZoneDropdownMenu = document.querySelector(".clearOnZoneDropdownMenu");
    this.hideOutOfCombatDropdownBtn = document.querySelector(".hideOutOfCombatDropdownBtn");
    this.hideOutOfCombatDropdownMenu = document.querySelector(".hideOutOfCombatDropdownMenu");
    this.discordStatDropdownBtn = document.querySelector(".discordStatDropdownBtn");
    this.discordStatDropdownMenu = document.querySelector(".discordStatDropdownMenu");
    this.trainSelectionModeDropdownMenu = document.querySelector(".trainSelectionModeDropdownMenu");
    this.meterAnchorDropdownBtn = document.querySelector(".meterAnchorDropdownBtn");
    this.meterAnchorDropdownMenu = document.querySelector(".meterAnchorDropdownMenu");
    this.meterAnchorRow = this.meterAnchorDropdownBtn?.closest?.(".settingsRow") || null;
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
    this.runAsAdminCheckbox = document.querySelector(".runAsAdminCheckbox");
    this.shareDataCheckbox = document.querySelector(".shareDataCheckbox");
    this.partyPresenceCheckbox = document.querySelector(".partyPresenceCheckbox");
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
    this.guestUploadCheckbox = document.querySelector(".guestUploadCheckbox");
    this.guestClaimRow = document.querySelector(".guestClaimRow");
    this.guestClaimText = document.querySelector(".guestClaimText");
    this.guestClaimBtn = document.querySelector(".guestClaimBtn");
    this.accountStateEl = document.querySelector(".accountState");
    this.accountHintEl = document.querySelector(".accountHint");
    this.accountConnectBtn = document.querySelector(".accountConnectBtn");
    this.accountSignOutBtn = document.querySelector(".accountSignOutBtn");
    this.accountCodeBox = document.querySelector(".accountCodeBox");
    this.accountCodeEl = document.querySelector(".accountCode");
    this.playerDpsBoldCheckbox = document.querySelector(".playerDpsBoldCheckbox");
    this.meterOpacityInput = document.querySelector(".meterOpacityInput");
    this.meterOpacityValue = document.querySelector(".meterOpacityValue");
    this.meterSaturationInput = document.querySelector(".meterSaturationInput");
    this.meterSaturationValue = document.querySelector(".meterSaturationValue");
    this.bossNameSizeInput = document.querySelector(".bossNameSizeInput");
    this.bossNameSizeValue = document.querySelector(".bossNameSizeValue");
    this.windowOpacityInput = document.querySelector(".windowOpacityInput");
    this.windowOpacityValue = document.querySelector(".windowOpacityValue");
    this.titleOpacityInput = document.querySelector(".titleOpacityInput");
    this.titleOpacityValue = document.querySelector(".titleOpacityValue");
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
    // The Donate page and the sponsor's link: in the browser, not the meter.
    for (const link of document.querySelectorAll(".supportModal [data-open-url]")) {
      link.addEventListener("click", (event) => {
        event.preventDefault();
        window.javaBridge?.openBrowser?.(link.href);
      });
    }
    this.kofiButton = document.querySelector(".kofiButton");
    this.kofiWidget = document.querySelector(".kofiWidget");
    this.quitButton = document.querySelector(".quitButton");
    this.settingsVersionValue = document.querySelector(".settingsVersionValue");
    this.settingsVersionLink = document.querySelector(".settingsVersionLink");
    this.languageDropdownBtn = document.querySelector(".languageDropdownBtn");
    this.languageDropdownMenu = document.querySelector(".languageDropdownMenu");
    this.themeDropdownBtn = document.querySelector(".themeDropdownBtn");
    this.themeDropdownMenu = document.querySelector(".themeDropdownMenu");
    this.settingsThemeDropdownBtn = document.querySelector(".settingsThemeDropdownBtn");
    this.settingsThemeDropdownMenu = document.querySelector(".settingsThemeDropdownMenu");
    this.fontDropdownBtn = document.querySelector(".fontDropdownBtn");
    this.fontDropdownMenu = document.querySelector(".fontDropdownMenu");
    this.settingsSelections = {
      language: "en",
      theme: this.theme,
      settingsTheme: "meter",
      font: "meter",
      defaultMeterMode: "bossTargets",
      allTargetsWindowMs: "120000",
      trainSelectionMode: "all",
      meterAnchor: "top",
      partyStat: "combatPower",
      discordStat: "off",
      barTexture: "none",
    };
    // "Meter stays fixed at" only where the meter can keep an edge, and
    // unset, the edge this platform kept (app.rs build_features).
    Promise.resolve(window.a2BuildReady).then((build) => {
      if (build?.meterAnchorDefault === "middle") SETTING_CHOICES.meterAnchor.fallback = "middle";
      const anchor = pickSettingChoice("meterAnchor", this.safeGetSetting(this.storageKeys.meterAnchor));
      this.settingsSelections.meterAnchor = anchor;
      this.syncSettingsDropdownSelection(this.meterAnchorDropdownBtn, this.meterAnchorDropdownMenu, anchor);
      this.updateMeterAnchorVisibility(build?.meterAnchor !== false);
    });

    // The meter already applied these settings. Opening a form must not change
    // the live combat selection or resend saved parameters to the backend.
    const syncBackend = window.A2_VIEW !== "settings";
    const storedName = this.safeGetStorage(this.storageKeys.userName) || "";
    const storedAllTargetsWindowMs = this.safeGetSetting(this.storageKeys.allTargetsWindowMs) ||
      this.safeGetStorage(this.storageKeys.allTargetsWindowMs) ||
      "120000";
    const storedPartyStat = this.safeGetSetting(this.storageKeys.partyStat) || "combatPower";
    const storedTrainSelectionMode = this.safeGetSetting(this.storageKeys.trainSelectionMode) ||
      this.safeGetStorage(this.storageKeys.trainSelectionMode) ||
      "all";
    let storedMeterOpacity = this.safeGetSetting(this.storageKeys.meterFillOpacity) ||
      this.safeGetStorage(this.storageKeys.meterFillOpacity);
    if (syncBackend && this.safeGetSetting("dpsMeter.migration.opacityReset1") !== "done") {
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

    this.setUserName(storedName, { persist: false, syncBackend });
    this.setOnlyShowUser(false, { persist: false });
    this.setDebugLogging(storedDebugLogging, { persist: false, syncBackend });
    this.setPinMeToTop(storedPinMeToTop, { persist: false });
    // The Classic skins are retired: Classic becomes Standard, Classic Slim
    // becomes Slim (slimMode is kept as it was). Saved once, so every window
    // and the next start agree.
    const wasClassic = this.safeGetSetting(this.storageKeys.betaUi) === "false";
    this.setBetaUi(true, { persist: wasClassic && syncBackend });
    const storedSlimMode = this.safeGetSetting(this.storageKeys.slimMode) === "true";
    this.setSlimMode(storedSlimMode, { persist: false });
    this.setMainPlayerNamesBold(storedMainPlayerNamesBold, { persist: false });
    this.setMainPlayerDpsBold(storedMainPlayerDpsBold, { persist: false });
    if (syncBackend && (mainPlayerNamesBoldSetting === null || mainPlayerNamesBoldSetting === undefined || mainPlayerNamesBoldSetting === "")) {
      this.safeSetSetting(this.storageKeys.mainPlayerNamesBold, "true");
    }
    if (syncBackend && (mainPlayerDpsBoldSetting === null || mainPlayerDpsBoldSetting === undefined || mainPlayerDpsBoldSetting === "")) {
      this.safeSetSetting(this.storageKeys.mainPlayerDpsBold, "true");
    }
    const normalizedDefaultMode = pickSettingChoice("defaultMeterMode", storedDefaultMeterMode);
    this.settingsSelections.defaultMeterMode = normalizedDefaultMode;
    this.setTargetSelection(normalizedDefaultMode, {
      persist: false,
      syncBackend,
      reason: "default meter mode setting",
    });
    this.applyTheme(storedTheme || this.theme, { persist: false });
    this.applySettingsTheme(this.safeGetSetting(this.storageKeys.settingsTheme));
    this.applyFont(this.safeGetSetting(this.storageKeys.font));
    if (storedLanguage && storedLanguage !== this.i18n?.getLanguage?.()) {
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
      if (window.A2_VIEW === "settings") window.addEventListener("settings-hidden", () => {
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
        window.javaBridge?.bindLocalActorId?.(value, true);
        if (this.USER_NAME) {
          window.javaBridge?.bindLocalNickname?.(value, this.USER_NAME);
        }
        this.setUserName(this.USER_NAME, { persist: true, syncBackend: true });
      });
    }

    this.settingsSelections.updateRateMs = pickSettingChoice("updateRateMs", this.safeGetSetting(this.storageKeys.updateRateMs));
    this.applyUpdateRate(this.settingsSelections.updateRateMs);
    const selectedWindow = pickSettingChoice("allTargetsWindowMs", storedAllTargetsWindowMs);
    this.settingsSelections.allTargetsWindowMs = selectedWindow;
    if (syncBackend) {
      this.safeSetSetting(this.storageKeys.allTargetsWindowMs, selectedWindow);
      window.javaBridge?.setAllTargetsWindowMs?.(selectedWindow);
    }

    this.partyStat = pickSettingChoice("partyStat", storedPartyStat);
    this.settingsSelections.partyStat = this.partyStat;
    this.syncPreviewPartyStat();
    this.settingsSelections.discordStat = pickSettingChoice("discordStat", this.safeGetSetting(this.storageKeys.discordStat));
    this.settingsSelections.clearOnZone = pickSettingChoice("clearOnZone", this.safeGetSetting(this.storageKeys.clearOnZone));
    this.settingsSelections.meterAnchor = pickSettingChoice("meterAnchor", this.safeGetSetting(this.storageKeys.meterAnchor));
    this.settingsSelections.hideOutOfCombat = pickSettingChoice("hideOutOfCombat", this.safeGetSetting(this.storageKeys.hideOutOfCombat));
    this.barTexture = pickSettingChoice("barTexture", this.safeGetSetting(this.storageKeys.barTexture));
    this.settingsSelections.barTexture = this.barTexture;
    this.applyBarTexture(this.barTexture);
    this.setTextureOffset(this.safeGetSetting(this.storageKeys.barTextureOffset) === "true");
    this.refreshSupporterStatus();
    const selectedMode = pickSettingChoice("trainSelectionMode", storedTrainSelectionMode);
    this.trainSelectionMode = selectedMode;
    this.settingsSelections.trainSelectionMode = selectedMode;
    if (syncBackend) {
      this.safeSetSetting(this.storageKeys.trainSelectionMode, selectedMode);
      window.javaBridge?.setTrainSelectionMode?.(selectedMode);
    }

    if (this.bossLogsCheckbox) {
      const storedBossLogs = this.safeGetSetting(this.storageKeys.bossLogs) === "true";
      this.bossLogsCheckbox.checked = storedBossLogs;
      this.bossLogsCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.bossLogs, String(isChecked));
        window.javaBridge?.setBossLogsEnabled?.(isChecked);
      });
    }
    // A2Tools users in the party see each other (party_presence.rs): on
    // unless turned off.
    if (this.partyPresenceCheckbox) {
      this.partyPresenceCheckbox.checked = this.safeGetSetting(this.storageKeys.partyPresence) === "true";
      this.partyPresenceCheckbox.addEventListener("change", (event) => {
        this.safeSetSetting(this.storageKeys.partyPresence, event.target?.checked ? "true" : "false");
      });
    }
    // Field boss timers and Supply Requests, shared with a2tools.app
    // (boss_report.rs shares_data): on unless turned off, here or, before
    // 2.0.78, with either of the two switches this one replaced.
    if (this.shareDataCheckbox) {
      const set = this.safeGetSetting(this.storageKeys.shareData);
      this.shareDataCheckbox.checked = set != null && set !== ""
        ? set !== "false"
        : ["dpsMeter.shareBossTimers", "dpsMeter.shareSupplyRequests"].every((k) => this.safeGetSetting(k) !== "false");
      this.shareDataCheckbox.addEventListener("change", (event) => {
        this.safeSetSetting(this.storageKeys.shareData, event.target?.checked ? "true" : "false");
      });
    }
    // Run as administrator (Windows only: the game's frame rate needs it).
    // On restarts the meter elevated; off counts from the next start.
    if (this.runAsAdminCheckbox && /Windows/i.test(navigator.userAgent || "")) {
      const box = this.runAsAdminCheckbox;
      box.closest(".runAsAdminRow")?.removeAttribute("hidden");
      box.checked = this.safeGetSetting("dpsMeter.runAsAdmin") === "true";
      box.addEventListener("change", async () => {
        const on = box.checked;
        box.disabled = true;
        try {
          const done = await window.javaBridge?.setRunAsAdmin?.(on);
          if (on && done === false) box.checked = false;
        } catch {
          box.checked = !on;
        } finally {
          box.disabled = false;
        }
      });
    }
    if (this.debugLoggingCheckbox) {
      this.debugLoggingCheckbox.checked = this.debugLoggingEnabled;
      this.debugLoggingCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.setDebugLogging(isChecked, { persist: true, syncBackend: true });
      });
    }
    // Plugins only: the meter window hidden, the backend keeps it so (and
    // tells every window when it changes, from the hotkey or a plugin bar).
    const showMeterCheckbox = document.querySelector(".showMeterCheckbox");
    const tauriInvoke = window.__TAURI__?.core?.invoke;
    if (showMeterCheckbox && tauriInvoke) {
      tauriInvoke("meter_shown").then((shown) => { showMeterCheckbox.checked = shown !== false; }).catch(() => {});
      window.__TAURI__?.event?.listen?.("meter-shown", (event) => { showMeterCheckbox.checked = event.payload !== false; });
      showMeterCheckbox.addEventListener("change", (event) => {
        tauriInvoke("set_meter_shown", { shown: !!event.target?.checked }).catch(() => {});
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
    this.showDpsSuffixCheckbox = document.querySelector(".showDpsSuffixCheckbox");
    if (this.showDpsSuffixCheckbox) {
      this.showDpsSuffixCheckbox.checked = this.showDpsSuffix;
      this.showDpsSuffixCheckbox.addEventListener("change", (event) => {
        this.showDpsSuffix = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.showDpsSuffix, String(this.showDpsSuffix));
        this.renderCurrentRows();
      });
    }
    // Keybinds on or off. Unset, on for Windows and off for Linux, where the
    // desktop registers them (the backend's platform::hotkeys::DEFAULT_ON).
    // The backend starts or stops them as the setting changes.
    this.globalHotkeysCheckbox = document.querySelector(".globalHotkeysCheckbox");
    if (this.globalHotkeysCheckbox) {
      const apply = (on) => {
        this.globalHotkeysCheckbox.checked = on;
        this.globalHotkeysCheckbox.closest(".settingsSection")?.classList.toggle("hotkeysOff", !on);
      };
      apply(this.globalHotkeysOn());
      const save = (on) => {
        apply(on);
        this.safeSetSetting(this.storageKeys.globalHotkeys, String(on));
      };
      // On Linux, turning them on first says that the desktop will register
      // them and may ask to approve them; nothing is registered until the
      // user goes ahead.
      const consent = document.querySelector(".hotkeyConsent");
      const closeConsent = () => {
        if (!consent) return;
        consent.hidden = true;
        this.globalHotkeysCheckbox.focus?.();
      };
      consent?.querySelector(".hotkeyConsentConfirm")?.addEventListener("click", () => {
        closeConsent();
        save(true);
      });
      consent?.querySelector(".hotkeyConsentCancel")?.addEventListener("click", closeConsent);
      consent?.addEventListener("keydown", (event) => {
        if (event.key !== "Escape") return;
        // Escape here closes the dialog, not the Settings window.
        event.stopPropagation();
        event.preventDefault();
        closeConsent();
      });
      this.globalHotkeysCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        const linux = document.documentElement?.classList?.contains?.("linux");
        if (isChecked && linux && consent) {
          apply(false);
          consent.hidden = false;
          consent.querySelector(".hotkeyConsentConfirm")?.focus?.();
          return;
        }
        save(isChecked);
      });
    }
    this.autoHideMeterCheckbox = document.querySelector(".autoHideMeterCheckbox");
    if (this.autoHideMeterCheckbox) {
      // Off unless turned on: the backend hides only on "true", so a box
      // ticked by default showed auto-hide on while it was off.
      const storedAutoHide = this.safeGetSetting(this.storageKeys.autoHideMeter) === "true";
      this.autoHideMeterCheckbox.checked = storedAutoHide;
      this.autoHideMeterCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.autoHideMeter, String(isChecked));
        window.javaBridge?.setAutoHideMeter?.(isChecked);
      });
    }
    // Hide the meter while AION2 is closed (the backend's tick loop reads it).
    this.hideWhenGameClosedCheckbox = document.querySelector(".hideWhenGameClosedCheckbox");
    if (this.hideWhenGameClosedCheckbox) {
      this.hideWhenGameClosedCheckbox.checked = this.safeGetSetting(this.storageKeys.hideWhenGameClosed) === "true";
      this.hideWhenGameClosedCheckbox.addEventListener("change", (event) => {
        this.safeSetSetting(this.storageKeys.hideWhenGameClosed, String(!!event.target?.checked));
      });
    }
    // And while not in a party (the same loop).
    this.hideWhenSoloCheckbox = document.querySelector(".hideWhenSoloCheckbox");
    if (this.hideWhenSoloCheckbox) {
      this.hideWhenSoloCheckbox.checked = this.safeGetSetting(this.storageKeys.hideWhenSolo) === "true";
      this.hideWhenSoloCheckbox.addEventListener("change", (event) => {
        this.safeSetSetting(this.storageKeys.hideWhenSolo, String(!!event.target?.checked));
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
      // Saved before the two were linked: packet logging on, debug off.
      if (storedSaveRaw && !this.debugLoggingEnabled) {
        this.setDebugLogging(true, { persist: true, syncBackend: true });
      }
      this.saveRawPacketsCheckbox.addEventListener("change", (event) => {
        const isChecked = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.saveRawPackets, String(isChecked));
        window.javaBridge?.setSaveRawPackets?.(isChecked);
        // Debug logging follows packet logging on and off.
        this.setDebugLogging(isChecked, { persist: true, syncBackend: true });
        this.syncLoggingLock();
      });
      this.syncLoggingLock();
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
    }
    Promise.resolve(window.javaBridge?.discordActivityAvailable?.())
      .catch(() => false)
      .then((ok) => {
        if (ok) for (const group of document.querySelectorAll(".discordActivityGroup")) group.style.display = "";
        if (window.A2_VIEW !== "main") return;
        if (window.a2Build?.online === false) return;   // nothing to offer
        // At most one offer per launch: the account one waits for a launch
        // where the Discord one does not open.
        // Sharing first (it decides whether anything uploads); otherwise at
        // most one other offer per launch.
        // Asked before this launch: not a new install (for What's new).
        const hadAsked = this.safeGetSetting(this.storageKeys.sharingAsked) === "true";
        this.maybeShowSharing().then(async (shown) => {
          const news = await this.maybeShowWhatsNew({ existingInstall: hadAsked, quiet: shown });
          if (!shown && !news && !(ok && this.maybeShowDiscordPromo())) this.maybeShowPartyPromo();
        });
      });

    this.initStreamOverlaySettings();

    // Uploading without an account (account/guest.rs): off unless turned on.
    if (this.guestUploadCheckbox) {
      this.guestUploadCheckbox.checked = this.safeGetSetting(this.storageKeys.guestUpload) === "true";
      this.guestUploadCheckbox.addEventListener("change", (event) => {
        const on = !!event.target?.checked;
        this.safeSetSetting(this.storageKeys.guestUpload, String(on));
        if (on) this.safeSetSetting(this.storageKeys.sharingAsked, "true");
      });
    }
    // Settings → Privacy: its links.
    document.querySelector(".privacyPolicyBtn")?.addEventListener("click", () => {
      window.javaBridge?.openBrowser?.("https://a2tools.app/privacy");
    });
    document.querySelector(".privacySignInBtn")?.addEventListener("click", () => {
      document.querySelector('.settingsNavItem[data-section="account"]')?.click();
    });
    // A code to claim this PC's guest fights from another device.
    this.guestClaimBtn?.addEventListener("click", async () => {
      this.guestClaimBtn.disabled = true;
      try {
        const c = await window.javaBridge?.guestClaimCode?.();
        if (c?.code && this.guestClaimText) {
          this.guestClaimText.textContent = window.i18n?.format?.(
            "settings.account.guestClaimCode", { code: c.code, url: c.url },
            `Enter ${c.code} at ${c.url} while signed in. The code expires in 15 minutes.`
          );
        }
      } catch (err) {
        if (this.guestClaimText) this.guestClaimText.textContent = typeof err === "string" ? err : err?.message || String(err);
      } finally {
        this.guestClaimBtn.disabled = false;
      }
    });

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
    // Applied in every window (the meter draws the bars); the slider is
    // Settings' only.
    const resolvedSaturation = this.normalizeMeterSaturation(this.safeGetSetting(this.storageKeys.meterFillSaturation));
    this.applyMeterFillSaturation(resolvedSaturation, { persist: false });
    if (this.meterSaturationInput && this.meterSaturationValue) {
      this.meterSaturationInput.value = String(resolvedSaturation);
      this.meterSaturationValue.textContent = `${resolvedSaturation}%`;
      const stopDrag = (event) => event.stopPropagation();
      this.meterSaturationInput.addEventListener("mousedown", stopDrag);
      this.meterSaturationInput.addEventListener("touchstart", stopDrag, { passive: true });
      this.meterSaturationInput.addEventListener("input", (event) => {
        const next = this.normalizeMeterSaturation(event.target?.value);
        this.meterSaturationValue.textContent = `${next}%`;
        this.applyMeterFillSaturation(next, { persist: true });
      });
    }

    const barGapInput = document.querySelector(".barGapInput");
    const barGapValue = document.querySelector(".barGapValue");
    if (barGapInput && barGapValue) {
      this.barGapInput = barGapInput;
      this.barGapValue = barGapValue;
      const shown = this.normalizeBarGap(this.safeGetSetting(this.storageKeys.barGap)) ?? this.defaultBarGap();
      barGapInput.value = String(shown);
      barGapValue.textContent = `${shown}px`;
      const stopDrag = (event) => event.stopPropagation();
      barGapInput.addEventListener("mousedown", stopDrag);
      barGapInput.addEventListener("touchstart", stopDrag, { passive: true });
      barGapInput.addEventListener("input", (event) => {
        const next = this.applyBarGap(event.target?.value, { persist: true });
        if (next !== null) barGapValue.textContent = `${next}px`;
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

    // Title bar opacity: its own once moved, else the background's.
    this.watchTitleHeight();
    if (this.titleOpacitySet()) this.applyTitleOpacity(this.safeGetStorage(this.storageKeys.titleOpacity), { persist: false });
    if (this.titleOpacityInput && this.titleOpacityValue) {
      const shown = this.titleOpacitySet()
        ? Math.max(0, Math.min(100, Math.round(Number(this.safeGetStorage(this.storageKeys.titleOpacity)))))
        : Number(this.windowOpacityInput?.value ?? 40);
      this.titleOpacityInput.value = String(shown);
      this.titleOpacityValue.textContent = `${shown}%`;
      const stopDrag = (event) => event.stopPropagation();
      this.titleOpacityInput.addEventListener("mousedown", stopDrag);
      this.titleOpacityInput.addEventListener("touchstart", stopDrag, { passive: true });
      this.titleOpacityInput.addEventListener("input", (event) => {
        const next = Math.max(0, Math.min(100, Math.round(Number(event.target?.value))));
        this.titleOpacityValue.textContent = `${next}%`;
        this.applyTitleOpacity(next, { persist: true });
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
      if (this.detailsMonitor !== "off" && window.A2_VIEW === "main") {
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

    this.initSettingsSections();

    this.resetDetectBtn?.addEventListener("click", () => {
      window.javaBridge?.resetAutoDetection?.();
      this.refreshConnectionInfo();
    });

    // Device selection: auto-detect checkbox + manual device dropdown. Both
    // are saved (#39): unticking Auto-detect, and the device picked, were held
    // nowhere, so reopening Settings ticked it again and lost the choice.
    this._autoDetectDevice = this.readAutoDetectDevice();
    if (this.autoDetectDeviceCheckbox) {
      this.autoDetectDeviceCheckbox.checked = this._autoDetectDevice;
      this._updateDeviceDropdownState();
      this.autoDetectDeviceCheckbox.addEventListener("change", () => {
        this._autoDetectDevice = this.autoDetectDeviceCheckbox.checked;
        this.safeSetSetting("dpsMeter.autoDetectDevice", String(this._autoDetectDevice));
        this._updateDeviceDropdownState();
        if (this._autoDetectDevice) {
          this.safeSetSetting("dpsMeter.manualDevice", "");
          window.javaBridge?.setManualDevice?.("");
          this.refreshConnectionInfo();
        } else {
          const saved = this.safeGetSetting("dpsMeter.manualDevice");
          if (saved) window.javaBridge?.setManualDevice?.(saved);
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
    this.supportQrImage?.addEventListener("click", () => {
      if (this.supportQrUrl) window.javaBridge?.openBrowser?.(this.supportQrUrl);
    });

    this.kofiButton?.addEventListener("click", () => {
      window.javaBridge?.openBrowser?.("https://ko-fi.com/W7W51T1YW9");
    });

    // This version's release notes; the list when the version isn't known.
    // Check for updates now: one found is offered (and installed: on Linux
    // through the package manager, after which the meter starts again).
    const updateBtn = document.querySelector(".settingsUpdateBtn");
    const updateStatus = document.querySelector(".settingsUpdateStatus");
    Promise.resolve(window.a2BuildReady).then((build) => {
      if (updateBtn && build?.online !== false && window.ReleaseChecker?.checkNow) updateBtn.hidden = false;
    });
    updateBtn?.addEventListener("click", async () => {
      if (updateBtn.classList.contains("isChecking")) return;
      const t = (key, fallback) => this.i18n?.t?.(key, fallback) ?? fallback;
      const say = (text) => {
        if (!updateStatus) return;
        updateStatus.textContent = text;
        updateStatus.hidden = !text;
      };
      updateBtn.classList.add("isChecking");
      say(t("settings.update.checking", "Checking…"));
      const found = await window.ReleaseChecker.checkNow();
      updateBtn.classList.remove("isChecking");
      const latest = String(found?.latest || "").replace(/^v/i, "");
      if (found?.state === "current") say(t("settings.update.current", "Up to date"));
      else if (found?.state === "installing") say(t("settings.update.installing", "Updating…"));
      else if (found?.state === "update") say(t("settings.update.available", "{version} is out").replace("{version}", latest));
      else say(t("settings.update.failed", "Couldn't check. Try again later."));
    });

    this.settingsVersionLink?.addEventListener("click", () => {
      const releases = "https://github.com/taengu/A2Tools-DPS-Meter/releases";
      const version = /^v\d+\.\d+\.\d+$/.exec(this.settingsVersionValue?.textContent?.trim() || "")?.[0];
      window.javaBridge?.openBrowser?.(version ? `${releases}/tag/${version}` : releases);
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
      const externalOnly = supportType === "paypal" || supportType === "afdian" || supportType === "kofi" || supportType === "patreon";
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
    // Ko-fi's (or Afdian's) code opens its page when clicked; WeChat's has
    // no page to open.
    const url = this.supportPrimaryButton?.dataset.support === type ? this.supportPrimaryButton.dataset.url : "";
    this.supportQrUrl = url || "";
    this.supportQrImage.classList.toggle("isLink", !!url);
    if (url) this.supportQrImage.title = url;
    else this.supportQrImage.removeAttribute("title");
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
    // Each theme's colours, read by putting the whole page in that theme for
    // a moment. Only Settings shows the theme list: every other window (a
    // fight's Details too) ran this for every theme on opening, restyling the
    // page and restarting its colour transitions each time, so it repainted
    // and ignored the mouse for seconds (2026-10-18). Elsewhere the list is
    // never seen, and its fixed previews are enough.
    const themesShown = !window.__A2_VIEW__ || window.__A2_VIEW__ === "main" || window.__A2_VIEW__ === "settings";
    const previewThemeVars = (themeId) => {
      this._themePreviewCache ||= new Map();
      if (this._themePreviewCache.has(themeId)) return this._themePreviewCache.get(themeId);
      if (!themesShown) return { textColor: "", nameShadow: "", rowFill: "" };
      const root = document.documentElement;
      const previous = root.dataset.theme;
      // No transitions while the page is in another theme for a moment.
      root.classList.add("readingThemes");
      root.dataset.theme = themeId;
      const computed = getComputedStyle(root);
      const textColor = computed.getPropertyValue("--text-color").trim() || "#ffffff";
      const nameShadow = computed.getPropertyValue("--player-name-shadow").trim() || "none";
      const rowFill = computed.getPropertyValue("--row-fill").trim() || "#2f2f2f";
      root.dataset.theme = previous || "aion2";
      getComputedStyle(root).color; // settle the page back before transitions return
      root.classList.remove("readingThemes");
      const preview = { textColor, nameShadow, rowFill };
      this._themePreviewCache.set(themeId, preview);
      return preview;
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
        if (!wasOpen) fitMenu(menu);
      };
    };

    // An open menu is placed over everything, the Appearance preview
    // included, rather than inside the scrolling pane, which cut it off (a
    // long one, the colour themes, had to end at the preview and scroll).
    // Fixed, under its button, or above it when there is more room there;
    // it scrolls only past the window's edge. The rects are in screen pixels
    // and the panel is zoomed, so they are scaled back to the menu's own.
    const fitMenu = (menu) => {
      for (const key of ["position", "top", "left", "maxHeight", "overflowY", "minWidth"]) menu.style[key] = "";
      const button = menu.parentElement?.querySelector(".settingsDropdownBtn");
      if (!button || !menu.offsetHeight || !button.offsetHeight) return;
      const r = button.getBoundingClientRect();
      const scale = r.height / button.offsetHeight || 1;
      const below = (window.innerHeight - r.bottom - 8) / scale;
      const above = (r.top - 8) / scale;
      menu.style.position = "fixed";
      menu.style.minWidth = `${r.width / scale}px`;
      const up = menu.offsetHeight > below && above > below;
      const room = up ? above : below;
      if (menu.offsetHeight > room) {
        menu.style.maxHeight = `${Math.max(120, Math.floor(room))}px`;
        menu.style.overflowY = "auto";
      }
      const top = up ? r.top / scale - 4 - menu.offsetHeight : r.bottom / scale + 4;
      menu.style.top = `${Math.max(4, top)}px`;
      menu.style.left = `${Math.max(4, r.right / scale - menu.offsetWidth)}px`;
    };

    if (!this._settingsDropdownOutsideBound) {
      document.addEventListener("click", (event) => {
        if (!event.target?.closest?.(".settingsDropdownWrapper")) {
          closeAll();
        }
      });
      // A placed menu stays where it opened: scrolling the pane under it, or
      // resizing the window, closes it (scrolling the menu itself does not).
      document.addEventListener("scroll", (event) => {
        if (!event.target?.closest?.(".settingsDropdownMenu")) closeAll();
      }, true);
      window.addEventListener("resize", () => closeAll());
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
      {
        value: "aion2-classes",
        label: this.i18n?.t("settings.theme.options.aion2Classes", "AION2 (class colours)"),
      },
      {
        value: "aion2-classes-vivid",
        label: this.i18n?.t("settings.theme.options.aion2ClassesVivid", "AION2 (class colours, vivid)"),
      },
      { value: "aion2-spectrum", label: this.i18n?.t("settings.theme.options.aion2Spectrum", "Spectrum") },
      { value: "aion2-aurora", label: this.i18n?.t("settings.theme.options.aion2Aurora", "Aurora") },
      { value: "aion2-sunset", label: this.i18n?.t("settings.theme.options.aion2Sunset", "Sunset") },
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
    // The vivid class colours straight after the regular ones, whatever the sort.
    const vividAt = themeOptions.findIndex((o) => o.value === "aion2-classes-vivid");
    if (vividAt >= 0) {
      const [vivid] = themeOptions.splice(vividAt, 1);
      themeOptions.splice(themeOptions.findIndex((o) => o.value === "aion2-classes") + 1, 0, vivid);
    }

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

    const barTextureOptions = SETTING_CHOICES.barTexture.allowed.map((value) => ({
      value,
      label: this.i18n?.t(`settings.barTexture.options.${value}`, value[0].toUpperCase() + value.slice(1)),
    }));
    // By name in the reader's language, None first.
    barTextureOptions.sort((a, b) =>
      a.value === "none" ? -1 : b.value === "none" ? 1 : a.label.localeCompare(b.label));

    const partyStatOptions = [
      { value: "combatPower", label: this.i18n?.t("settings.partyStat.options.combatPower", "Combat power") },
      { value: "gearScore", label: this.i18n?.t("settings.partyStat.options.gearScore", "Gear score") },
      { value: "both", label: this.i18n?.t("settings.partyStat.options.both", "Both") },
      { value: "off", label: this.i18n?.t("settings.partyStat.options.off", "Off") },
    ];
    const discordStatOptions = partyStatOptions.filter((opt) => SETTING_CHOICES.discordStat.allowed.includes(opt.value));

    const trainModeOptions = [
      { value: "all", label: this.i18n?.t("settings.trainingMode.options.all", "All") },
      {
        value: "highestDamage",
        label: this.i18n?.t("settings.trainingMode.options.highestDamage", "Highest Damage"),
      },
    ];
    const meterAnchorOptions = [
      { value: "top", label: this.i18n?.t("settings.meterAnchor.options.top", "Top") },
      { value: "middle", label: this.i18n?.t("settings.meterAnchor.options.middle", "Centre") },
      { value: "bottom", label: this.i18n?.t("settings.meterAnchor.options.bottom", "Bottom") },
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

    const settingsThemeOptions = SETTING_CHOICES.settingsTheme.allowed.map((value) => ({
      value,
      label: this.i18n?.t(`settings.settingsTheme.options.${value}`, value[0].toUpperCase() + value.slice(1)),
    }));
    setupDropdown(
      this.settingsThemeDropdownBtn,
      this.settingsThemeDropdownMenu,
      settingsThemeOptions,
      this.settingsSelections.settingsTheme,
      (value) => {
        if (!value) return;
        this.applySettingsTheme(value, { persist: true });
      },
      {
        decorateItem: (item, value) => {
          const swatch = document.createElement("span");
          swatch.className = "settingsThemeSwatch";
          swatch.dataset.settingsTheme = value;
          swatch.setAttribute("aria-hidden", "true");
          item.prepend(swatch);
        },
      }
    );

    // Each font's name in the font itself, so the menu shows how it reads.
    const FONT_PREVIEW = {
      meter: '"Pretendard", "A2 Pretendard", "Segoe UI", sans-serif',
      system: 'system-ui, "Segoe UI", "Noto Sans", sans-serif',
      inter: '"A2 Inter", sans-serif',
      exo2: '"A2 Exo 2", sans-serif',
      oxanium: '"A2 Oxanium", sans-serif',
      jetbrains: '"A2 JetBrains Mono", monospace',
      atkinson: '"A2 Atkinson", sans-serif',
      lexend: '"A2 Lexend", sans-serif',
      andika: '"A2 Andika", sans-serif',
    };
    const fontOptions = SETTING_CHOICES.font.allowed.map((value) => ({
      value,
      label: this.i18n?.t(`settings.font.options.${value}`, value[0].toUpperCase() + value.slice(1)),
    }));
    setupDropdown(
      this.fontDropdownBtn,
      this.fontDropdownMenu,
      fontOptions,
      this.settingsSelections.font,
      (value) => {
        if (!value) return;
        this.applyFont(value, { persist: true });
      },
      {
        decorateItem: (item, value) => {
          item.style.fontFamily = FONT_PREVIEW[value] || "";
        },
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
          item.style.background = THEME_PREVIEWS[value] || colors.rowFill;
          item.style.opacity = "1";
          item.style.color = colors.textColor;
          item.style.textShadow = colors.nameShadow;
        },
        decorateButton: (button, value) => {
          const colors = previewThemeVars(value);
          button.style.background = THEME_PREVIEWS[value] || colors.rowFill;
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
      this.updateRateDropdownBtn,
      this.updateRateDropdownMenu,
      [
        { value: "100", label: this.i18n?.t("settings.updateRate.options.100", "0.1 s") },
        { value: "200", label: this.i18n?.t("settings.updateRate.options.200", "0.2 s (default)") },
        { value: "250", label: this.i18n?.t("settings.updateRate.options.250", "0.25 s") },
        { value: "500", label: this.i18n?.t("settings.updateRate.options.500", "0.5 s") },
      ],
      this.settingsSelections.updateRateMs,
      (value) => {
        if (!value) return;
        this.settingsSelections.updateRateMs = value;
        this.applyUpdateRate(value);
        // The backend reads it from the settings on its next update.
        this.safeSetSetting(this.storageKeys.updateRateMs, value);
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
      this.clearOnZoneDropdownBtn,
      this.clearOnZoneDropdownMenu,
      [
        { value: "instance", label: this.i18n?.t("settings.clearOnZone.options.instance", "Entering or leaving a dungeon") },
        { value: "any", label: this.i18n?.t("settings.clearOnZone.options.any", "Any zone change") },
        { value: "never", label: this.i18n?.t("settings.clearOnZone.options.never", "Never (keep the last fight)") },
      ],
      this.settingsSelections.clearOnZone,
      (value) => {
        if (!value) return;
        this.settingsSelections.clearOnZone = value;
        this.safeSetSetting(this.storageKeys.clearOnZone, value);
      }
    );

    setupDropdown(
      this.hideOutOfCombatDropdownBtn,
      this.hideOutOfCombatDropdownMenu,
      SETTING_CHOICES.hideOutOfCombat.allowed.map((value) => ({
        value,
        label: value === "0"
          ? this.i18n?.t("settings.hideOutOfCombat.off", "Off") ?? "Off"
          : this.i18n?.format("settings.hideOutOfCombat.seconds", { seconds: value }, `After ${value} s`) ?? `After ${value} s`,
      })),
      this.settingsSelections.hideOutOfCombat,
      (value) => {
        if (!value) return;
        this.settingsSelections.hideOutOfCombat = value;
        this.safeSetSetting(this.storageKeys.hideOutOfCombat, value);
      }
    );

    setupDropdown(
      this.discordStatDropdownBtn,
      this.discordStatDropdownMenu,
      discordStatOptions,
      this.settingsSelections.discordStat,
      (value) => {
        if (!value) return;
        this.settingsSelections.discordStat = value;
        this.safeSetSetting(this.storageKeys.discordStat, value);
      }
    );

    setupDropdown(
      this.barTextureDropdownBtn,
      this.barTextureDropdownMenu,
      barTextureOptions,
      this.settingsSelections.barTexture,
      (value) => {
        if (!value || !this.isSupporter) return;
        this.settingsSelections.barTexture = value;
        this.applyBarTexture(value);
        this.safeSetSetting(this.storageKeys.barTexture, value);
      },
      {
        // Each row, and the button, drawn as a bar with its texture (styles.css).
        decorateItem: (item, value) => {
          item.dataset.texture = value;
        },
        decorateButton: (button, value) => {
          button.dataset.texture = value;
        },
      }
    );
    this.syncBarTextureControl();
    if (this.textureOffsetCheckbox) {
      this.textureOffsetCheckbox.checked = this.textureOffset;
      this.textureOffsetCheckbox.addEventListener("change", (event) => {
        this.setTextureOffset(!!event.target?.checked);
        this.safeSetSetting(this.storageKeys.barTextureOffset, String(this.textureOffset));
      });
    }

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
    setupDropdown(
      this.meterAnchorDropdownBtn,
      this.meterAnchorDropdownMenu,
      meterAnchorOptions,
      this.settingsSelections.meterAnchor,
      (value) => {
        if (!value) return;
        this.settingsSelections.meterAnchor = value;
        this.safeSetSetting(this.storageKeys.meterAnchor, value);
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

    // Standard or Slim. The Classic skins are retired; the values keep their
    // old names ("beta", "betaSlim") so a saved choice still reads the same.
    const meterLayoutOptions = [
      { value: "beta", label: this.i18n?.t?.("settings.meterLayout.standard", "Standard") ?? "Standard" },
      { value: "betaSlim", label: this.i18n?.t?.("settings.meterLayout.slim", "Slim") ?? "Slim" },
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
    // Columns added after the first release: hidden until switched on, for new
    // and upgrading users alike (a column is "seen" once its toggle is used).
    const NEW_COLUMNS_DEFAULT_HIDDEN = ["regen", "block", "perfectblock", "ironwall", "regeneration", "miss", "resist"];
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
      const columns = ["hit", "dmg", "dmgpct", "mhit", "mdmg", "crit", "parry", "perfect", "double", "back", "block", "perfectblock", "ironwall", "regeneration", "miss", "resist", "regen", "mindmg", "avgdmg", "maxdmg"];
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
    // The web viewer (a2tools.app) sets a smaller default for compact rows.
    const fallback = Number(globalThis.__A2_DETAILS_ICON_SIZE__) || 36;
    if (value === null || value === undefined || value === "") {
      return fallback;
    }
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) {
      return fallback;
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
    const pluginsKeyBtn = document.querySelector(".pluginsKeybindBtn");
    const notSet = () => this.i18n?.t?.("settings.keybind.notSet", "Not set") ?? "Not set";

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
      if (pluginsKeyBtn) {
        const label = window.javaBridge?.getCurrentPluginWindowsHotKey?.() || "";
        pluginsKeyBtn.querySelector(".keybindText").textContent = label || notSet();
        pluginsKeyBtn.classList.toggle("isUnset", !label);
      }
    };
    this.refreshKeybindLabels();

    // A key another program holds, or one two of the meter's actions share,
    // is framed in red, with the reason in a line under its label (a hover
    // alone goes unread). The backend says which keys it could not register
    // ("taken"); sharing is read off the labels.
    let lastHotkeyStatus = {};
    this.applyHotkeyStatus = (status) => {
      lastHotkeyStatus = status && typeof status === "object" ? status : {};
      const rows = [[reloadBtn, "reload"], [toggleBtn, "toggle"], [lockKeyBtn, "lock"], [pluginsKeyBtn, "plugins"]]
        .filter(([btn]) => btn && btn.closest?.(".settingsRowKeybind")?.style?.display !== "none")
        .filter(([btn]) => !btn.classList?.contains?.("isUnset"));
      const labelOf = (btn) => (btn.querySelector?.(".keybindText")?.textContent || "").trim().toLowerCase();
      for (const [btn, key] of rows) {
        const shared = rows.some(([other]) => other !== btn && labelOf(other) === labelOf(btn));
        const taken = lastHotkeyStatus[key] === "taken";
        const unset = !shared && lastHotkeyStatus[key] === "unset";
        // The key the desktop gave it, when that differs from ours (KDE
        // keeps the one it stored first).
        const assigned = String(lastHotkeyStatus[`${key}.assigned`] || "").trim();
        const squash = (s) => s.toLowerCase().replace(/\s+/g, "");
        const otherKey = !shared && !taken && assigned && squash(assigned) !== squash(labelOf(btn));
        btn.classList?.toggle?.("isTaken", shared || taken);
        btn.classList?.toggle?.("isUnsetByDesktop", unset);
        let reason = "";
        if (shared) {
          reason = this.i18n?.t?.("settings.keybindShared", "Another action uses these keys. Pick different keys.")
            ?? "Another action uses these keys. Pick different keys.";
        } else if (taken) {
          reason = this.i18n?.t?.("settings.keybindTaken", "Another program already uses these keys. Pick different keys.")
            ?? "Another program already uses these keys. Pick different keys.";
        } else if (unset) {
          reason = this.i18n?.t?.("settings.keybindUnset", "Your desktop hasn't given this a key yet. Set it in Desktop shortcuts.")
            ?? "Your desktop hasn't given this a key yet. Set it in Desktop shortcuts.";
        } else if (otherKey) {
          reason = (this.i18n?.t?.("settings.keybindAssigned", "Your desktop uses {key}. Change it in Desktop shortcuts.")
            ?? "Your desktop uses {key}. Change it in Desktop shortcuts.").replace("{key}", assigned);
        }
        const info = btn.closest?.(".settingsRowKeybind")?.querySelector?.(".settingsInfo");
        let hint = info?.querySelector?.(".keybindHint");
        if (reason && info && !hint && document.createElement) {
          hint = document.createElement("div");
          hint.className = "keybindHint";
          hint.setAttribute?.("role", "status");
          info.appendChild?.(hint);
        }
        if (hint) {
          hint.textContent = reason;
          hint.hidden = !reason;
          hint.classList?.toggle?.("isNote", unset || otherKey);
        }
        if (reason) btn.title = reason;
        else btn.removeAttribute?.("title");
      }
      // Where the desktop decides the keys (Linux), a way to its settings.
      const desktopBtn = document.querySelector?.(".keybindDesktopBtn");
      if (desktopBtn) {
        desktopBtn.hidden = !Object.entries(lastHotkeyStatus)
          .some(([k, v]) => v === "unset" || (k.endsWith(".assigned") && v));
      }
    };
    const refreshLabels = this.refreshKeybindLabels;
    this.refreshKeybindLabels = () => {
      refreshLabels();
      this.applyHotkeyStatus(lastHotkeyStatus);
    };
    Promise.resolve(window.javaBridge?.getHotkeyStatus?.())
      .then((status) => this.applyHotkeyStatus(status || {}))
      .catch(() => {});

    let activeRecording = null;

    const stopRecording = () => {
      if (!activeRecording) return;
      activeRecording.btn.classList.remove("recording");
      activeRecording = null;
    };
    if (window.A2_VIEW === "settings") window.addEventListener("settings-hidden", () => {
      if (!activeRecording) return;
      stopRecording();
      this.refreshKeybindLabels();
    });

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
    pluginsKeyBtn?.addEventListener("click", () => handleKeybindClick(pluginsKeyBtn, "plugins"));
    document.querySelector(".keybindDesktopBtn")?.addEventListener("click", () => {
      Promise.resolve(window.javaBridge?.openDesktopShortcuts?.()).catch(() => {});
    });
    document.querySelector(".keybindDefaultsBtn")?.addEventListener("click", () => {
      stopRecording();
      window.javaBridge?.restoreDefaultHotkeys?.();
      this.refreshKeybindLabels();
    });

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

      // 1 to 3 keys: a key alone (F9), or with one or two modifiers.
      if (modCount > 2) {
        activeRecording.btn.querySelector(".keybindText").textContent =
          this.i18n?.t?.("settings.keybind.tooManyKeys", "Up to 3 keys") ?? "Up to 3 keys";
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
      } else if (type === "plugins") {
        window.javaBridge?.setPluginWindowsHotkey?.(mods, vk);
      }
      this.refreshKeybindLabels();
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
    this.updateOdyle();
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

  // A packet log is read beside the debug log of the same session, so packet
  // logging keeps debug logging on: its switch is locked while packets are
  // logged. Debug logging can still be on by itself.
  syncLoggingLock() {
    const box = this.debugLoggingCheckbox;
    if (!box) return;
    const locked = !!this.saveRawPacketsCheckbox?.checked;
    if (locked && !box.checked) box.checked = true;
    box.disabled = locked;
    const row = box.closest?.(".settingsToggle");
    if (!row) return;
    row.classList.toggle("isLocked", locked);
    if (locked) {
      row.title = this.i18n?.t?.("settings.debugLoggingLocked",
        "On while packet logging is on: packet logs need the debug log to be read.")
        ?? "On while packet logging is on: packet logs need the debug log to be read.";
    } else {
      row.removeAttribute("title");
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
    // setupSettingsPanel already requested monitors; reuse that result.
    this._loadDeviceDropdown();
    this.settingsPanel?.classList.add("isOpen");
    // Closing is wired in startApp, before anything that can be slow.
    requestAnimationFrame(() => {
      requestAnimationFrame(() => window.javaBridge?.toolWindowReady?.("settings"));
    });
  }

  // Settings has no title poll; the bridge calls this after each status read,
  // which happens only while the window is shown.
  refreshSettingsStatus() {
    const title = window.javaBridge?.getAion2WindowTitle?.();
    this.aionRunning = typeof title === "string" && title.trim().length > 0;
    if (this.aionRunning) this.syncCharacterNameFromGame();
    this.refreshConnectionInfo();
  }

  // A reused Settings window is shown again without start(). Re-read the form
  // from stored settings; the controls keep the handlers setupSettingsPanel wired.
  syncSettingsForm() {
    this.refreshStreamOverlaySettings();
    const get = (name) => this.safeGetSetting(this.storageKeys[name]);
    const isSet = (value) => value !== null && value !== undefined && String(value).trim() !== "";
    const storedName = this.safeGetStorage(this.storageKeys.userName);
    if (storedName !== null) this.applyRemoteSettingChange(this.storageKeys.userName, storedName);
    for (const key of Object.keys(REMOTE_APPLIED_SETTING_CONTROLS)) {
      const value = this.safeGetSetting(key);
      if (isSet(value)) this.applyRemoteSettingChange(key, value);
    }
    for (const [box, name, defaultOn] of [
      [this.bossLogsCheckbox, "bossLogs", false],
      [this.autoHideMeterCheckbox, "autoHideMeter", false],
      [this.hideWhenGameClosedCheckbox, "hideWhenGameClosed", false],
      [this.hideWhenSoloCheckbox, "hideWhenSolo", false],
      [this.globalHotkeysCheckbox, "globalHotkeys", !document.documentElement?.classList?.contains?.("linux")],
      [this.saveRawPacketsCheckbox, "saveRawPackets", false],
      [document.querySelector(".discordActivityCheckbox"), "discordActivity", false],
    ]) {
      if (box) box.checked = defaultOn ? get(name) !== "false" : get(name) === "true";
    }
    this.setDebugLogging(get("debugLogging") === "true");
    this.syncLoggingLock();
    // Refilling a form applies saved values without replaying input handlers
    // that would persist them again. A focused slider keeps its draft.
    for (const [name, value] of [
      ["meterFillOpacity", get("meterFillOpacity")],
      ["meterFillSaturation", get("meterFillSaturation")],
      ["windowOpacity", get("windowOpacity")],
      ["titleOpacity", get("titleOpacity")],
      ["barGap", get("barGap")],
    ]) {
      if (isSet(value)) this.applyRemoteSettingChange(this.storageKeys[name], value);
    }

    const limit = get("playerLimit");
    if (isSet(limit)) this.setPlayerLimit(limit);

    this.setBetaUi(true);
    this.setSlimMode(get("slimMode") === "true");
    this.applyTheme(get("theme") || this.theme);
    this.applyFont(get("font"));
    const language = get("language");
    if (isSet(language)) this.applyRemoteSettingChange(this.storageKeys.language, language);
    this.settingsSelections.language = this.i18n?.getLanguage?.() || this.settingsSelections.language;
    for (const name of Object.keys(SETTING_CHOICES)) {
      this.settingsSelections[name] = pickSettingChoice(name, get(name));
    }
    this.trainSelectionMode = this.settingsSelections.trainSelectionMode;
    this.detailsMonitor = this.getDetailsMonitorSetting();

    this._autoDetectDevice = this.readAutoDetectDevice();
    if (this.autoDetectDeviceCheckbox) this.autoDetectDeviceCheckbox.checked = this._autoDetectDevice;
    this._updateDeviceDropdownState();
    this._loadDeviceDropdown();

    this.initializeSettingsDropdowns();
    // Displays may have been plugged in or removed while the window was hidden.
    return this.refreshMonitorList().then(() => this.initializeSettingsDropdowns());
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
    // "classic"/"classicSlim" may still arrive from an old setting: Standard/Slim.
    const slim = String(value).endsWith("Slim");
    this.setBetaUi(true, { persist });
    this.setSlimMode(slim, { persist });
  }

  // The redesigned main window is the only skin since the Classic ones were
  // retired; dpsMeter.betaUi stays "true" (a saved "false" is moved over at
  // start, see the settings load).
  setBetaUi(enabled, { persist = false } = {}) {
    this.betaUi = true;
    this.fitBossName();
    this.syncSettingsDropdownSelection(this.meterLayoutDropdownBtn, this.meterLayoutDropdownMenu, this.getMeterLayout());
    if (window.A2_VIEW === "main") window.javaBridge?.updateOverlaySize?.();
    if (persist) {
      this.safeSetSetting(this.storageKeys.betaUi, String(this.betaUi));
    }
  }

  setSlimMode(enabled, { persist = false } = {}) {
    this.slimMode = !!enabled;
    document.querySelector(".meter")?.classList.toggle("slim", this.slimMode);
    document.querySelector(".barPreviewPanel")?.classList.toggle("slim", this.slimMode);
    this.fitBossName();
    this.syncSettingsDropdownSelection(this.meterLayoutDropdownBtn, this.meterLayoutDropdownMenu, this.getMeterLayout());
    if (window.A2_VIEW === "main") window.javaBridge?.updateOverlaySize?.();
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
    // Leaving Boss mode drops "No boss yet" at once, rather than on
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
    // The "meter" Settings colours take this theme's accent.
    this.sharePluginTheme();
    if (this.settingsSelections) {
      this.settingsSelections.theme = normalized;
    }
    const selected = this.syncSettingsDropdownSelection(this.themeDropdownBtn, this.themeDropdownMenu, normalized);
    if (selected && this.themeDropdownBtn) {
      for (const property of ["background", "color", "textShadow"]) {
        this.themeDropdownBtn.style[property] = selected.style[property];
      }
      const text = this.themeDropdownBtn.querySelector(".settingsDropdownText");
      if (text) text.style.textShadow = selected.style.textShadow;
    }
    if (persist) {
      this.safeSetSetting(this.storageKeys.theme, normalized);
    }
  }

  // The Settings window's colours, apart from the meter's theme.
  // The font of every meter window (#98): data-font on the page, which
  // styles.css turns into the font; "meter" keeps the meter's own.
  applyFont(value, { persist = false } = {}) {
    const normalized = pickSettingChoice("font", value);
    const root = document.documentElement;
    if (root?.dataset) root.dataset.font = normalized;
    if (this.settingsSelections) this.settingsSelections.font = normalized;
    this.syncSettingsDropdownSelection(this.fontDropdownBtn, this.fontDropdownMenu, normalized);
    if (persist) this.safeSetSetting(this.storageKeys.font, normalized);
  }

  applySettingsTheme(value, { persist = false } = {}) {
    const normalized = pickSettingChoice("settingsTheme", value);
    document.documentElement.dataset.settingsTheme = normalized;
    if (this.settingsSelections) this.settingsSelections.settingsTheme = normalized;
    this.syncSettingsDropdownSelection(this.settingsThemeDropdownBtn, this.settingsThemeDropdownMenu, normalized);
    if (persist) this.safeSetSetting(this.storageKeys.settingsTheme, normalized);
    this.sharePluginTheme();
  }

  // Plugin windows take the Settings colours (their panel, title bar and
  // controls): read as this window shows them, as #rrggbb, and handed to the
  // meter, which keeps them and tells the plugins. Only the Settings window
  // has them; the meter skips a theme it already has.
  sharePluginTheme() {
    const root = document.documentElement;
    if (!root.classList.contains("settingsWindow")) return;
    clearTimeout(this._pluginThemeTimer);
    this._pluginThemeTimer = setTimeout(() => {
      const hex = (value) => {
        const probe = document.createElement("span");
        probe.style.color = value;
        if (!probe.style.color) return "";
        probe.style.display = "none";
        document.body.appendChild(probe);
        const rgba = getComputedStyle(probe).color.match(/[\d.]+/g) || [];
        probe.remove();
        if (rgba.length < 3 || (rgba.length > 3 && Number(rgba[3]) === 0)) return "";
        return `#${rgba.slice(0, 3).map((n) => Number(n).toString(16).padStart(2, "0")).join("")}`;
      };
      const css = getComputedStyle(root);
      const read = (name) => hex(css.getPropertyValue(name).trim());
      const bg = read("--settings-bg") || "#0a0e16";
      const theme = {
        bg,
        panel: read("--settings-nav-bg") || bg,
        accent: read("--settings-accent") || read("--isUser-fill") || "#8fc2ff",
      };
      window.__TAURI__?.core?.invoke?.("set_plugin_theme", { theme })?.catch?.(() => {});
    }, 0);
  }

  /** The display modes after DPS, in the menu's order. */
  displayModes() {
    return ["totalDamage", "damageReceived", "hps", "totalHeal", "stagger"];
  }

  setDisplayMode(mode, { persist = false } = {}) {
    const modes = ["dps", ...this.displayModes()];
    this.displayMode = modes.includes(mode) ? mode : "dps";
    // Extras in the menu's order, never the primary, never one turned off.
    const extras = new Set(this.extraMetrics || []);
    this.extraMetrics = modes.filter((m) => m !== this.displayMode && extras.has(m));
    if (persist) {
      this.safeSetStorage(this.storageKeys.displayMode, this.displayMode);
      this.safeSetStorage(this.storageKeys.extraMetrics, JSON.stringify(this.extraMetrics));
    }
    this.updateDisplayToggleLabel();
    this.renderMetricMenu();
  }

  metricLabels(mode) {
    return {
      totalDamage: ["header.display.total", "DMG", "header.display.ariaDamage", "Showing total damage", "header.display.names.totalDamage", "Total damage"],
      damageReceived: ["header.display.damageReceived", "TAKEN", "header.display.ariaDamageReceived", "Showing damage taken", "header.display.names.damageReceived", "Damage taken"],
      hps: ["header.display.hps", "HPS", "header.display.ariaHps", "Showing healing per second", "header.display.names.hps", "Healing per second"],
      totalHeal: ["header.display.totalHeal", "HEAL", "header.display.ariaHeal", "Showing total healing", "header.display.names.totalHeal", "Total healing"],
      stagger: ["header.display.stagger", "STAG", "header.display.ariaStagger", "Showing stagger dealt", "header.display.names.stagger", "Stagger"],
      dps: ["header.display.dps", "DPS", "header.display.ariaDps", "Showing DPS", "header.display.names.dps", "Damage per second"],
    }[mode];
  }

  updateDisplayToggleLabel() {
    if (!this.metricToggleBtn) return;
    const [labelKey, labelFallback, ariaKey, ariaFallback] = this.metricLabels(this.displayMode);
    const label = this.i18n?.t(labelKey, labelFallback) ?? labelFallback;
    const ariaLabel = this.i18n?.t(ariaKey, ariaFallback) ?? ariaFallback;
    const extras = this.extraMetrics?.length || 0;
    this.metricToggleBtn.textContent = extras ? `${label}+${extras}` : label;
    // In the metric's own colour, as its numbers on the rows are.
    this.metricToggleBtn.dataset.metric = this.displayMode;
    this.metricToggleBtn.setAttribute("aria-label", ariaLabel);
    this.metricToggleBtn.setAttribute("aria-expanded", String(!!this.metricMenuOpen));
    const tip = "Pick what the rows show. Wheel or right-click: next metric";
    this.metricToggleBtn.title = this.i18n?.t("header.display.menuTip", tip) ?? tip;
  }

  // The metric button opens a menu: tick a metric to add it as a column,
  // click its name to sort and draw the bars by it. The wheel or a right
  // click on the button steps the sorting metric, as the old click did.
  // Last in the menu, boss stagger % on the HP bar: on or off, never a sort.
  bindMetricMenu() {
    const btn = this.metricToggleBtn;
    const footer = document.querySelector(".meter .meterFooter");
    if (!btn || !footer) return;
    const menu = document.createElement("div");
    menu.className = "metricMenu";
    menu.setAttribute("role", "menu");
    menu.setAttribute("data-no-drag", "true");
    menu.hidden = true;
    // In the meter's flow, above the footer, so the window grows to fit it.
    footer.parentNode.insertBefore(menu, footer);
    this.metricMenuEl = menu;
    this.metricMenuOpen = false;

    const step = (dir) => {
      const order = ["dps", ...this.displayModes()];
      const i = order.indexOf(this.displayMode);
      this.setDisplayMode(order[(i + dir + order.length) % order.length], { persist: true });
      this.renderCurrentRows();
    };
    btn.addEventListener("click", () => this.setMetricMenuOpen(!this.metricMenuOpen));
    btn.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      step(1);
    });
    let lastWheel = 0;
    btn.addEventListener(
      "wheel",
      (event) => {
        event.preventDefault();
        // A touchpad sends a stream of small deltas: one step per gesture tick.
        const now = performance.now();
        if (now - lastWheel < 150 || event.deltaY === 0) return;
        lastWheel = now;
        step(event.deltaY > 0 ? 1 : -1);
      },
      { passive: false },
    );
    menu.addEventListener("click", (event) => {
      const item = event.target.closest?.("[data-metric]");
      if (!item) return;
      const mode = item.dataset.metric;
      if (mode === "bossStagger") {
        this.showBossStagger = !this.showBossStagger;
        this.safeSetStorage(this.storageKeys.showBossStagger, String(this.showBossStagger));
        this.renderMetricMenu();
        this.renderCurrentRows();
        return;
      }
      if (mode === "statColours") {
        this.statColours = !this.statColours;
        this.safeSetStorage(this.storageKeys.statColours, String(this.statColours));
        document.body.classList.toggle("noStatColours", !this.statColours);
        this.renderMetricMenu();
        return;
      }
      if (mode === "deaths") {
        this.showDeaths = !this.showDeaths;
        this.safeSetStorage(this.storageKeys.showDeaths, String(this.showDeaths));
        document.body.classList.toggle("noDeaths", !this.showDeaths);
        this.renderMetricMenu();
        return;
      }
      // Beside the names: combat power and gear score, each on or off (the
      // setting's four values: one, the other, both, neither).
      if (mode === "combatPower" || mode === "gearScore") {
        const on = { combatPower: false, gearScore: false };
        if (this.partyStat === "both") on.combatPower = on.gearScore = true;
        else if (this.partyStat in on) on[this.partyStat] = true;
        on[mode] = !on[mode];
        const value = on.combatPower && on.gearScore ? "both" : on.combatPower ? "combatPower" : on.gearScore ? "gearScore" : "off";
        this.settingsSelections.partyStat = value;
        this.applyPartyStat(value);
        this.safeSetSetting(this.storageKeys.partyStat, value);
        this.renderMetricMenu();
        this.renderCurrentRows();
        return;
      }
      if (event.target.closest(".metricMenuSort")) {
        if (mode === this.displayMode) return;
        // The old sort metric stays on as a column: the switch loses nothing.
        this.extraMetrics = [...this.extraMetrics, this.displayMode];
        this.setDisplayMode(mode, { persist: true });
      } else {
        // The sorting metric is always shown.
        if (mode === this.displayMode) return;
        const extras = new Set(this.extraMetrics);
        if (extras.has(mode)) extras.delete(mode);
        else extras.add(mode);
        this.extraMetrics = [...extras];
        this.setDisplayMode(this.displayMode, { persist: true });
      }
      this.renderCurrentRows();
    });
    document.addEventListener("pointerdown", (event) => {
      if (!this.metricMenuOpen) return;
      if (menu.contains(event.target) || btn.contains(event.target)) return;
      this.setMetricMenuOpen(false);
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && this.metricMenuOpen) this.setMetricMenuOpen(false);
    });
    window.addEventListener("blur", () => this.setMetricMenuOpen(false));
    this.renderMetricMenu();
  }

  setMetricMenuOpen(open) {
    if (!this.metricMenuEl || this.metricMenuOpen === open) return;
    if (open) {
      this.setTargetMenuOpen(false);
      this.setFooterMenuOpen?.(false);
    }
    this.metricMenuOpen = open;
    this.metricMenuEl.hidden = !open;
    this.metricToggleBtn?.classList.toggle("isOpen", open);
    this.metricToggleBtn?.setAttribute("aria-expanded", String(open));
    if (open) this.renderMetricMenu();
    // Grow (or shrink) the window for it now, not on the next poll.
    requestAnimationFrame(() => window.javaBridge?.updateOverlaySize?.());
  }

  // The target mode button opens a menu like the metric one: a radio per
  // mode. The wheel or a right click on the button steps the mode, as the
  // old click did.
  bindTargetMenu() {
    const btn = this.targetModeBtn;
    const footer = document.querySelector(".meter .meterFooter");
    if (!btn || !footer) return;
    const menu = document.createElement("div");
    menu.className = "metricMenu targetMenu";
    menu.setAttribute("role", "menu");
    menu.setAttribute("data-no-drag", "true");
    menu.hidden = true;
    footer.parentNode.insertBefore(menu, footer);
    this.targetMenuEl = menu;
    this.targetMenuOpen = false;

    const choose = (mode) => {
      this.setTargetSelection(mode, { persist: true, syncBackend: true, reason: "target menu" });
      this.renderTargetMenu();
      if (!this.isCollapse) this.fetchDps();
    };
    const step = (dir) => {
      const modes = TARGET_MENU_MODES.map(([mode]) => mode);
      const i = modes.indexOf(this.targetSelection);
      choose(modes[(i + dir + modes.length) % modes.length]);
    };
    btn.addEventListener("click", () => this.setTargetMenuOpen(!this.targetMenuOpen));
    btn.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      step(1);
    });
    let lastWheel = 0;
    btn.addEventListener(
      "wheel",
      (event) => {
        event.preventDefault();
        const now = performance.now();
        if (now - lastWheel < 150 || event.deltaY === 0) return;
        lastWheel = now;
        step(event.deltaY > 0 ? 1 : -1);
      },
      { passive: false },
    );
    menu.addEventListener("click", (event) => {
      const item = event.target.closest?.("[data-mode]");
      if (item && item.dataset.mode !== this.targetSelection) choose(item.dataset.mode);
    });
    document.addEventListener("pointerdown", (event) => {
      if (!this.targetMenuOpen) return;
      if (menu.contains(event.target) || btn.contains(event.target)) return;
      this.setTargetMenuOpen(false);
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && this.targetMenuOpen) this.setTargetMenuOpen(false);
    });
    window.addEventListener("blur", () => this.setTargetMenuOpen(false));
  }

  setTargetMenuOpen(open) {
    if (!this.targetMenuEl || this.targetMenuOpen === open) return;
    if (open) {
      this.setMetricMenuOpen(false);
      this.setFooterMenuOpen?.(false);
    }
    this.targetMenuOpen = open;
    this.targetMenuEl.hidden = !open;
    this.targetModeBtn?.classList.toggle("isOpen", open);
    this.targetModeBtn?.setAttribute("aria-expanded", String(open));
    if (open) this.renderTargetMenu();
    requestAnimationFrame(() => window.javaBridge?.updateOverlaySize?.());
  }

  renderTargetMenu() {
    const menu = this.targetMenuEl;
    if (!menu || !this.targetMenuOpen) return;
    const t = (key, fallback) => this.i18n?.t(key, fallback) ?? fallback;
    menu.replaceChildren();
    for (const [mode, label, descKey, descFallback] of TARGET_MENU_MODES) {
      const primary = mode === this.targetSelection;
      const el = document.createElement("div");
      el.className = "metricMenuItem";
      el.dataset.mode = mode;
      el.classList.toggle("isPrimary", primary);
      el.setAttribute("role", "menuitemradio");
      el.setAttribute("aria-checked", String(primary));
      const radio = document.createElement("span");
      radio.className = "metricMenuSort";
      const name = document.createElement("span");
      name.className = "metricMenuName";
      name.textContent = label;
      const desc = document.createElement("span");
      desc.className = "targetMenuDesc";
      desc.textContent = t(descKey, descFallback);
      el.append(radio, name, desc);
      menu.appendChild(el);
    }
  }

  // The footer's displays (battle timer, party total, Odyle energy) open a
  // menu of every footer display to turn on or off, ping included: these
  // were switches in Settings → Meter Settings.
  bindFooterMenu() {
    const area = document.querySelector(".meter .footerInfo");
    const footer = document.querySelector(".meter .meterFooter");
    if (!area || !footer) return;
    const menu = document.createElement("div");
    menu.className = "metricMenu footerMenu";
    menu.setAttribute("role", "menu");
    menu.setAttribute("data-no-drag", "true");
    menu.hidden = true;
    footer.parentNode.insertBefore(menu, footer);
    this.footerMenuEl = menu;
    this.footerMenuOpen = false;
    const t = (key, fallback) => this.i18n?.t(key, fallback) ?? fallback;
    area.title = t("footerMenu.tip", "Choose what the footer shows");
    area.addEventListener("click", () => this.setFooterMenuOpen(!this.footerMenuOpen));
    area.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      this.setFooterMenuOpen(!this.footerMenuOpen);
    });
    menu.addEventListener("click", (event) => {
      const item = event.target.closest?.("[data-display]");
      if (!item) return;
      const display = FOOTER_DISPLAYS.find(([key]) => key === item.dataset.display);
      if (!display) return;
      const [key] = display;
      this[key] = !this[key];
      this.safeSetSetting(this.storageKeys[key], String(this[key]));
      if (key === "showTtk" && !this.showTtk) this.battleTime?.setTtk?.(null);
      if (key === "showOdyle") this.updateOdyle();
      if (key === "showPing") this.updatePing();
      if (key === "showTotalDps") this.renderCurrentRows();
      this.renderFooterMenu();
    });
    document.addEventListener("pointerdown", (event) => {
      if (!this.footerMenuOpen) return;
      if (menu.contains(event.target) || area.contains(event.target)) return;
      this.setFooterMenuOpen(false);
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && this.footerMenuOpen) this.setFooterMenuOpen(false);
    });
    window.addEventListener("blur", () => this.setFooterMenuOpen(false));
  }

  setFooterMenuOpen(open) {
    if (!this.footerMenuEl || this.footerMenuOpen === open) return;
    if (open) {
      this.setMetricMenuOpen(false);
      this.setTargetMenuOpen(false);
    }
    this.footerMenuOpen = open;
    this.footerMenuEl.hidden = !open;
    document.querySelector(".meter .footerInfo")?.classList.toggle("isOpen", open);
    if (open) this.renderFooterMenu();
    requestAnimationFrame(() => window.javaBridge?.updateOverlaySize?.());
  }

  renderFooterMenu() {
    const menu = this.footerMenuEl;
    if (!menu || !this.footerMenuOpen) return;
    const t = (key, fallback) => this.i18n?.t(key, fallback) ?? fallback;
    menu.replaceChildren();
    for (const [key, labelKey, fallback] of FOOTER_DISPLAYS) {
      const shown = this[key] !== false;
      const el = document.createElement("div");
      el.className = "metricMenuItem";
      el.dataset.display = key;
      el.classList.toggle("isShown", shown);
      el.setAttribute("role", "menuitemcheckbox");
      el.setAttribute("aria-checked", String(shown));
      const name = document.createElement("span");
      name.className = "metricMenuName";
      name.textContent = t(labelKey, fallback);
      const check = document.createElement("span");
      check.className = "metricMenuCheck";
      check.setAttribute("aria-hidden", "true");
      el.append(name, check);
      menu.appendChild(el);
    }
  }

  renderMetricMenu() {
    const menu = this.metricMenuEl;
    if (!menu) return;
    const t = (key, fallback) => this.i18n?.t(key, fallback) ?? fallback;
    const cell = (tag, className, text = "") => {
      const el = document.createElement(tag);
      el.className = className;
      if (text) el.textContent = text;
      return el;
    };
    // One row: sort radio (none for boss stagger), name, show tick.
    const item = (mode, label, shown, { primary = false, sortable = true } = {}) => {
      const el = cell("div", "metricMenuItem");
      el.dataset.metric = mode;
      el.classList.toggle("isPrimary", primary);
      el.classList.toggle("isShown", shown);
      if (sortable) {
        const sort = cell("button", "metricMenuSort");
        sort.type = "button";
        sort.setAttribute("role", "menuitemradio");
        sort.setAttribute("aria-checked", String(primary));
        sort.setAttribute("aria-label", `${t("header.display.sortBy", "Sort by this")}: ${label}`);
        sort.title = t("header.display.sortBy", "Sort by this");
        el.appendChild(sort);
      } else {
        el.appendChild(cell("span", "metricMenuSortNone"));
      }
      const name = cell("button", "metricMenuName", label);
      name.type = "button";
      name.setAttribute("role", "menuitemcheckbox");
      name.setAttribute("aria-checked", String(shown));
      if (primary) name.setAttribute("aria-disabled", "true");
      const check = cell("span", "metricMenuCheck");
      check.setAttribute("aria-hidden", "true");
      el.append(name, check);
      return el;
    };
    menu.replaceChildren();
    const head = cell("div", "metricMenuHead");
    head.append(
      cell("span", "metricMenuHeadSort", t("header.display.sortCol", "Sort")),
      cell("span", "metricMenuHeadName"),
      cell("span", "metricMenuHeadShow", t("header.display.showCol", "Show")),
    );
    menu.appendChild(head);
    for (const mode of ["dps", ...this.displayModes()]) {
      const [, , , , nameKey, nameFallback] = this.metricLabels(mode);
      const primary = mode === this.displayMode;
      menu.appendChild(item(mode, t(nameKey, nameFallback), primary || (this.extraMetrics || []).includes(mode), { primary }));
    }
    menu.appendChild(cell("div", "metricMenuSep"));
    const boss = item("bossStagger", t("header.display.names.bossStagger", "Boss stagger %"), !!this.showBossStagger, {
      sortable: false,
    });
    boss.title = t("header.display.bossStaggerTip", "How far the boss's stagger gauge is worn down, on its HP bar");
    menu.appendChild(boss);
    // Beside the names (Settings' "Beside player names" was here before).
    const stat = this.partyStat;
    const cp = item("combatPower", t("settings.partyStat.options.combatPower", "Combat power"),
      stat === "combatPower" || stat === "both", { sortable: false });
    const gs = item("gearScore", t("settings.partyStat.options.gearScore", "Gear score"),
      stat === "gearScore" || stat === "both", { sortable: false });
    const deaths = item("deaths", t("header.display.names.deaths", "Death count"), this.showDeaths === true, {
      sortable: false,
    });
    menu.append(cp, gs, deaths);
    const colours = item("statColours", t("header.display.names.statColours", "Stat colours"), this.statColours !== false, {
      sortable: false,
    });
    colours.title = t("header.display.statColoursTip", "Each stat in its own colour on the rows, the menu and the button");
    menu.appendChild(colours);
  }

  // Time to kill beside the battle timer: bosses only, and only with their HP
  // known. The remaining HP is read the way the HP bar reads it.
  updateTtk(isBoss, targetId, battleTimeMs, maxHp, totalDamage, currentHp) {
    if (!this.battleTime?.setTtk) return;
    const max = Number(maxHp) || 0;
    if (!this.showTtk || !isBoss || !this.ttk || max <= 0 || !Number.isFinite(battleTimeMs)) {
      this.battleTime.setTtk(null);
      return;
    }
    const live = Number(currentHp);
    const remaining =
      Number.isFinite(live) && live >= 0
        ? Math.min(max, Math.max(0, live))
        : Math.max(0, max - Math.max(0, Number(totalDamage) || 0));
    this.battleTime.setTtk(this.ttk.update(targetId, battleTimeMs, remaining, max));
  }

  // Boss remaining-HP bar. There is no live boss current-HP packet, so remaining
  // is derived from spawn-time max HP minus the damage the meter has tracked
  // against this target. Hidden unless a single boss target with known max HP.
  updateBossHpBar(maxHp, totalDamage, currentHp, staggerPct = -1) {
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
      // Glides only when the change shows, as the rows' bars do (meter.js
      // GLIDE_MIN_JUMP): a boss loses a hair of HP on most updates.
      const jump = Math.abs(pct - (this._bossHpPct ?? 0));
      this.elBossHpFill.classList.toggle("noGlide", jump < 3);
      this._bossHpPct = pct;
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
      const stagEl = this.elBossHpText.querySelector(".bossHpStag");
      if (stagEl) {
        // How far the stagger gauge is worn down, when turned on in the menu.
        const stag = Number(staggerPct);
        const shown = this.showBossStagger && Number.isFinite(stag) && stag >= 0;
        const label = this.i18n?.t("header.display.stagger", "STAG") ?? "STAG";
        stagEl.textContent = shown ? `${label} ${Math.round(stag)}%` : "";
      }
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
    this.invalidateHoverTooltip();
    this.refreshPending = true;
    this.refreshPendingStartedAt = this.nowMs();
    this.lastSnapshot = null;
    this._barsTargetLabel = null;
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
  // Custom dropdowns and live appearance values apply directly: replaying
  // their edit handlers would write back or resend backend combat parameters.
  // Combat power or gear score beside each name: the meter rows redraw on
  // their next refresh, an open Details panel redraws its party list now.
  // The bars' texture: drawn only for a supporter's account (styles.css,
  // [data-bar-texture]); the choice is kept either way.
  applyBarTexture(value) {
    this.barTexture = pickSettingChoice("barTexture", value);
    const data = document.documentElement?.dataset;
    if (!data) return;
    if (this.isSupporter && this.barTexture !== "none") data.barTexture = this.barTexture;
    else delete data.barTexture;
  }

  // The bars glide to a new length in five steps (styles.css), over the
  // time between updates or 200 ms, whichever is shorter.
  applyUpdateRate(ms) {
    const glide = Math.min(Number(ms) || 200, 200);
    document.documentElement?.style.setProperty("--bar-glide", `${glide}ms`);
  }

  // "Offset textures": each bar starts its texture at a place of its own
  // (meter.js sets --tex-x per row; styles.css uses it with this on).
  setTextureOffset(on) {
    this.textureOffset = !!on;
    const data = document.documentElement?.dataset;
    if (!data) return;
    if (this.textureOffset) data.textureOffset = "";
    else delete data.textureOffset;
  }

  // Settings' texture row: open to a supporter, else greyed out with a note.
  syncBarTextureControl() {
    if (this.barTextureDropdownBtn) this.barTextureDropdownBtn.disabled = !this.isSupporter;
    if (this.textureOffsetCheckbox) this.textureOffsetCheckbox.disabled = !this.isSupporter;
    if (this.barTextureHint) this.barTextureHint.hidden = this.isSupporter;
  }

  updateMeterAnchorVisibility(enabled) {
    if (this.meterAnchorRow) this.meterAnchorRow.hidden = !enabled;
  }

  setSupporter(isSupporter) {
    // A development build may act as a supporter (build_features devSupporter).
    this.isSupporter = !!isSupporter || this.devSupporter === true;
    this.applyBarTexture(this.barTexture);
    this.syncBarTextureControl();
  }

  // Whether the signed-in account is a supporter's: the last check's answer
  // at once, then the server's. Run at start and when the account changes.
  async refreshSupporterStatus() {
    if ((await window.a2BuildReady)?.devSupporter) {
      this.devSupporter = true;
      this.setSupporter(true);
    }
    try {
      const seen = await window.javaBridge?.accountStatusCached?.();
      if (seen) this.setSupporter(seen.who?.supporter);
      const who = await window.javaBridge?.accountStatus?.();
      this.setSupporter(who?.supporter);
    } catch {
      // Offline or signed out: keep what was known.
    }
  }

  // Settings' bar preview shows the same figure beside each name, or none.
  syncPreviewPartyStat() {
    for (const el of document.querySelectorAll(".barPreviewList .combatPower")) {
      const text =
        this.partyStat === "gearScore" ? el.dataset.gearScore
        : this.partyStat === "both" ? `${el.dataset.combatPower} · ${el.dataset.gearScore}`
        : el.dataset.combatPower;
      el.textContent = this.partyStat === "off" ? "" : text || "";
      el.hidden = this.partyStat === "off";
    }
  }

  applyPartyStat(value) {
    this.partyStat = pickSettingChoice("partyStat", value);
    this.syncPreviewPartyStat();
    this.meterUI?.invalidateRows?.();
    this.detailsUI?.refreshPartyStat?.();
    if (!this.isCollapse) this.fetchDps?.();
  }

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
      this.updateOdyle();
      this.renderCurrentRows();
      return;
    }
    if (key === this.storageKeys.betaUi) {
      this.setBetaUi(true, { persist: false });    // Classic is retired
      return;
    }
    if (key === this.storageKeys.slimMode) {
      this.setSlimMode(value === "true", { persist: false });
      return;
    }
    if (key === this.storageKeys.theme) {
      this.applyTheme(value, { persist: false });
      return;
    }
    if (key === this.storageKeys.font) {
      this.applyFont(value);
      return;
    }
    if (key === this.storageKeys.playerLimit) {
      this.setPlayerLimit(value, { persist: false });
      return;
    }
    if (key === this.storageKeys.barGap) {
      const next = this.applyBarGap(value, { persist: false });
      const shown = next ?? this.defaultBarGap();
      this.syncSettingsRangeValue(this.barGapInput, null, shown);
      if (this.barGapValue && document.activeElement !== this.barGapInput) this.barGapValue.textContent = `${shown}px`;
      return;
    }
    if (key === this.storageKeys.meterFillSaturation) {
      const normalized = this.normalizeMeterSaturation(value);
      this.applyMeterFillSaturation(normalized, { persist: false });
      this.syncSettingsRangeValue(this.meterSaturationInput, this.meterSaturationValue, normalized);
      return;
    }
    if (key === this.storageKeys.titleOpacity) {
      const numeric = Number(value);
      const normalized = Number.isFinite(numeric) ? Math.max(0, Math.min(100, Math.round(numeric))) : 40;
      this.applyTitleOpacity(normalized, { persist: false });
      this.syncSettingsRangeValue(this.titleOpacityInput, this.titleOpacityValue, normalized);
      return;
    }
    if (key === this.storageKeys.meterFillOpacity || key === this.storageKeys.windowOpacity) {
      const meterFill = key === this.storageKeys.meterFillOpacity;
      const numeric = Number(value);
      const normalized = meterFill ? this.normalizeMeterOpacity(value)
        : Number.isFinite(numeric) ? Math.max(0, Math.min(100, Math.round(numeric))) : 40;
      if (meterFill) this.applyMeterFillOpacity(normalized, { persist: false });
      else this.applyWindowOpacity(normalized, { persist: false });
      this.syncSettingsRangeValue(
        meterFill ? this.meterOpacityInput : this.windowOpacityInput,
        meterFill ? this.meterOpacityValue : this.windowOpacityValue,
        normalized
      );
      return;
    }
    if (key === this.storageKeys.language) {
      const language = String(value || "en");
      this.settingsSelections ||= {};
      const pending = this.i18n?.getLanguage?.() === language ? null
        : this.i18n?.setLanguage?.(language, { persist: false });
      this.settingsSelections.language = this.i18n?.getLanguage?.() || language;
      this.syncSettingsDropdownSelection(this.languageDropdownBtn, this.languageDropdownMenu, this.settingsSelections.language);
      Promise.resolve(pending).then(() => {
        if (window.A2_VIEW === "main") window.javaBridge?.updateOverlaySize?.();
      }).catch((error) => this.logDebug(`Language change failed: ${error?.message || error}`));
      return;
    }
    const choices = {
      defaultMeterMode: "defaultMeterMode",
      trainSelectionMode: "trainSelectionMode",
      allTargetsWindowMs: "allTargetsWindow",
      updateRateMs: "updateRate",
      meterAnchor: "meterAnchor",
      partyStat: "partyStat",
      discordStat: "discordStat",
      barTexture: "barTexture",
      hideOutOfCombat: "hideOutOfCombat",
    };
    for (const [name, control] of Object.entries(choices)) {
      if (key !== this.storageKeys[name]) continue;
      const normalized = pickSettingChoice(name, value);
      this.settingsSelections ||= {};
      this.settingsSelections[name] = normalized;
      if (name === "defaultMeterMode") {
        this.setTargetSelection(normalized, { persist: false, syncBackend: false, reason: "remote setting" });
        if (window.A2_VIEW === "main") window.javaBridge?.updateOverlaySize?.();
      } else if (name === "trainSelectionMode") {
        this.trainSelectionMode = normalized;
      } else if (name === "partyStat") {
        this.applyPartyStat(normalized);
      } else if (name === "barTexture") {
        this.applyBarTexture(normalized);
      } else if (name === "updateRateMs") {
        this.applyUpdateRate(normalized);
      }
      this.syncSettingsDropdownSelection(this[`${control}DropdownBtn`], this[`${control}DropdownMenu`], normalized);
      return;
    }
    const selector = REMOTE_APPLIED_SETTING_CONTROLS[key];
    if (!selector) return;
    const control = document.querySelector(selector);
    if (!control) return;

    // A slider being dragged here is ahead of the echoes of its own earlier
    // values; taking one would move it back under the pointer.
    if (control.type === "range" && document.activeElement === control) return;
    if (control.type === "checkbox") {
      const next = value !== "false";
      if (control.checked === next) return;
      control.checked = next;
    } else {
      if (String(control.value) === String(value)) return;
      control.value = value;
    }
    // Replay the control's handler to apply the value here, but without
    // saving it: the handler used to write it straight back, and during a
    // drag the Settings and meter windows then wrote each other's older
    // values back and forth without end (Target name size, 2026-10-11).
    this._applyingRemoteSetting = true;
    try {
      control.dispatchEvent(new Event("change", { bubbles: true }));
      if (control.type === "range") {
        control.dispatchEvent(new Event("input", { bubbles: true }));
      }
    } finally {
      this._applyingRemoteSetting = false;
    }
  }

  // Keep existing controls and their handlers; a remote update only changes
  // the selected item and label, without rebuilding an open menu.
  syncSettingsDropdownSelection(button, menu, value) {
    let selected = null;
    menu?.querySelectorAll(".settingsDropdownItem").forEach((item) => {
      const active = String(item.dataset.value) === String(value);
      item.classList.toggle("isActive", active);
      if (active) selected = item;
    });
    const text = button?.querySelector(".settingsDropdownText");
    if (text && selected) text.textContent = selected.textContent;
    return selected;
  }

  syncSettingsRangeValue(input, label, value) {
    if (!input || document.activeElement === input) return;
    input.value = String(value);
    if (label) label.textContent = `${value}%`;
    const min = Number(input.min);
    const max = Number(input.max);
    if (Number.isFinite(min) && Number.isFinite(max) && max > min) {
      const percent = Math.max(0, Math.min(100, ((value - min) / (max - min)) * 100));
      input.style.setProperty("--range-pct", `${percent}%`);
    }
  }

  getMetricForRow(row) {
    const metric = this.metricFor(row, this.displayMode);
    metric.mode = this.displayMode;
    // Worked out only when drawn: the sort asks for every row's value often.
    const extras = this.extraMetrics || [];
    Object.defineProperty(metric, "extras", {
      get: () => extras.map((mode) => ({ mode, text: this.metricFor(row, mode).text })),
      enumerable: true,
    });
    return metric;
  }

  metricFor(row, mode) {
    if (mode === "totalDamage") {
      const totalDamage = Number(row?.totalDamage) || 0;
      return {
        value: totalDamage,
        text: this.formatAbbreviatedNumber(totalDamage),
      };
    }
    if (mode === "damageReceived") {
      const received = Number(row?.damageReceived) || 0;
      return { value: received, text: this.formatAbbreviatedNumber(received) };
    }
    if (mode === "hps") {
      const hps = Number(row?.hps) || 0;
      return { value: hps, text: `${this.formatDpsThousands(hps)}${this.meterDpsSuffix()}` };
    }
    if (mode === "totalHeal") {
      const healAmount = Number(row?.healAmount) || 0;
      return { value: healAmount, text: this.formatAbbreviatedNumber(healAmount) };
    }
    if (mode === "stagger") {
      // Gauge points, small whole numbers (a Story boss's gauge is 40).
      const stagger = Number(row?.stagger) || 0;
      return { value: stagger, text: String(stagger) };
    }
    const dps = Number(row?.dps) || 0;
    return { value: dps, text: `${this.formatDpsThousands(dps)}${this.meterDpsSuffix()}` };
  }

  /** "/s" after the meter's DPS figures, or nothing when turned off in Advanced. */
  meterDpsSuffix() {
    if (this.showDpsSuffix === false) return "";
    return this.i18n?.t("meter.dpsSuffix", "/s") ?? "/s";
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
      this.meterTotalDpsEl.textContent = `${this.formatDpsThousands(totalDps)}${this.meterDpsSuffix()}`;
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
      // Exactly what goes, before it goes (security audit M10).
      const files = await window.__TAURI__?.core?.invoke?.("dev_logs_preview").catch(() => []) || [];
      const size = (n) => (n >= 1048576 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
      const list = files.length
        ? "\n\n" + t("settings.sendLogs.files", "These files will be sent:") + "\n" +
          files.map((f) => `• ${f.name} (${size(f.bytes)})`).join("\n") + "\n\n" +
          t("settings.sendLogs.contents", "Packet logs can include chat and other players' names.")
        : "";
      const ok = window.confirm(t("settings.sendLogs.confirm",
        "Send your 3 most recent packet logs to the A2 Tools developer?\n\n" +
        "Packet logs are raw game traffic recorded while packet logging was on, " +
        "including character names. Only the developer can open them, and they " +
        "are deleted after 30 days.") + list);
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

  setPlayerLimit(value, { persist = false } = {}) {
    const numeric = parseInt(value, 10);
    const next = Number.isFinite(numeric) && numeric >= 1 ? numeric : 6;
    const changed = this.playerLimit !== next;
    this.playerLimit = next;
    const wrapper = document.querySelector(".playerLimitDropdownWrapper");
    const button = wrapper?.querySelector(".playerLimitDropdownBtn");
    const menu = wrapper?.querySelector(".playerLimitDropdownMenu");
    this.syncSettingsDropdownSelection(button, menu, next);
    const text = button?.querySelector(".settingsDropdownText");
    if (text) text.textContent = String(next);
    if (persist) this.safeSetSetting(this.storageKeys.playerLimit, String(next));
    if (changed) this.renderCurrentRows();
    if (window.A2_VIEW === "main") window.javaBridge?.updateOverlaySize?.();
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
        this.setPlayerLimit(val, { persist: true });
        menu.style.display = "none";
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
    if (this.isCollapse) {
      this.closeOrphanTooltip();
      return;
    }
    let rowsToRender = Array.isArray(this.lastSnapshot) ? this.lastSnapshot : [];
    if (this.lastTargetMode === "trainTargets" && this.isLocalUserIdentified()) {
      rowsToRender = rowsToRender.filter((row) => row.isUser); // by id or name: an unnamed row is still yours
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
    this.closeOrphanTooltip();
  }

  // A tooltip whose row is gone closes. A row removed from under the pointer
  // (a new fight, a reset, a zone change, the meter folded) never fires its
  // mouse-leave, so the tooltip stayed up over a meter with no rows (Hirai's
  // screenshots, 2026-10-24).
  closeOrphanTooltip() {
    if (!this.hoverTooltipEl?.classList.contains("isVisible")) return;
    const id = this.hoveredDetailsRowId;
    const row = !this.isCollapse && id != null
      ? this.elList?.querySelector?.(`.item[data-row-id="${CSS.escape(String(id))}"]`)
      : null;
    if (row) return;
    this.hoveredDetailsRowId = null;
    this.hideHoverTooltip();
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
        ? this.i18n?.t("connection.detecting", "Connecting...")
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
        this.safeSetSetting("dpsMeter.manualDevice", opt.value);
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

  // Odyle energy: basic (+charged), as the game writes it. Without the
  // game's maximum: it is 840 with a membership and 560 without, and the
  // records read so far don't say which applies.
  //
  // The game sends it only at login, after a server transfer and when it
  // changes, so a meter started mid-session would have nothing to show for a
  // long while. Each value the backend names an owner for is kept by that
  // character's name, and shown dimmed until the game sends one again.
  updateOdyle(energy) {
    if (energy && typeof energy === "object") {
      this.odyleEnergy = energy;
      this.rememberOdyle(energy);
    }
    const el = this.odyleEl;
    if (!el) return;
    const isEnergy = (e) => typeof e?.basic === "number" && typeof e?.charged === "number";
    const live = isEnergy(this.odyleEnergy);
    const shown = live ? this.odyleEnergy : this.rememberedOdyle()[this.USER_NAME];
    if (!this.showOdyle || !isEnergy(shown)) {
      el.classList.remove("isVisible");
      return;
    }
    el.querySelector(".odyleBasic").textContent = String(shown.basic);
    el.querySelector(".odyleCharged").textContent = shown.charged > 0 ? `(+${shown.charged})` : "";
    el.classList.toggle("isRemembered", !live);
    el.classList.add("isVisible");
  }

  rememberedOdyle() {
    const saved = this.safeParseJSON(this.safeGetStorage(this.storageKeys.odyleLast) || "{}", {});
    return saved && typeof saved === "object" && !Array.isArray(saved) ? saved : {};
  }

  rememberOdyle({ basic, charged, name }) {
    const owner = String(name ?? "").trim();
    if (!owner || typeof basic !== "number" || typeof charged !== "number") return;
    const saved = this.rememberedOdyle();
    const was = saved[owner];
    if (was?.basic === basic && was?.charged === charged) return;
    delete saved[owner];
    saved[owner] = { basic, charged };
    // The most recent characters only.
    const names = Object.keys(saved);
    for (const old of names.slice(0, Math.max(0, names.length - 20))) delete saved[old];
    this.safeSetStorage(this.storageKeys.odyleLast, JSON.stringify(saved));
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
        this.i18n?.t("battleTime.notRunning", "AION2 closed") ?? "AION2 closed"
      );
      return;
    }
    if (this.isDetectingPort) {
      this.applyConnectionStatusOverride(
        this.i18n?.t("connection.detecting", "Connecting...") ??
          "Connecting..."
      );
      return;
    }
    if (this._waitingForBoss && this.targetSelection === "bossTargets" && !this._captureSuspended) {
      this.applyConnectionStatusOverride(
        this.i18n?.t("battleTime.waitingBoss", "No boss yet") ?? "No boss yet"
      );
      return;
    }
    this.clearConnectionStatusOverride();
  }

  // Called every 100 ms while idle. Writing the same text again still
  // replaces the text node, and WebKit then lays out and repaints the window
  // each time; only a change is written.
  applyConnectionStatusOverride(text) {
    const root = this.battleTimeRoot.classList;
    if (this._connectionStatusOverride && this.analysisStatusEl.textContent === text
        && this.analysisStatusEl.style.display === "" && root.contains("isVisible")
        && root.contains("state-idle")) return;
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
        this.i18n?.t?.("battleTime.suspended", "Suspended") ?? "Suspended";
    } else {
      this.analysisStatusEl.textContent =
        this.i18n?.t("battleTime.analysing", "Monitoring") ??
        "Monitoring";
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

  // "Gatekeeper Pinopi · Level 2": a Nightmare boss's level is its own, and
  // the name alone does not say which of its ten it is (i18n getNpcLevel).
  withNpcLevel(id, name) {
    const level = this.i18n?.getNpcLevel?.(id) ?? "";
    return level ? `${name} · ${level}` : name;
  }

  getTargetLabel({ targetId = 0, targetName = "", targetMode = "", dungeonId = 0, targetMobCode = 0 } = {}) {
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
        // By NPC code where the backend gave one: targetId is the entity.
        const code = Number(targetMobCode) > 0 ? Number(targetMobCode) : numericId;
        const bossName = this.i18n?.getNpcName?.(code, cleanName) ?? cleanName;
        if (bossName) return this.withNpcLevel(code, bossName);
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
      const code = Number(targetMobCode) > 0 ? Number(targetMobCode) : numericTargetId;
      const localizedName = this.i18n?.getNpcName?.(code, cleanTargetName) ?? cleanTargetName;
      return localizedName ? this.withNpcLevel(code, localizedName) : `Mob #${numericTargetId}`;
    }
    if (cleanTargetName) {
      return cleanTargetName;
    }
    return this.getDefaultTargetLabel(targetMode);
  }

  updateTargetModeButton() {
    this.renderTargetMenu?.();
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
    let startWidth = 0;
    const minWidth = 300;

    // The width the player gave the meter, kept across restarts: it went back
    // to the default on every start (Windows, 2026-10-17). Saved by both
    // resizes, the corner handle here and the window's edge (tauriBridge.js
    // sends "meter-resized"). The handle sets the width only: the meter is
    // as tall as its rows (a saved height, dpsMeter.meterMinHeight, left an
    // empty box under them and is no longer applied).
    const px = (v) => Math.round(parseFloat(v));
    const saveSize = () => {
      const width = px(this.meterEl.style.width);
      if (width >= minWidth && width <= 3000) this.safeSetSetting(this.storageKeys.meterWidth, String(width));
    };
    if (window.A2_VIEW === "main") {
      const width = px(this.safeGetSetting(this.storageKeys.meterWidth));
      if (width >= minWidth && width <= 3000) this.meterEl.style.width = `${width}px`;
      window.addEventListener("meter-resized", saveSize);
    }

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
      this.meterEl.style.width = `${nextWidth}px`;
      // Linux: the window grows sideways ahead of the meter (tauriBridge.js).
      window.javaBridge?.growResizeViewport?.(nextWidth + 16);
    };

    const onMouseUp = () => {
      if (!isResizing) return;
      isResizing = false;
      saveSize();
    };

    this.resizeHandle.addEventListener("mousedown", (event) => {
      event.preventDefault();
      event.stopPropagation();
      const rect = this.meterEl.getBoundingClientRect();
      startWidth = rect.width;
      startX = event.screenX;
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
        this._watchOverlayCards();
        // Stay locked across restarts, as the player left it.
        if (show && this.safeGetSetting(this.storageKeys.overlayLocked) === "true") {
          requestAnimationFrame(() => this._setOverlayLocked(true));
        }
      })
      .catch(() => {});
  }

  // A card open in the page (a one-time popup at start, the update prompt,
  // the support card) takes the mouse though the overlay is locked: locked
  // from last time, nothing on it could be clicked, and it covers the lock
  // button. The lock itself stays as it was.
  _watchOverlayCards() {
    const cards = [...document.querySelectorAll(".updateModal, .discordPromo, .supportModal")];
    if (!cards.length || typeof MutationObserver !== "function") return;
    let wasOpen = false;
    const sync = () => {
      const open = cards.some((card) => card.classList.contains("isOpen"));
      if (open === wasOpen) return;
      wasOpen = open;
      window.javaBridge?.setOverlayCardOpen?.(open);
    };
    const observer = new MutationObserver(sync);
    for (const card of cards) observer.observe(card, { attributes: true, attributeFilter: ["class"] });
    sync();
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
    // Locked, clicks go through to the game: the menu could not be closed.
    if (this._overlayLocked) {
      this.setMetricMenuOpen?.(false);
      this.setTargetMenuOpen?.(false);
      this.setFooterMenuOpen?.(false);
    }
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
  // either way in Settings, and never again once answered. True if it opens.
  // Stream overlay: the meter's rows served on the local network for OBS on
  // another PC. Off unless turned on. The backend owns the server and its key;
  // this shows what it reports and passes the toggle and port through.
  initStreamOverlaySettings() {
    if (this._streamOverlaySettings) return;
    const group = document.querySelector(".streamOverlayGroup");
    const bridge = window.javaBridge;
    if (!group || typeof bridge?.streamOverlayStatus !== "function") return;
    const checkbox = group.querySelector(".streamOverlayCheckbox");
    const details = group.querySelector(".streamOverlayDetails");
    const portInput = group.querySelector(".streamOverlayPortInput");
    const newKeyBtn = group.querySelector(".streamOverlayNewKeyBtn");
    const urlsEl = group.querySelector(".streamOverlayUrls");
    const statusEl = group.querySelector(".streamOverlayStatus");
    const nameInput = group.querySelector(".streamOverlayNameInput");
    if (!checkbox || !details || !portInput || !newKeyBtn || !urlsEl || !statusEl) return;
    const t = (key, fallback) => window.i18n?.t?.(key, fallback) ?? fallback;
    let lastPort = 18731;
    let requestSequence = 0;
    let mutation = Promise.resolve();

    const copy = async (input, button) => {
      let ok = false;
      try {
        await navigator.clipboard.writeText(input.value);
        ok = true;
      } catch {
        input.focus();
        input.select();
        try {
          ok = document.execCommand("copy");
        } catch {
          ok = false;
        }
      }
      if (!ok) return;
      button.textContent = t("settings.streamOverlay.copied", "Copied");
      clearTimeout(button._copiedTimer);
      button._copiedTimer = setTimeout(() => {
        button.textContent = t("settings.streamOverlay.copy", "Copy");
      }, 1500);
    };

    const show = (status) => {
      if (!status) return;
      const enabled = !!status.enabled;
      const urls = Array.isArray(status.urls) ? status.urls : [];
      checkbox.checked = enabled;
      details.style.display = enabled ? "" : "none";
      if (Number(status.port) > 0) lastPort = Number(status.port);
      if (document.activeElement !== portInput) portInput.value = String(lastPort);
      portInput.classList.remove("isInvalid");

      urlsEl.replaceChildren();
      for (const url of urls) {
        const row = document.createElement("div");
        row.className = "streamOverlayUrlRow";
        const input = document.createElement("input");
        input.className = "streamOverlayUrlInput";
        input.readOnly = true;
        input.spellcheck = false;
        input.value = url;
        input.addEventListener("mousedown", (event) => event.stopPropagation());
        input.addEventListener("focus", () => input.select());
        const button = document.createElement("button");
        button.type = "button";
        button.className = "settingsAction";
        button.dataset.i18n = "settings.streamOverlay.copy";
        button.textContent = t("settings.streamOverlay.copy", "Copy");
        button.addEventListener("click", () => copy(input, button));
        row.append(input, button);
        urlsEl.appendChild(row);
      }

      let message = "";
      if (enabled && status.error) {
        message =
          window.i18n?.format?.(
            "settings.streamOverlay.error",
            { error: status.error },
            `The overlay could not start: ${status.error}`
          ) ?? `The overlay could not start: ${status.error}`;
      } else if (enabled && urls.length === 0) {
        message = t(
          "settings.streamOverlay.noAddress",
          "No network address found. Is this PC connected to your network?"
        );
      }
      statusEl.textContent = message;
    };
    const fail = (err) => {
      statusEl.textContent = typeof err === "string" ? err : err?.message || String(err);
    };

    const request = (work, { mutate = false } = {}) => {
      const sequence = ++requestSequence;
      // A refresh must see completed changes, and rapid changes keep their order.
      const pending = mutation.catch(() => {}).then(work);
      if (mutate) mutation = pending;
      return pending.then((status) => {
        if (sequence !== requestSequence) return;
        group.style.display = "";
        show(status);
        if (nameInput && document.activeElement !== nameInput) {
          nameInput.value = this.safeGetSetting("dpsMeter.streamOverlayName") || "";
        }
      }).catch((error) => {
        if (sequence === requestSequence) fail(error);
      });
    };
    this._streamOverlaySettings = {
      refresh: () => request(() => bridge.streamOverlayStatus()),
      invalidate: () => { ++requestSequence; },
    };
    if (window.A2_VIEW === "settings") window.addEventListener("settings-hidden", () => {
      this.invalidateStreamOverlaySettings();
    });

    const apply = () => {
      let port = Number.parseInt(portInput.value, 10);
      const valid = Number.isInteger(port) && port >= 1024 && port <= 65535;
      if (!valid) {
        if (checkbox.checked) {
          ++requestSequence;
          portInput.classList.add("isInvalid");
          statusEl.textContent = t(
            "settings.streamOverlay.portInvalid",
            "Enter a port between 1024 and 65535."
          );
          return;
        }
        port = lastPort;
      }
      const enabled = checkbox.checked;
      request(() => bridge.streamOverlayConfigure(enabled, port), { mutate: true });
    };

    checkbox.addEventListener("change", apply);
    portInput.addEventListener("change", apply);
    portInput.addEventListener("mousedown", (event) => event.stopPropagation());
    portInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter") portInput.blur();
    });
    // Your name on stream; the overlay reads it live. Blank means your
    // character name. Other players are never sent by name.
    if (nameInput) {
      nameInput.value = this.safeGetSetting("dpsMeter.streamOverlayName") || "";
      const saveName = () => this.safeSetSetting("dpsMeter.streamOverlayName", nameInput.value.trim());
      nameInput.addEventListener("change", saveName);
      nameInput.addEventListener("mousedown", (event) => event.stopPropagation());
      nameInput.addEventListener("keydown", (event) => {
        if (event.key === "Enter") nameInput.blur();
      });
    }
    newKeyBtn.addEventListener("click", () => {
      request(() => bridge.streamOverlayNewKey(), { mutate: true });
    });

    this.refreshStreamOverlaySettings();
  }

  refreshStreamOverlaySettings() {
    return this._streamOverlaySettings?.refresh();
  }

  invalidateStreamOverlaySettings() {
    this._streamOverlaySettings?.invalidate();
  }

  // A one-time popup about showing A2Tools users in the party to each other,
  // unticked: it tells party members you use a meter, so it is the player's
  // to turn on (#131). Done keeps the tick's answer; ×, Escape or a click
  // outside keep what was set (off, unless turned on before).
  maybeShowPartyPromo() {
    const promo = document.querySelector("#partyPromo");
    if (!promo) return false;
    if (this.safeGetSetting(this.storageKeys.partyPromoShown) === "true") return false;
    const checkbox = promo.querySelector(".partyPromoCheckbox");
    let answered = false;
    const onKey = (event) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        answer(this.safeGetSetting(this.storageKeys.partyPresence) === "true");
      }
    };
    const answer = (enabled) => {
      if (answered) return;
      answered = true;
      this.safeSetSetting(this.storageKeys.partyPresence, String(enabled));
      this.safeSetSetting(this.storageKeys.partyPromoShown, "true");
      const settingsCheckbox = document.querySelector(".partyPresenceCheckbox");
      if (settingsCheckbox) settingsCheckbox.checked = enabled;
      promo.classList.remove("isOpen");
      promo.setAttribute("aria-hidden", "true");
      document.removeEventListener("keydown", onKey, true);
    };
    const kept = () => this.safeGetSetting(this.storageKeys.partyPresence) === "true";
    promo.querySelector(".partyPromoDone")?.addEventListener("click", () => answer(!!checkbox?.checked));
    promo.querySelector(".partyPromoClose")?.addEventListener("click", () => answer(kept()));
    promo.addEventListener("click", (event) => {
      if (event.target === promo) answer(kept());
    });
    setTimeout(() => {
      if (answered) return;
      if (checkbox) checkbox.checked = kept();
      promo.classList.add("isOpen");
      promo.setAttribute("aria-hidden", "false");
      document.addEventListener("keydown", onKey, true);
    }, 2500);
    return true;
  }

  maybeShowDiscordPromo() {
    const promo = document.querySelector("#discordPromo");
    if (!promo) return false;
    if (this.safeGetSetting(this.storageKeys.discordPromoShown) === "true") return false;
    const current = this.safeGetSetting(this.storageKeys.discordActivity);
    if (current === "true" || current === "false") return false;

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
    return true;
  }

  // Sharing (2.0.80): asked once, before anything uploads without an
  // account (account/guest.rs `uploads`). Not signed in and not asked yet:
  // those already sharing (guest uploads on, the 2.0.78-79 default) get the
  // notice (C); anyone who never chose gets the choice (B); those who turned
  // it off themselves are taken as asked. Signed in: asked, nothing shown.
  // Resolves true when it showed something.
  // What's new (#127): the first launch after an update opens Settings on
  // it. Not after a new install (nothing changed for them) or a downgrade,
  // and not on a launch that already asked about sharing (`quiet`): then it
  // waits for the next one. Resolves true when it opened.
  async maybeShowWhatsNew({ existingInstall = false, quiet = false } = {}) {
    const invoke = window.__TAURI__?.core?.invoke;
    const version = await invoke?.("get_app_version").catch(() => "");
    if (!version) return false;
    const seen = this.safeGetSetting(this.storageKeys.lastSeenVersion);
    if (seen === version) return false;
    const parts = (v) => String(v).split(/[.-]/).map((n) => parseInt(n, 10) || 0);
    const newer = (a, b) => {
      const [x, y] = [parts(a), parts(b)];
      for (let i = 0; i < 3; i++) if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) > (y[i] || 0);
      return false;
    };
    // Nothing seen yet: a new install, or one from before this setting,
    // which had answered the sharing question.
    const updated = seen ? newer(version, seen) : existingInstall;
    if (updated && quiet) return false;
    this.safeSetSetting(this.storageKeys.lastSeenVersion, version);
    if (!updated) return false;
    await invoke("open_settings_at", { section: "changelog" }).catch(() => {});
    return true;
  }

  async maybeShowSharing() {
    if ((await window.a2BuildReady)?.online === false) return false;
    if (this.safeGetSetting(this.storageKeys.sharingAsked) === "true") return false;
    const asked = () => this.safeSetSetting(this.storageKeys.sharingAsked, "true");
    try {
      const seen = await window.javaBridge?.accountStatusCached?.();
      if (seen?.who || (await window.javaBridge?.accountStatus?.())) {
        asked();
        return false;
      }
    } catch {
      return false; // a token is stored but could not be checked: ask another time
    }
    const guest = this.safeGetSetting(this.storageKeys.guestUpload);
    if (guest === "true") return this.showSharingNotice(asked);
    if (guest === "false") {
      asked();
      return false;
    }
    return this.showSharingChoice(asked);
  }

  // B: share anonymously (picked to start) or don't, with Sign in for fights
  // under the player's name. Only Continue (or a finished sign-in) answers;
  // Escape closes it unanswered, so nothing uploads and it asks next launch.
  showSharingChoice(asked) {
    const promo = document.querySelector("#accountPromo");
    if (!promo) return false;
    const choices = [...promo.querySelectorAll(".sharingChoice")];
    const signInBtn = promo.querySelector(".accountPromoSignIn");
    const codeBox = promo.querySelector(".accountPromoCodeBox");
    const codeEl = promo.querySelector(".accountPromoCode");
    const stateEl = promo.querySelector(".accountPromoState");
    let share = true;
    let open = false;
    const pick = (on) => {
      share = on;
      for (const c of choices) {
        const picked = (c.dataset.share === "true") === on;
        c.classList.toggle("isPicked", picked);
        c.setAttribute("aria-pressed", String(picked));
      }
    };
    const setGuest = (on) => {
      this.safeSetSetting(this.storageKeys.guestUpload, String(on));
      if (this.guestUploadCheckbox) this.guestUploadCheckbox.checked = on;
    };
    const onKey = (event) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        close(false);
      }
    };
    const close = (answered) => {
      if (!open) return;
      open = false;
      if (answered) {
        asked();
        this.safeSetSetting(this.storageKeys.accountPromoShown, "true");
        this.safeSetSetting(this.storageKeys.guestPromoShown, "true");
        this.safeSetSetting("dpsMeter.newInstall", "false");
      }
      promo.classList.remove("isOpen");
      promo.setAttribute("aria-hidden", "true");
      document.removeEventListener("keydown", onKey, true);
    };
    for (const c of choices) c.addEventListener("click", () => pick(c.dataset.share === "true"));
    promo.querySelector(".sharingContinue")?.addEventListener("click", () => {
      setGuest(share);
      close(true);
    });
    promo.querySelector(".sharingPolicy")?.addEventListener("click", () => {
      window.javaBridge?.openBrowser?.("https://a2tools.app/privacy");
    });

    // Sign in: the same browser approval as Settings. Once connected, fights
    // upload to the account (and the guest's, if any, become its).
    let signingIn = false;
    this.onAccountPromoResult = (result) => {
      if (!signingIn) return;
      if (result?.connected) {
        signingIn = false;
        this.safeSetSetting(this.storageKeys.autoUpload, "true");
        if (this.autoUploadCheckbox) this.autoUploadCheckbox.checked = true;
        close(true);
      } else if (result?.error) {
        const msg = String(result.error);
        if (stateEl) {
          stateEl.textContent = window.i18n?.format?.(
            "settings.account.failed", { error: msg }, `Sign-in failed: ${msg}`);
        }
        if (codeBox) codeBox.style.display = "none";
        if (signInBtn) signInBtn.disabled = false;
        signingIn = false;
      }
    };
    signInBtn?.addEventListener("click", async () => {
      signInBtn.disabled = true;
      if (stateEl) stateEl.textContent = "";
      try {
        const prompt = await window.javaBridge?.accountBeginLink?.();
        if (!prompt?.userCode) {
          signInBtn.disabled = false;
          return;
        }
        signingIn = true;
        if (codeEl) codeEl.textContent = prompt.userCode;
        if (codeBox) codeBox.style.display = "block";
        if (stateEl) {
          stateEl.textContent = window.i18n?.t?.("settings.account.working", "Waiting for approval…");
        }
      } catch (err) {
        const msg = typeof err === "string" ? err : err?.message || String(err);
        if (stateEl) {
          stateEl.textContent = window.i18n?.format?.(
            "settings.account.failed", { error: msg }, `Sign-in failed: ${msg}`);
        }
        signInBtn.disabled = false;
      }
    });

    // A moment after start, so it does not land on top of the first paint.
    setTimeout(() => {
      pick(true);
      open = true;
      promo.classList.add("isOpen");
      promo.setAttribute("aria-hidden", "false");
      document.addEventListener("keydown", onKey, true);
    }, 2500);
    return true;
  }

  // C: for those already sharing. Shown counts as asked: sharing goes on
  // unless they stop it. Stop sharing deletes their earlier guest uploads
  // too (guest_stop_sharing, in the background); Details opens Settings →
  // Privacy.
  showSharingNotice(asked) {
    const notice = document.querySelector("#sharingNotice");
    if (!notice) return false;
    const text = notice.querySelector(".sharingNoticeText");
    let open = false;
    const close = () => {
      if (!open) return;
      open = false;
      notice.classList.remove("isOpen");
      notice.setAttribute("aria-hidden", "true");
    };
    notice.querySelector(".sharingKeep")?.addEventListener("click", close);
    notice.querySelector(".sharingDetails")?.addEventListener("click", () => {
      window.__TAURI__?.core?.invoke?.("open_settings_at", { section: "privacy" }).catch(() => {});
      close();
    });
    notice.querySelector(".sharingStop")?.addEventListener("click", () => {
      if (this.guestUploadCheckbox) this.guestUploadCheckbox.checked = false;
      window.__TAURI__?.core?.invoke?.("guest_stop_sharing").catch(() => {});
      for (const b of notice.querySelectorAll("button")) b.disabled = true;
      if (text) text.textContent = window.i18n?.t?.("sharingNotice.stopped",
        "Sharing stopped. The fights you shared before are being removed from a2tools.app.");
      setTimeout(close, 4000);
    });
    setTimeout(() => {
      asked();
      open = true;
      notice.classList.add("isOpen");
      notice.setAttribute("aria-hidden", "false");
    }, 2500);
    return true;
  }

  _updateSuspendStatusMessage() {
    const el = this.analysisStatusEl || document.querySelector(".battleTime .analysisStatus");
    if (!el) return;
    if (this._captureSuspended) {
      el.textContent = this.i18n?.t?.("battleTime.suspended", "Suspended") ?? "Suspended";
      el.style.display = "";
      // Ensure the battle time bar is visible so the message shows
      if (this.battleTimeRoot) this.battleTimeRoot.classList.add("isVisible");
    } else {
      el.textContent = this.i18n?.t?.("battleTime.analysing", "Monitoring") ?? "Monitoring";
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
  // Quit, Close and Escape in the Settings window are answered by a script
  // the window runs before this page (SETTINGS_WINDOW_SCRIPT in app.rs), so
  // they work before any of this has loaded.
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
    await window.javaBridge?.notifyUiReady?.();

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
