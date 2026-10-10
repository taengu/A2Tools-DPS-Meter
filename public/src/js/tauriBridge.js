/**
 * Tauri 2 bridge adapter.
 * Creates window.javaBridge and window.dpsData compatibility objects
 * that translate the old JavaFX bridge calls to Tauri 2 IPC.
 */
(function () {
  "use strict";

  const { invoke } = window.__TAURI__.core;

  // Which optional parts this build has (app.rs build_features). The private
  // build, compiled without online features, has no account, uploads, update
  // check, Send Logs to Dev, Discord activity or stream overlay: html.a2-offline
  // hides them (styles.css) and the code below skips them.
  window.a2Build = { online: true };
  window.a2BuildReady = invoke("build_features")
    .then((features) => {
      window.a2Build = { online: features?.online !== false };
      document.documentElement.classList.toggle("a2-offline", !window.a2Build.online);
      return window.a2Build;
    })
    .catch(() => window.a2Build);
  const { listen } = window.__TAURI__.event;
  const { open: shellOpen } = window.__TAURI__.opener;

  // The backend enables compositor resizing only on GNOME.
  const isLinux = document.documentElement.classList.contains("linux");
  // Native startup policy is independent of the webview's user agent.
  const loadsHidden = window.__A2_WINDOW_STARTUP__?.loadsHidden === true;
  const reusesSettings = window.__A2_WINDOW_STARTUP__?.reusesSettings === true;
  let compositorResize = isLinux ? null : false;
  const compositorResizeReady = isLinux ? invoke("compositor_resize_supported")
    .then((supported) => {
      compositorResize = supported;
      if (supported) {
        const apply = () => document.body.classList.add("linuxOverlay");
        if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", apply, { once: true });
        else apply();
      }
      return supported;
    })
    .catch(() => { compositorResize = false; return false; })
    : Promise.resolve(false);

  // Three windows share this bundle: the game overlay (label "main"), the
  // Details view ("details") which the user can park on a second monitor, and
  // Settings ("settings"). Which one we are is needed synchronously, before
  // anything else initialises.
  //
  // __A2_VIEW__ is injected by the window's initialization_script and runs
  // before any page script. getCurrentWindow().label agrees with it and is kept
  // as a fallback, but the injected value is preferred because it does not
  // depend on the Tauri JS API being present: a window missing from
  // capabilities/default.json gets no API injected at all, and the fallback
  // would then throw rather than answer.
  let viewMode = "main";
  try {
    const injected = window.__A2_VIEW__;
    const fromUrl = new URLSearchParams(window.location.search).get("view");
    // Per-fight windows are labelled details-<id>, so the label is only a
    // fallback for the singletons; the injected value is what identifies them.
    const label = window.__TAURI__?.window?.getCurrentWindow?.()?.label;
    const candidate = injected || fromUrl || label;
    if (candidate === "details" || candidate === "settings" || candidate === "history") {
      viewMode = candidate;
    }
  } catch {}
  window.A2_VIEW = viewMode;
  if (viewMode === "details") {
    document.documentElement.classList.add("detailsWindow");
  } else if (viewMode === "settings") {
    document.documentElement.classList.add("settingsWindow");
  } else if (viewMode === "history") {
    document.documentElement.classList.add("historyWindow");
  }

  // Tool-window bootstrap. This runs regardless of whether app startup
  // succeeds, so a failure in core.js can never leave a window the user can see
  // but not use.
  if (viewMode !== "main") {
    // Show this window's panel here rather than relying on core.js. Its start()
    // does a lot of work and is wrapped in a try/catch, so a single failure in
    // an unrelated part of it used to leave the tool window blank.
    const PANEL_FOR_VIEW = {
      settings: [".settingsPanel", "isOpen"],
      history: [".historyPanel", "open"],
      details: [".detailsPanel", "open"],
    };
    const showPanel = () => {
      const [sel, cls] = PANEL_FOR_VIEW[viewMode] || PANEL_FOR_VIEW.details;
      document.querySelector(sel)?.classList.add(cls);
    };
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", showPanel, { once: true });
    } else {
      showPanel();
    }

    // A blank tool window is impossible to diagnose from the outside, so make
    // startup failures visible in the window itself.
    window.addEventListener("error", (event) => {
      try {
        let bar = document.querySelector(".toolWindowError");
        if (!bar) {
          bar = document.createElement("div");
          bar.className = "toolWindowError";
          bar.style.cssText =
            "position:fixed;left:0;right:0;bottom:0;z-index:99999;padding:8px 12px;" +
            "background:#4a1220;color:#ffd9df;font:12px/1.4 ui-monospace,Consolas,monospace;" +
            "white-space:pre-wrap;max-height:40vh;overflow:auto;border-top:1px solid #ff5f7a";
          document.body.appendChild(bar);
        }
        bar.textContent += `${event.message}
    at ${event.filename}:${event.lineno}:${event.colno}
`;
      } catch {}
    });

    // WebView2 tool windows must load visible. Linux Settings instead reveals
    // directly from notifyUiReady, after preparing the form; a hidden webview
    // need not run animation frames, so its reveal must not wait for one.
    const reveal = () => {
      try {
        const w = window.__TAURI__.window.getCurrentWindow();
        w.show();
        w.setFocus();
      } catch (e) {
        console.error("[A2Tools] reveal failed", e);
      }
    };
    const scheduleReveal = () => requestAnimationFrame(() => requestAnimationFrame(reveal));
    if (!(loadsHidden && viewMode === "settings")) {
      if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", scheduleReveal, { once: true });
      } else {
        scheduleReveal();
      }
      // Belt and braces if rAF never fires (window fully occluded at creation).
      setTimeout(reveal, 1200);
    }
  }

  // --- Cached state ---
  let settingsCache = {};
  let settingsLoaded = false;
  let cachedDpsJson = null;      // latest DPS snapshot as JSON string
  let cachedPing = null;
  let cachedCaptureStatus = null;
  let cachedDetailsContext = null;
  let cachedAppVersion = "";     // populated on startup from Tauri backend
  let lastSkillDetailsIssue = "";
  let captureSuspended = false;  // the suspend button's state; the backend's is the truth
  // null awaits the first show; false is an explicitly paused form.
  let settingsActive = viewMode === "settings" && loadsHidden ? null : true;
  let settingsLifecycleGeneration = 0;
  let settingsReadId = 0;
  let settingsReadPromise = null;
  let settingsRevision = 0;
  let pendingSettingsAccountResult = null;
  const settingsKeyRevisions = new Map();
  const cacheSetting = (key, value) => {
    settingsKeyRevisions.set(key, ++settingsRevision);
    settingsCache[key] = String(value);
    try { localStorage.setItem(key, String(value)); } catch {}
  };
  const applyPendingSettingsAccountResult = () => {
    if (pendingSettingsAccountResult === null || !window._dpsApp?.accountStateEl
      || !window._dpsApp?.refreshAccountPanel) return false;
    const result = pendingSettingsAccountResult;
    pendingSettingsAccountResult = null;
    window._dpsApp.refreshAccountPanel(result);
    window._dpsApp.onAccountPromoResult?.(result);
    return true;
  };
  // Linux keeps Settings when it is closed. Shown again, it resumes; not every
  // change is broadcast as setting-changed, so the backend is read again.
  const resumeSettings = ({ resync = true } = {}) => {
    if (viewMode !== "settings" || settingsActive) return;
    settingsActive = true;
    applyPendingSettingsAccountResult();
    startStatusPolling();
    if (resync && reusesSettings) loadSettings().then((applied) => {
      if (applied && settingsActive) window._dpsApp?.syncSettingsForm?.();
    });
  };
  let devicesLoading = null;
  let devicesLoadedAt = 0;
  const DEVICES_STALE_MS = 10000;
  const loadDevices = () => {
    if (!devicesLoading) {
      devicesLoading = invoke("get_available_devices")
        .then((devices) => {
          window._cachedDevices = devices;
          devicesLoadedAt = Date.now();
          return devices;
        })
        .finally(() => { devicesLoading = null; });
    }
    return devicesLoading;
  };

  // A reloaded window picks the suspend state back up from the backend.
  invoke("is_capture_suspended").then((v) => {
    captureSuspended = !!v;
  }).catch(() => {});

  // Fetch app version from backend (sourced from Cargo.toml via env!("CARGO_PKG_VERSION"))
  invoke("get_app_version").then((v) => {
    if (typeof v === "string") cachedAppVersion = v;
  }).catch(() => {});

  // Load settings from Rust backend and merge with localStorage.
  // localStorage acts as the synchronous fallback for first reads before invoke resolves.
  const loadSettings = () => {
    const readId = ++settingsReadId;
    const revision = settingsRevision;
    const pending = invoke("get_settings").then((s) => {
      if (readId !== settingsReadId) return false;
      settingsLoaded = true;
      if (!s || typeof s !== "object" || Array.isArray(s)) return false;
      const merged = { ...s };
      // An event or local edit after this read began is newer than its snapshot.
      for (const [key, changed] of settingsKeyRevisions) {
        if (changed > revision) merged[key] = settingsCache[key];
      }
      for (const key of Object.keys(settingsCache)) {
        if (!(key in merged)) {
          try { localStorage.removeItem(key); } catch {}
        }
      }
      settingsCache = merged;
      for (const [key, value] of Object.entries(merged)) {
        try { localStorage.setItem(key, value); } catch {}
      }
      return true;
    }).catch(() => {
      if (readId === settingsReadId) settingsLoaded = true;
      return false;
    });
    settingsReadPromise = pending;
    return pending;
  };
  loadSettings();
  window.a2SettingsReady = (async () => {
    // A reopen during startup can supersede the initial read.
    let pending;
    do { pending = settingsReadPromise; await pending; }
    while (pending !== settingsReadPromise);
  })();

  // --- DPS data polling via events ---
  // The Rust backend emits "dps-update" every 500ms.
  // We cache the latest snapshot so getDpsData() can return it synchronously.
  if (viewMode !== "settings") listen("dps-update", (event) => {
    cachedDpsJson = JSON.stringify(event.payload);
    // NOTE: do NOT pre-fetch get_details_context here. It clones the full combat
    // aggregate and runs O(targets×actors×skills) work; firing it every 500ms
    // (even with the details panel closed) was a major source of CPU lag that
    // grew with fight length. The details panel self-refreshes every 2s while
    // open (details.js), and getDetailsContext() below refreshes on demand.
  });

  if (viewMode !== "settings") listen("ping-update", (event) => {
    cachedPing = event.payload;
    // Push directly to the app instance for immediate display update
    window._dpsApp?.updatePing?.(event.payload);
  });

  listen("capture-status-changed", (event) => {
    cachedCaptureStatus = event.payload;
    if (viewMode === "settings" && settingsActive) window._dpsApp?.refreshConnectionInfo?.();
  });

  // Settings live in their own window, so a change there has to reach the meter.
  // Refresh the local cache and hand the app the key so it can re-apply just
  // that option — see applyRemoteSettingChange() in core.js.
  listen("setting-changed", (event) => {
    const key = event?.payload?.key;
    const value = event?.payload?.value;
    if (typeof key !== "string") return;
    cacheSetting(key, value);
    window._dpsApp?.applyRemoteSettingChange?.(key, String(value));
  });

  listen("account-changed", (event) => {
    if (viewMode === "settings" && (!settingsActive || !window._dpsApp?.accountStateEl)) {
      // A device grant finishes once. Keep its outcome while the form is
      // hidden: account_status alone cannot distinguish pending from expired.
      pendingSettingsAccountResult = event?.payload || null;
      return;
    }
    window._dpsApp?.refreshAccountPanel?.(event?.payload);
    window._dpsApp?.onAccountPromoResult?.(event?.payload);
  });

  // Settings was asked to open while already open; check the account again
  // behind what it shows.
  const settingsShownSubscription = listen("settings-shown", () => {
    if (viewMode === "settings") ++settingsLifecycleGeneration;
    if (!applyPendingSettingsAccountResult()) window._dpsApp?.refreshAccountPanel?.();
    resumeSettings();
  });
  const settingsHiddenSubscription = viewMode === "settings" ? listen("settings-hidden", () => {
    window.dispatchEvent(new Event("settings-hidden"));
  }) : undefined;
  const settingsLifecycleReady = Promise.all([settingsShownSubscription, settingsHiddenSubscription]);

  // The lock hotkey toggled the click-through lock; the page follows.
  listen("overlay-lock-changed", (event) => {
    window._dpsApp?._onOverlayLockChanged?.(!!event?.payload);
  });

  listen("npcap-missing", () => {
    const msg = "Npcap is required for packet capture but is not installed.\n\nWould you like to download it now?";
    if (confirm(msg)) {
      shellOpen("https://npcap.com/#download");
    }
  });

  listen("combat-reset", () => {
    // Clear frontend state without re-invoking backend (already cleared by hotkey)
    cachedDpsJson = null;
    if (window._dpsApp) {
      window._dpsApp.refreshPending = false;
      window._dpsApp.lastJson = null;
      window._dpsApp.lastSnapshot = [];
      window._dpsApp._lastRenderedListSignature = "";
      window._dpsApp._lastRenderedRowsSummary = null;
      window._dpsApp._lastBattleTimeMs = null;
      window._dpsApp._battleTimeVisible = false;
      window._dpsApp.battleTime?.setVisible?.(false);
      window._dpsApp.meterUI?.onResetMeterUi?.();
    }
  });

  // ===== window.dpsData — polled by core.js every 100ms =====
  window.dpsData = {
    getDpsData() {
      return cachedDpsJson;
    },

    getDetailsContext() {
      // Return cached context synchronously; refresh in background
      invoke("get_details_context").then((ctx) => {
        cachedDetailsContext = ctx;
      }).catch(() => {});
      if (cachedDetailsContext) {
        return typeof cachedDetailsContext === "string"
          ? cachedDetailsContext
          : JSON.stringify(cachedDetailsContext);
      }
      return null;
    },

    async getTargetDetails(targetId, actorIdsJson) {
      try {
        const actorIds = actorIdsJson ? JSON.parse(actorIdsJson) : null;
        const result = await invoke("get_skill_details", {
          targetId: Number(targetId),
          actorIds: Array.isArray(actorIds) ? actorIds.map(Number) : null,
        });
        return JSON.stringify(result);
      } catch (e) {
        console.error("[A2Tools] getTargetDetails error:", e);
        return null;
      }
    },

    // The live fight's buffs and debuffs on a target: { targetId, startTimeMs,
    // durationMs, buffs }, or null.
    async getFightBuffs(targetId) {
      try {
        return await invoke("get_fight_buffs", { targetId: Number(targetId) });
      } catch (e) {
        console.error("[A2Tools] getFightBuffs error:", e);
        return null;
      }
    },

    async getBattleDetail(actorId, { summaryOnly = false } = {}) {
      const dps = cachedDpsJson ? JSON.parse(cachedDpsJson) : null;
      const targetIds = [...new Set((Array.isArray(dps?.detailTargetIds)
        ? dps.detailTargetIds : [dps?.targetId]).map(Number).filter((id) => id > 0))];
      if (!targetIds.length) {
        const issue = `no selected target (mode=${dps?.targetMode || "unknown"}, rows=${Object.keys(dps?.map || {}).length})`;
        if (issue !== lastSkillDetailsIssue) window.javaBridge?.logToDebug?.(`Skill details: ${issue}`);
        lastSkillDetailsIssue = issue;
        return null;
      }
      const aid = Number(actorId);
      // A long session may select dozens of targets. Bound IPC fan-out instead
      // of filling the native work queue with all of them at once.
      const results = new Array(targetIds.length);
      let nextTarget = 0;
      let failed = false;
      await Promise.all(Array.from({ length: Math.min(4, targetIds.length) }, async () => {
        while (!failed && nextTarget < targetIds.length) {
          const index = nextTarget++;
          try {
            results[index] = await invoke("get_skill_details", {
              targetId: targetIds[index],
              actorIds: Number.isFinite(aid) && aid > 0 ? [aid] : null,
              summaryOnly,
            });
          } catch (error) {
            failed = true;
            throw error;
          }
        }
      }));
      let result = results[0];
      if (results.length > 1) {
        const startTime = Math.min(...results.map((item) => Number(item.startTime) || 0));
        const mergeSkills = (field) => {
          const merged = new Map();
          const sumFields = ["time", "dmg", "multiHitCount", "multiHitDamage", "multiHitHits",
            "crit", "parry", "back", "frontal", "perfect", "double", "shieldBlock", "ironWall",
            "regeneration", "perfectBlock", "miss", "resist", "regen"];
          for (const item of results) {
            const offset = (Number(item.startTime) || 0) - startTime;
            for (const skill of item[field] || []) {
              const key = `${skill.actorId}:${skill.code}:${Boolean(skill.isDot)}`;
              const timestamps = (skill.hitTimestamps || []).map((ts) => Number(ts) + offset);
              const entry = merged.get(key);
              if (!entry) {
                merged.set(key, { ...skill, hitTimestamps: timestamps, specs: [...(skill.specs || [])] });
                continue;
              }
              for (const name of sumFields) entry[name] = (Number(entry[name]) || 0) + (Number(skill[name]) || 0);
              const minimums = [entry.minDmg, skill.minDmg].map(Number).filter((n) => n > 0);
              entry.minDmg = minimums.length ? Math.min(...minimums) : 0;
              entry.maxDmg = Math.max(Number(entry.maxDmg) || 0, Number(skill.maxDmg) || 0);
              entry.hitTimestamps.push(...timestamps);
              entry.specs.push(...(skill.specs || []));
            }
          }
          return [...merged.values()];
        };
        result = {
          targetId: 0, maxHp: 0, startTime,
          battleTime: Number(dps.battleTime) || 0,
          totalTargetDamage: results.reduce((sum, item) => sum + (Number(item.totalTargetDamage) || 0), 0),
          skills: mergeSkills("skills"), healSkills: mergeSkills("healSkills"), pingHistory: [],
        };
      }
      const issue = Array.isArray(result?.skills) && result.skills.length
        ? "" : `empty response for targets=${targetIds.join(",")}`;
      if (issue && issue !== lastSkillDetailsIssue) window.javaBridge?.logToDebug?.(`Skill details: ${issue}`);
      lastSkillDetailsIssue = issue;
      return summaryOnly ? result : JSON.stringify(result);
    },

    getVersion() {
      return cachedAppVersion;
    },
  };

  // ===== window.javaBridge — called by various JS modules =====
  window.javaBridge = {
    // --- Details window (second monitor) ---
    listMonitors() {
      return invoke("list_monitors").catch(() => []);
    },
    openDetailsWindow(monitorIndex) {
      return invoke("open_details_window", { monitorIndex: Number(monitorIndex) || 0 });
    },
    closeDetailsWindow() {
      return invoke("close_details_window").catch(() => {});
    },
    // Details is one surface. The overlay never opens the panel inside itself —
    // it describes what to show and the standalone window (created on demand)
    // renders it. Rejections are surfaced so core.js can fall back in-overlay.
    requestDetailsView(payload) {
      return invoke("request_details_view", { payload: payload || {} });
    },
    takePendingDetailsRequest() {
      // No label argument — the backend reads it from the calling window, so a
      // window can only ever claim the request that was parked for it.
      return invoke("take_pending_details_request").catch(() => null);
    },
    closeToolWindow() {
      return invoke("close_tool_window").catch(() => {});
    },
    detailsWindowReady() {
      try {
        const w = window.__TAURI__.window.getCurrentWindow();
        w.show();
        w.setFocus();
      } catch (e) {
        console.error("[A2Tools] revealSelf failed", e);
      }
      return invoke("details_window_ready").catch(() => {});
    },
    openSettingsWindow() {
      return invoke("open_settings_window").catch((e) => console.error("[A2Tools] openSettingsWindow", e));
    },
    closeSettingsWindow() {
      return invoke("close_settings_window").catch(() => {});
    },
    toolWindowReady(label) {
      // Linux Settings loads hidden; notifyUiReady reveals it once the form is ready.
      if (loadsHidden && label === "settings") return Promise.resolve();
      // Reveal from the window's own webview thread. Calling show() on the Rust
      // side from a spawned task did not take effect — the window stayed created
      // but unmapped — so the window shows itself and the backend call is only
      // a fallback for focus.
      try {
        const w = window.__TAURI__.window.getCurrentWindow();
        w.show();
        w.setFocus();
      } catch (e) {
        console.error("[A2Tools] revealSelf failed", e);
      }
      return invoke("tool_window_ready", { label: String(label) }).catch(() => {});
    },
    async notifyUiReady() {
      // A visible startup or native fallback can precede core installation.
      if (viewMode === "settings" && settingsActive) applyPendingSettingsAccountResult();
      if (!loadsHidden) return;
      if (viewMode === "settings") {
        await settingsLifecycleReady;
        const generation = settingsLifecycleGeneration;
        // Lay out the translated form before mapping the native window.
        document.querySelector(".settingsPanel")?.getBoundingClientRect();
        // The backend shows it only if it is still wanted: a Close may have
        // come first, and then paused the form.
        const shown = await invoke("tool_window_ready", { label: "settings" }).catch(() => false);
        if (generation !== settingsLifecycleGeneration) return;
        if (shown) resumeSettings({ resync: settingsActive === false });
        else window.dispatchEvent(new Event("settings-hidden"));
        return;
      }
      if (viewMode !== "main") return;
      // Startup setters may already have requested this same size. Await the
      // actual native requests, including a newer size queued while waiting.
      for (let attempt = 0; attempt < 2; ++attempt) {
        let applied = await updateWindowSize();
        while (pendingWindowSize) applied = await pendingWindowSize.promise;
        if (!applied) continue;
        await invoke("main_window_ready").catch(() => {});
        return;
      }
    },

    // --- Settings ---
    getSetting(key) {
      return settingsCache[key] ?? localStorage.getItem(key);
    },
    setSetting(key, value) {
      // A full storage quota must not keep the setting from the backend.
      cacheSetting(key, value);
      invoke("update_settings", { key, value: String(value) }).catch(() => {});
      // Reload backend i18n data when language changes
      if (key === "dpsMeter.language") {
        invoke("set_language", { language: String(value) }).catch(() => {});
      }
    },
    clearAllSettings() {
      ++settingsReadId;
      ++settingsRevision;
      settingsKeyRevisions.clear();
      try { localStorage.clear(); } catch {}
      settingsCache = {};
      invoke("clear_settings").catch(() => {});
    },

    // --- DPS & Combat ---
    resetDps() {
      invoke("reset_combat").catch(() => {});
      cachedDpsJson = null;
      // Clear frontend state and skip the 1s grace period
      if (window._dpsApp) {
        window._dpsApp.refreshPending = false;
        window._dpsApp.lastJson = null;
        window._dpsApp.lastSnapshot = [];
        window._dpsApp._lastRenderedListSignature = "";
        window._dpsApp._lastRenderedRowsSummary = null;
        window._dpsApp._lastBattleTimeMs = null;
        window._dpsApp._battleTimeVisible = false;
        window._dpsApp.battleTime?.setVisible?.(false);
        window._dpsApp.meterUI?.onResetMeterUi?.();
      }
    },
    restartTargetSelection() {
      this.resetDps();
    },
    setTargetSelection(mode) {
      invoke("set_target_mode", { mode }).catch(() => {});
    },
    // `manual`: the player typed it, so the backend takes it even after the
    // game has named the character.
    setCharacterName(name, manual) {
      invoke("set_character_name", { name, manual: !!manual }).catch(() => {});
    },
    // `manual`: the player typed the id in Settings. Otherwise this is the
    // window echoing the id it last saw, which the backend ignores once the
    // game has named someone else.
    bindLocalActorId(actorId, manual) {
      const id = Number(actorId);
      if (!Number.isFinite(id) || id <= 0) return;
      // Always invoke — the backend is idempotent and needs to reapply the
      // permanent nickname if the character name was set after the initial bind.
      window._boundLocalActorId = id;
      invoke("bind_local_actor_id", { actorId: id, manual: !!manual }).catch(() => {});
      // Also bind nickname if we can find it from any source
      const name =
        window._dpsApp?.USER_NAME ||
        document.querySelector(".characterNameInput")?.value?.trim() ||
        "";
      if (name) {
        this.bindLocalNickname(actorId, name);
      }
      // Force immediate meter refresh so the name shows right away
      invoke("get_dps_snapshot").then((dps) => {
        cachedDpsJson = JSON.stringify(dps);
      }).catch(() => {});
    },
    setLocalPlayerId(actorId) {
      this.bindLocalActorId(actorId);
    },
    bindLocalNickname(actorId, nickname) {
      const id = Number(actorId);
      if (!Number.isFinite(id) || id <= 0 || !nickname) return;
      // Always invoke — backend handles idempotency and will refresh the
      // nickname even if the (id:nickname) pair was previously sent.
      window._boundLocalNickname = `${id}:${nickname}`;
      invoke("bind_local_nickname", { actorId: id, nickname }).catch(() => {});
    },
    setAllTargetsWindowMs() {},
    setTrainSelectionMode() {},

    // --- Window ---
    moveWindow() {
      // No-op — native drag handles window movement via start_drag command.
      // This also effectively disables core.js's bindDragToMoveWindow since
      // it checks `if (!window.javaBridge) return` on mousemove — the function
      // exists but does nothing, so the JS drag system runs but has no effect.
      // The ghost panel logic is tied to hasDragMoved which requires >3px of
      // mouse movement with isDragging=true. We prevent this below.
    },
    exitApp() {
      invoke("quit_app").catch(() => {});
    },

    // --- Browser ---
    openBrowser(url) {
      invoke("open_url", { url }).catch(() => {});
    },

    // --- Ping ---
    getPingMs() {
      return cachedPing;
    },

    // --- Connection Info ---
    getConnectionInfo() {
      return cachedCaptureStatus ? JSON.stringify(cachedCaptureStatus) : null;
    },
    getLastParsedAtMs() {
      return 0;
    },
    getAvailableDevices() {
      // Stale-while-revalidate: answer now, refresh an old list in the background.
      if (!window._cachedDevices || Date.now() - devicesLoadedAt > DEVICES_STALE_MS) {
        loadDevices().catch(() => {});
      }
      return JSON.stringify(window._cachedDevices || []);
    },
    // The same list, for a caller that can wait for it.
    loadAvailableDevices() {
      return loadDevices();
    },
    setManualDevice(device) {
      invoke("set_manual_device", { device: device || "" }).catch(() => {});
    },
    resetAutoDetection() {
      invoke("reset_auto_detection").catch(() => {});
    },

    // --- Screenshots ---
    // Screenshots. The backend measures against whichever window calls, so the
    // Details window captures (and saves) itself. Coordinates are the page's
    // CSS pixels; `scale` is devicePixelRatio, which the backend needs to map
    // them onto the screen at 125%/150% display scaling.
    // Resolves to { clipboard: bool, file: path | null }.
    captureScreenshot({ x, y, width, height, scale, includeMeter, saveFile, folder, filename }) {
      return invoke("capture_screenshot", {
        x, y, width, height,
        scale: scale || window.devicePixelRatio || 1,
        includeMeter: !!includeMeter,
        saveFile: !!saveFile,
        folder: folder || null,
        filename: filename || null,
      }).catch(() => ({ clipboard: false, file: null }));
    },
    captureScreenshotToClipboard(x, y, w, h, scale) {
      this.captureScreenshot({ x, y, width: w, height: h, scale });
      return true;
    },
    // Resolves to the chosen folder, or null if the player cancels.
    chooseScreenshotFolder(current) {
      return invoke("choose_screenshot_folder", { current: current || null }).catch(() => null);
    },
    getDefaultScreenshotFolder() {
      return window._defaultScreenshotFolder || "";
    },

    // --- Hotkeys ---
    getCurrentHotKey() {
      return this.getSetting("dpsMeter.hotkey") || "Ctrl+Alt+Shift+R";
    },
    getCurrentToggleWindowHotKey() {
      return this.getSetting("dpsMeter.toggleWindowHotkey") || "Ctrl+Alt+Up";
    },
    getCurrentLockHotKey() {
      return this.getSetting("dpsMeter.lockHotkey") || "Ctrl+Alt+L";
    },
    setLockHotkey(mods, vk) {
      this.setSetting("dpsMeter.lockHotkey", this._buildHotkeyLabel(mods, vk));
    },
    // The click-through lock (OverlayLock in app.rs). A promise: whether the
    // backend can keep the lock button clickable here.
    overlayLockSupported() {
      return invoke("overlay_lock_supported").catch(() => false);
    },
    setOverlayLocked(locked) {
      invoke("set_overlay_locked", { locked: !!locked }).catch(() => {});
    },
    setLockButtonRect(x, y, width, height, scale) {
      invoke("set_lock_button_rect", { x, y, width, height, scale }).catch(() => {});
    },
    setHotkey(mods, vk) {
      const label = this._buildHotkeyLabel(mods, vk);
      this.setSetting("dpsMeter.hotkey", label);
    },
    setToggleWindowHotkey(mods, vk) {
      const label = this._buildHotkeyLabel(mods, vk);
      this.setSetting("dpsMeter.toggleWindowHotkey", label);
    },
    _buildHotkeyLabel(mods, vk) {
      const parts = [];
      if (mods & 0x02) parts.push("Ctrl");
      if (mods & 0x01) parts.push("Alt");
      if (mods & 0x04) parts.push("Shift");
      // Map common VK codes to names
      const vkNames = {
        0x08: "Backspace", 0x09: "Tab", 0x0D: "Enter", 0x1B: "Esc",
        0x20: "Space", 0x21: "PageUp", 0x22: "PageDown", 0x23: "End",
        0x24: "Home", 0x25: "Left", 0x26: "Up", 0x27: "Right", 0x28: "Down",
        0x2D: "Insert", 0x2E: "Delete",
        0x70: "F1", 0x71: "F2", 0x72: "F3", 0x73: "F4", 0x74: "F5",
        0x75: "F6", 0x76: "F7", 0x77: "F8", 0x78: "F9", 0x79: "F10",
        0x7A: "F11", 0x7B: "F12",
      };
      const keyName = vkNames[vk] || String.fromCharCode(vk);
      parts.push(keyName);
      return parts.join("+");
    },

    // --- Feature flags ---
    isRunningFromIde() { return false; },
    getParsingBacklog() { return 0; },
    // The header's suspend button. The backend owns the switch; this keeps a
    // copy so the UI can read it synchronously, as it does at load.
    isCaptureSuspended() { return captureSuspended; },
    suspendCapture(suspended) {
      captureSuspended = !!suspended;
      invoke("suspend_capture", { suspended: captureSuspended }).catch(() => {});
    },
    setBossLogsEnabled() {},
    setAutoHideMeter(enabled) {
      invoke("update_settings", { key: "dpsMeter.autoHideMeter", value: String(enabled) }).catch(() => {});
    },
    setSaveRawPackets(enabled) {
      invoke("set_packet_logging", { enabled: !!enabled }).catch(() => {});
    },
    // Sends the newest packet captures to the developer. Resolves with
    // { code, files }; rejects with a message to show as is.
    sendLogsToDev() {
      return invoke("send_logs_to_dev");
    },
    setDebugLoggingEnabled(enabled) {
      invoke("set_debug_logging", { enabled: !!enabled }).catch(() => {});
    },
    getAion2WindowTitle() { return window._cachedAion2Title ?? null; },
    logDebug() {},

    getFightHistory() {
      // Trigger async refresh for next call
      invoke("get_fight_history").then((h) => { window._cachedFightHistory = h; }).catch(() => {});
      if (window._cachedFightHistory) {
        return JSON.stringify(window._cachedFightHistory);
      }
      // First call: block briefly with synchronous fallback
      return "[]";
    },

    // Await the cache actually being filled. getFightHistory() is synchronous
    // and answers from window._cachedFightHistory, which a prefetch populates
    // asynchronously — so a window created in order to show History renders
    // before its own prefetch lands and gets an empty list.
    refreshFightHistory() {
      return invoke("get_fight_history")
        .then((h) => { window._cachedFightHistory = h; return true; })
        .catch(() => false);
    },

    getFightDetails(id) {
      // Async — returns a promise
      return invoke("load_fight", { id }).then((r) => JSON.stringify(r)).catch(() => null);
    },

    accountStatus() {
      return invoke("account_status");
    },

    discordActivityAvailable() {
      return invoke("discord_activity_available");
    },

    // The stream overlay for OBS on another PC, as
    // { enabled, running, port, urls, error }. Configure and New key resolve
    // with the status after the change; configure rejects a bad port.
    streamOverlayStatus() {
      return invoke("stream_overlay_status");
    },
    streamOverlayConfigure(enabled, port) {
      return invoke("stream_overlay_configure", { enabled: !!enabled, port: Number(port) });
    },
    streamOverlayNewKey() {
      return invoke("stream_overlay_new_key");
    },

    // The last check's answer, at once: null if none has run yet, else
    // { who } with who null when signed out.
    accountStatusCached() {
      return invoke("account_status_cached").then((seen) =>
        seen === null || seen === undefined ? null : { who: seen }
      );
    },

    // Starts the device grant and opens the browser. Resolves with the code to
    // show; completion arrives later on the "account-changed" event.
    accountBeginLink() {
      return invoke("account_begin_link");
    },

    accountSignOut() {
      return invoke("account_sign_out").catch(() => {});
    },

    // Writes the two files an upload would send and returns a summary.
    // Deliberately does NOT upload — see docs/PRIVACY.md and the
    // `preview_share` command. Rejects with a message the UI shows verbatim.
    previewShare(id) {
      return invoke("preview_share", { fightId: id });
    },

    // Uploads the fight's Evidence Slice to a2tools.app and resolves with
    // { url, visibility, duplicate }. Rejects with a message to show as is.
    uploadFight(id) {
      return invoke("upload_fight", { fightId: id });
    },

    // { fightId: { hasSlice, url } } for every fight that can be, or was, uploaded.
    shareStatus() {
      return invoke("share_status").catch(() => ({}));
    },

    deleteFight(id) {
      invoke("delete_fight", { id }).catch(() => {});
      return true;
    },

    // --- Resources ---
    readResource(path) {
      // Load resource files synchronously via XMLHttpRequest.
      // Try multiple path prefixes since files may be at root or under /src/data/.
      const candidates = [path, "/src/data" + path, "/src" + path];
      for (const url of candidates) {
        try {
          const xhr = new XMLHttpRequest();
          xhr.open("GET", url, false); // synchronous
          xhr.send();
          if (xhr.status === 200 && xhr.responseText) {
            return xhr.responseText;
          }
        } catch {
          // try next
        }
      }
      return null;
    },
    readCachedIcon(key) {
      // Synchronous read from Rust via cached map
      if (!key) return null;
      if (window._iconCache?.[key] !== undefined) return window._iconCache[key];
      // Trigger async load for next call
      invoke("read_cached_icon", { key }).then((data) => {
        if (!window._iconCache) window._iconCache = {};
        window._iconCache[key] = data ?? null;
      }).catch(() => {});
      return null;
    },
    // A line in debug.log, for problems only the UI sees.
    logToDebug(message) {
      invoke("log_from_ui", { message: String(message) }).catch(() => {});
    },
    writeCachedIcon(key, data) {
      if (!key || !data) return;
      if (!window._iconCache) window._iconCache = {};
      window._iconCache[key] = data;
      invoke("write_cached_icon", { key, data }).catch(() => {});
    },

    // --- Fetch ---
    fetchUrlAsync(url, callbackId) {
      // checkRelease.js registers a callback via window._fetchUrlCallback(id, raw)
      if (window.a2Build?.online === false) {
        if (callbackId && typeof window._fetchUrlCallback === "function") {
          window._fetchUrlCallback(callbackId, JSON.stringify({ error: "not in this build" }));
        }
        return;
      }
      // Add cache-buster and no-cache headers to avoid stale CDN responses
      const bustUrl = url + (url.includes("?") ? "&" : "?") + "_t=" + Date.now();
      fetch(bustUrl, { cache: "no-store" })
        .then((r) => r.text())
        .then((text) => {
          if (callbackId && typeof window._fetchUrlCallback === "function") {
            window._fetchUrlCallback(callbackId, text);
          }
        })
        .catch(() => {
          if (callbackId && typeof window._fetchUrlCallback === "function") {
            window._fetchUrlCallback(callbackId, JSON.stringify({ error: "fetch failed" }));
          }
        });
    },

    // --- Admin ---
    isAdmin() {
      return invoke("is_admin");
    },
  };


  // Poll AION2 window title and capture status from Rust backend
  const pollStatus = () => {
    const title = invoke("get_aion2_window_title")
      .then((title) => { window._cachedAion2Title = title ?? null; })
      .catch(() => { window._cachedAion2Title = null; });

    const status = invoke("get_capture_status")
      .then((status) => { cachedCaptureStatus = status; })
      .catch(() => {});
    // Settings has no title poll of its own; this one runs only while it is shown.
    if (viewMode === "settings") Promise.all([title, status]).then(() => {
      if (settingsActive) window._dpsApp?.refreshSettingsStatus?.();
    });
  };
  let statusTimer = null;
  const startStatusPolling = () => {
    if (statusTimer !== null) return;
    pollStatus();
    statusTimer = setInterval(pollStatus, 3000);
  };
  // A repeat without a show in between is ignored.
  if (viewMode === "settings") window.addEventListener("settings-hidden", () => {
    ++settingsLifecycleGeneration;
    window._dpsApp?.invalidateStreamOverlaySettings?.();
    if (settingsActive === false) return;
    settingsActive = false;
    clearInterval(statusTimer);
    statusTimer = null;
    document.activeElement?.blur?.();
    window._dpsApp?.closeSupportModal?.();
    document.querySelectorAll(".settingsDropdownMenu.isOpen").forEach(menu => menu.classList.remove("isOpen"));
  });
  if (viewMode !== "settings" || settingsActive) startStatusPolling();

  // ===== Dynamic window resizing =====
  const PANEL_WIDTH = 1540;
  const PANEL_HEIGHT = 820;
  const PROMO_WIDTH = 400;
  const PROMO_HEIGHT = 480;
  let lastSizeKey = "";
  let pendingWindowSize = null;
  let resizeActive = false;
  let nativeResize = null;
  let primaryHeld = false;

  const overlayPadding = () => ({ w: 16, h: 10 });

  const overlayMinimum = (meter) => {
    const height = meter.style.height;
    const minHeight = meter.style.minHeight;
    // Measure normal-flow content without the user's saved empty space.
    meter.style.height = "auto";
    meter.style.minHeight = "0";
    const style = getComputedStyle(meter);
    const contentHeight = Math.max(30, parseFloat(style.height) || 0);
    const outerHeight = Math.ceil(meter.getBoundingClientRect().height);
    const borderWidth = style.boxSizing === "border-box" ? 0
      : (parseFloat(style.borderLeftWidth) || 0) + (parseFloat(style.borderRightWidth) || 0)
        + (parseFloat(style.paddingLeft) || 0) + (parseFloat(style.paddingRight) || 0);
    meter.style.height = height;
    meter.style.minHeight = minHeight;
    const padding = overlayPadding();
    return { contentHeight, width: 300 + borderWidth + padding.w, height: outerHeight + padding.h };
  };

  // Follow the compositor viewport while automatic content sizing is paused.
  const followNativeSize = () => {
    if (!nativeResize || window.A2_VIEW !== "main") return;
    const meter = document.querySelector(".meter");
    if (!meter) return;
    const padding = overlayPadding();
    const style = getComputedStyle(meter);
    const px = (name) => parseFloat(style[name]) || 0;
    const borderW = style.boxSizing === "border-box" ? 0
      : px("borderLeftWidth") + px("borderRightWidth") + px("paddingLeft") + px("paddingRight");
    const borderH = style.boxSizing === "border-box" ? 0
      : px("borderTopWidth") + px("borderBottomWidth") + px("paddingTop") + px("paddingBottom");
    // Viewport units follow layout immediately, without waiting for resize events.
    meter.style.width = `max(300px, calc(100vw - ${padding.w + borderW}px))`;
    meter.style.height = `max(${nativeResize.contentHeight || 30}px, calc(100vh - ${padding.h + borderH}px))`;
  };

  const finishNativeResize = (cancel = false) => {
    const operation = nativeResize;
    if (!operation) return Promise.resolve(true);
    operation.cancel = operation.cancel || cancel;
    if (operation.finished) return operation.finished;
    operation.finished = (async () => {
      // A release may arrive before the start IPC settles.
      await operation.started;
      // Consume the compositor's final configure before pinning the size again.
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      followNativeSize();
      let finished = false;
      try {
        const cancelRequested = operation.cancel;
        finished = await invoke("finish_window_resize", { cancel: cancelRequested });
        // Preserve cancellation requested during the pointer-return check.
        if (!finished && operation.cancel && !cancelRequested) {
          finished = await invoke("finish_window_resize", { cancel: true });
        }
      } catch (error) {
        console.error("[A2Tools] finishing native resize failed", error);
      }
      if (!finished) {
        // WebKit can emit hover events during a grab; GTK must confirm release.
        operation.finished = null;
        return false;
      }
      if (window.A2_VIEW === "main") {
        const meter = document.querySelector(".meter");
        if (meter?.style.height) {
          const style = getComputedStyle(meter);
          meter.style.width = style.width;
          meter.style.minHeight = style.height;
          meter.style.height = "";
        }
      }
      nativeResize = null;
      resizeActive = false;
      lastSizeKey = "";
      return true;
    })();
    return operation.finished;
  };

  const startNativeResize = async (direction, minWidth, minHeight) => {
    if (nativeResize && !await finishNativeResize(true)) return;
    if (!primaryHeld) return;
    resizeActive = true;
    const operation = { finished: null, started: null, cancel: false };
    nativeResize = operation;
    if (window.A2_VIEW === "main") {
      const meter = document.querySelector(".meter");
      if (meter) {
        const minimum = overlayMinimum(meter);
        operation.contentHeight = minimum.contentHeight;
        minWidth = minimum.width;
        minHeight = minimum.height;
        followNativeSize();
        meter.style.minHeight = `${minimum.contentHeight}px`;
      }
    }
    operation.started = invoke("begin_window_resize", {
      minWidth, minHeight, scale: window.devicePixelRatio || 1,
    }).then((held) => {
      if (held) return window.__TAURI__.window.getCurrentWindow().startResizeDragging(direction);
      queueMicrotask(() => finishNativeResize(true));
    }).catch((error) => {
      console.error("[A2Tools] native window resize failed", error);
      // Run after this promise settles, so finishing cannot await itself.
      queueMicrotask(() => finishNativeResize(true));
    });
  };

  // GTK3 hides the compositor resize state. End on returned pointer input,
  // not a pause in motion; GTK rejects synthetic WebKit hover events.
  const finishOnPointerReturn = (event) => {
    const inside = event.clientX >= 0 && event.clientY >= 0
      && event.clientX < window.innerWidth && event.clientY < window.innerHeight;
    if ((event.buttons & 1) === 0 && inside) {
      if (!nativeResize) primaryHeld = false;
      finishNativeResize();
    }
  };
  document.addEventListener("pointermove", finishOnPointerReturn, { capture: true });
  document.addEventListener("pointerover", finishOnPointerReturn, { capture: true });
  document.addEventListener("mouseup", (event) => {
    if (event.button !== 0) return;
    primaryHeld = false;
    finishNativeResize();
  }, { capture: true });
  document.addEventListener("mousedown", (event) => {
    primaryHeld = event.button === 0;
    finishNativeResize(true);
  }, { capture: true });

  // The screen space right of and below the window. The overlay grows from its
  // top-left corner, and growing past the screen edge makes a window manager
  // that keeps windows on screen (KWin) move it back, out from under the
  // cursor: the tooltip hid, the window shrank, the row was under the cursor
  // again, in a loop (issue #11). Windows simply lets it hang off screen.
  const spaceRightBelow = () => {
    const s = window.screen;
    const right = (s.availLeft || 0) + (s.availWidth || 1920);
    const bottom = (s.availTop || 0) + (s.availHeight || 1080);
    return {
      w: Math.max(0, right - (window.screenX || 0)),
      h: Math.max(0, bottom - (window.screenY || 0)),
    };
  };

  // Crossing the gap between rows hides the tooltip for a moment. Keep its area
  // briefly so the native window does not shrink and grow back on every row.
  const TOOLTIP_RELEASE_MS = 250;
  let tooltipReserve = null;
  let tooltipReleaseTimer = 0;
  const updateWindowSize = () => {
    if (resizeActive) return Promise.resolve(false); // Don't fight the user while they're resizing
    // Tool windows own their own geometry (and remember it). The overlay's
    // auto-sizing would otherwise shrink them to meter dimensions.
    if (window.A2_VIEW !== "main") return;

    const fullPanel = !!(
      document.querySelector(".settingsPanel.isOpen") ||
      document.querySelector(".detailsPanel.open") ||
      document.querySelector(".historyPanel.isOpen") ||
      document.querySelector(".historyPanel.open")
    );
    const tooltip = fullPanel ? null : document.querySelector(".hoverDetailsTooltip.isVisible");
    // The one-time Discord popup needs room for its card, no more: a
    // full-panel window would block clicks on the game around it.
    const promoOpen = !fullPanel && !!document.querySelector(".discordPromo.isOpen");

    // Measure meter width (may be resized by user via drag handle) and height
    const meter = document.querySelector(".meter");
    let contentW = 396;
    let contentH = 300;
    if (meter) {
      contentW = Math.ceil(meter.offsetWidth) + 16;
      const meterH = Math.max(meter.offsetHeight, meter.scrollHeight);
      // The ping sits inside the footer row, so it is part of offsetHeight.
      contentH = Math.ceil(meterH) + 10;
    }

    // The tooltip's extra room stops at the screen edge; the meter itself
    // never shrinks below its content.
    const tooltipBounds = tooltip?.getBoundingClientRect();
    // Reserve the tooltip's travel over the rows once. Resizing the native
    // window for every pointer position can stall WebKit/GTK and the compositor.
    const listBounds = tooltip ? document.querySelector(".list")?.getBoundingClientRect() : null;
    const room = spaceRightBelow();
    const tooltipW = tooltipBounds
      ? Math.ceil(Math.max(tooltipBounds.right, listBounds ? listBounds.right + 12 + tooltipBounds.width : 0)) + 8
      : contentW;
    const tooltipH = tooltipBounds
      ? Math.ceil(Math.max(tooltipBounds.bottom, listBounds ? listBounds.bottom + 12 + tooltipBounds.height : 0)) + 8
      : contentH;
    if (tooltip) {
      clearTimeout(tooltipReleaseTimer);
      tooltipReleaseTimer = 0;
      // Within one hover the area only grows, so rows with different tooltip
      // sizes do not resize the window back and forth.
      tooltipReserve = {
        w: Math.max(tooltipW, tooltipReserve?.w || 0),
        h: Math.max(tooltipH, tooltipReserve?.h || 0),
      };
    } else if (fullPanel) {
      clearTimeout(tooltipReleaseTimer);
      tooltipReleaseTimer = 0;
      tooltipReserve = null;
    } else if (tooltipReserve && !tooltipReleaseTimer) {
      tooltipReleaseTimer = setTimeout(() => {
        tooltipReleaseTimer = 0;
        tooltipReserve = null;
        updateWindowSize();
      }, TOOLTIP_RELEASE_MS);
    }
    const w = fullPanel
      ? PANEL_WIDTH
      : promoOpen
        ? Math.max(contentW, PROMO_WIDTH)
        : tooltipReserve
          ? Math.max(contentW, Math.min(tooltipReserve.w, room.w))
          : contentW;
    const h = fullPanel
      ? Math.max(PANEL_HEIGHT, contentH)
      : promoOpen
        ? Math.max(contentH, PROMO_HEIGHT)
        : tooltipReserve
          ? Math.max(contentH, Math.min(tooltipReserve.h, room.h))
          : contentH;
    const scale = window.devicePixelRatio || 1;
    const sizeKey = `${w}x${h}@${scale}`;
    if (sizeKey === lastSizeKey) return pendingWindowSize?.promise || Promise.resolve(true);
    lastSizeKey = sizeKey;
    // The page's devicePixelRatio, so the backend sizes the window in the
    // pixels the page is actually drawn at (Windows text size included).
    const operation = { promise: null };
    // Separate IPC fetches can arrive out of order. Queue size changes so the
    // latest dimensions reach the native window last, and share an in-flight
    // same-size request with notifyUiReady instead of treating it as applied.
    operation.promise = Promise.resolve(pendingWindowSize?.promise)
      .then(() => {
        // A queued auto-size must not interrupt a resize the player began.
        if (resizeActive) return false;
        return invoke("resize_window", { width: w, height: h, scale }).then(() => true);
      })
      .then((applied) => {
        if (!applied && pendingWindowSize === operation) lastSizeKey = "";
        return applied;
      }, () => {
        if (pendingWindowSize === operation) lastSizeKey = "";
        return false;
      })
      .finally(() => {
        if (pendingWindowSize === operation) pendingWindowSize = null;
      });
    pendingWindowSize = operation;
    return operation.promise;
  };

  window.javaBridge.updateOverlaySize = updateWindowSize;

  // Watch all class changes on the container to catch panel open/close instantly
  const containerObserver = new MutationObserver(() => updateWindowSize());
  const startObserving = () => {
    const container = document.querySelector(".container");
    if (container) {
      containerObserver.observe(container, {
        attributes: true,
        attributeFilter: ["class"],
        subtree: true,
      });
    }
  };
  if (viewMode === "main" && document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", startObserving);
  } else if (viewMode === "main") {
    startObserving();
  }
  // Fallback poll
  if (viewMode === "main") setInterval(updateWindowSize, 500);

  // Force resize when window is restored from auto-hide
  // (content may have changed while minimized, stale lastSizeKey would skip resize)
  listen("force-resize", () => {
    lastSizeKey = "";
    updateWindowSize();
  });

  // The meter has no drag and drop. A press that lands on row text or an icon
  // would otherwise start a native drag of it, and under XWayland a drag the
  // meter never finishes holds the pointer (a no-drop cursor with the text
  // stuck to it) until it times out, about a minute, even after the meter quits.
  document.addEventListener("dragstart", (e) => e.preventDefault(), { capture: true });

  // ===== Window dragging =====
  // Core.js's JS-based drag (moveWindow + screenX/Y) is too slow over IPC.
  // Use native Win32 drag via WM_NCLBUTTONDOWN — instant, OS-handled, zero latency.
  // Intercept mousedown on .meter in capture phase before core.js sees it.
  // Intercept mousedown to:
  // 1. Start native drag on header/footer/empty meter areas
  // 2. Block core.js's JS drag system everywhere (it doesn't work in Tauri)
  // 3. Let clicks on interactive elements (.item, buttons, panels) pass through
  document.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    const target = e.target?.nodeType === Node.TEXT_NODE ? e.target.parentElement : e.target;
    // Let interactive elements handle their own clicks normally
    if (target?.closest?.("button, input, select, textarea, a, [data-no-drag]")) return;
    if (target?.closest?.(".headerBtn, .footerBtn, .bossIcon, .resizeHandle")) return;
    // Let panel internals work (close buttons, dropdowns, etc.)
    if (target?.closest?.(".settingsPanel, .historyPanel, .detailsBody, .detailsHeader, .detailsSettingsMenu")) return;
    // Let meter bar item clicks pass through for details/hover
    if (target?.closest?.(".item")) return;
    // Everything else in .meter: native drag
    if (target?.closest?.(".meter")) {
      e.stopImmediatePropagation();
      invoke("start_drag");
    }
  }, { capture: true });

  // ===== Tool windows: drag by the header; on Linux, resize from the edges =====
  // The tool windows are frameless, so the page starts the drag itself, through
  // start_tool_drag. Not -webkit-app-region: WebView2 moves a helper window
  // with SetWindowPos whenever the draggable area changes, and that call waits
  // on other windows (the game's among them), so Settings froze the whole app
  // for seconds at a time (2026-10-08). WebKitGTK ignores app-region anyway.
  // On Windows the window manager resizes them by their border; on Linux a
  // frameless window has no border to grab, so the page also starts the resize
  // through begin_tool_resize, which lifts the pinned size hints
  // (platform::window::set_size) for the length of the resize.
  const DRAG_HEADERS = ".historyHeader, .detailsHeader, .settingsHeader";
  const NO_DRAG = "button, a, input, select, textarea, [data-no-drag], "
    + ".historyViewToggle, .historyFilters, .historyClose, .detailsModeToggle, "
    + ".detailsSettingsMenuWrapper, .detailsScreenshotWrapper, .detailsWindowClose, .closeX";
  const dragFromHeader = (e) => {
    const target = e.target?.nodeType === Node.TEXT_NODE ? e.target.parentElement : e.target;
    if (!target?.closest?.(DRAG_HEADERS) || target.closest(NO_DRAG)) return false;
    e.preventDefault();
    e.stopImmediatePropagation();
    invoke("start_tool_drag").catch(() => {});
    return true;
  };
  if (window.A2_VIEW !== "main" && !isLinux) {
    document.addEventListener("mousedown", (e) => {
      if (e.button === 0) dragFromHeader(e);
    }, { capture: true });
  }
  if (window.A2_VIEW !== "main" && isLinux) {
    const MIN_SIZE = { settings: [520, 420], history: [480, 360], details: [520, 360] };
    const [minW, minH] = MIN_SIZE[window.A2_VIEW] || [480, 360];
    const EDGE = 6;

    const style = document.createElement("style");
    style.textContent = [
      ["n", "ns"], ["s", "ns"], ["e", "ew"], ["w", "ew"],
      ["ne", "nesw"], ["sw", "nesw"], ["nw", "nwse"], ["se", "nwse"],
    ].map(([edge, cur]) => `html.toolEdge-${edge}, html.toolEdge-${edge} * { cursor: ${cur}-resize !important; }`).join("\n");
    document.head.appendChild(style);

    const edgeAt = (e) => {
      let edge = "";
      if (e.clientY < EDGE) edge += "n";
      else if (e.clientY >= window.innerHeight - EDGE) edge += "s";
      if (e.clientX < EDGE) edge += "w";
      else if (e.clientX >= window.innerWidth - EDGE) edge += "e";
      return edge;
    };
    let shownEdge = "";
    const showEdge = (edge) => {
      if (edge === shownEdge) return;
      if (shownEdge) document.documentElement.classList.remove(`toolEdge-${shownEdge}`);
      if (edge) document.documentElement.classList.add(`toolEdge-${edge}`);
      shownEdge = edge;
    };

    const DIRECTION = {
      n: "North", s: "South", e: "East", w: "West",
      ne: "NorthEast", nw: "NorthWest", se: "SouthEast", sw: "SouthWest",
    };

    document.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      const edge = edgeAt(e);
      if (edge) {
        e.preventDefault();
        e.stopImmediatePropagation();
        // The backend unpins the size first, then the window manager resizes.
        compositorResizeReady.then((supported) => {
          if (supported) {
            startNativeResize(DIRECTION[edge], minW, minH);
          } else {
            return invoke("begin_tool_resize", { minWidth: minW, minHeight: minH })
              .then(() => window.__TAURI__.window.getCurrentWindow().startResizeDragging(DIRECTION[edge]));
          }
        })
          .catch((err) => console.error("[A2Tools] tool window resize failed", err));
        return;
      }
      dragFromHeader(e);
    }, { capture: true });

    document.addEventListener("mousemove", (e) => showEdge(e.buttons ? "" : edgeAt(e)), { capture: true });
  }

  // Pre-fetch device list and fight history so they're ready when panels open
  if (viewMode !== "settings") {
    loadDevices().catch(() => {});
    invoke("get_fight_history").then((h) => { window._cachedFightHistory = h; }).catch(() => {});
  }
  if (viewMode !== "settings") invoke("default_screenshot_folder")
    .then((f) => {
      window._defaultScreenshotFolder = f || "";
      window._dpsApp?.updateScreenshotFolderDisplay?.();
    })
    .catch(() => {});
  // Refresh fight history periodically (picks up auto-saved fights)
  if (viewMode !== "settings") setInterval(() => {
    invoke("get_fight_history").then((h) => { window._cachedFightHistory = h; }).catch(() => {});
  }, 10000);

  // ===== Overlay resize handle =====
  const expandViewport = () => {
    resizeActive = true;
    const space = spaceRightBelow();
    invoke("resize_window", { width: Math.min(space.w, 2000), height: Math.min(space.h, 1200), scale: window.devicePixelRatio || 1 }).catch(() => {});
  };
  const shrinkViewport = () => {
    if (nativeResize) return;
    if (resizeActive) {
      resizeActive = false;
      lastSizeKey = "";
    }
  };
  document.addEventListener("mousedown", (e) => {
    if (e.button !== 0 || !e.target?.closest?.(".resizeHandle")) return;
    if (compositorResize === false) {
      expandViewport();
      return;
    }
    e.preventDefault();
    e.stopImmediatePropagation();
    compositorResizeReady.then((supported) => {
      if (!primaryHeld) return;
      if (supported) {
        const padding = overlayPadding();
        startNativeResize("SouthEast", 300 + padding.w, 30 + padding.h);
      } else {
        // Replay an early press once backend detection has completed.
        e.target.dispatchEvent(new MouseEvent("mousedown", e));
      }
    });
  }, { capture: true });
  document.addEventListener("mouseup", shrinkViewport);
  // A release outside the window sends no mouseup; the next move shows it.
  // (Not on blur: KWin blurs the window at the start of every drag.)
  // Only a move inside the window is trusted (see bindResizeHandle in core.js).
  document.addEventListener("mousemove", (e) => {
    const inside = e.clientX >= 0 && e.clientY >= 0
      && e.clientX < window.innerWidth && e.clientY < window.innerHeight;
    if (resizeActive && (e.buttons & 1) === 0 && inside) shrinkViewport();
  });

  // Startup diagnostics
  invoke("debug_status").then((s) => {
    console.log("[A2Tools] Debug status:", JSON.stringify(s));
    if (!s.isAdmin) {
      console.warn("[A2Tools] NOT RUNNING AS ADMIN — packet capture will not work!");
    }
  }).catch((e) => console.error("[A2Tools] debug_status failed:", e));

  console.log("[A2Tools] Tauri bridge adapter loaded (javaBridge + dpsData)");
})();
