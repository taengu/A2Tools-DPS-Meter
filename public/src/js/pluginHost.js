// A plugin window's host page (src-tauri/src/plugins): its title bar and
// panel around the plugin's frame (pluginFrame.js, which the dock uses too).
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const id = window.__A2_PLUGIN_ID__;
  const stage = document.querySelector(".pluginStage");
  const toast = document.querySelector(".pluginToast");

  let toastTimer = 0;
  const showToast = (text) => {
    toast.textContent = text.slice(0, 200);
    toast.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toast.hidden = true; }, 4000);
  };

  // The Settings colours (#rrggbb each): this page's panel and bar take
  // them, and the plugin hears them as "theme" (a2.js styles its controls).
  const applyTheme = (theme) => {
    if (!theme) return;
    const rgb = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)).join(", ");
    const root = document.documentElement.style;
    root.setProperty("--theme-bg", rgb(theme.bg));
    root.setProperty("--theme-panel", rgb(theme.panel));
    root.setProperty("--theme-accent", theme.accent);
  };

  // Panel preferences, per plugin, kept by this page: background opacity, and
  // whether the plugin's controls are folded away ("compact", which the plugin
  // hears as an event and handles itself).
  const PREFS_KEY = `a2plugin.panel.${id}`;
  let panel = { opacity: 100, compact: false };
  try { panel = { ...panel, ...JSON.parse(localStorage.getItem(PREFS_KEY) || "{}") }; } catch {}
  const savePanel = () => { try { localStorage.setItem(PREFS_KEY, JSON.stringify(panel)); } catch {} };

  const plugin = window.createPluginFrame({
    id,
    stage,
    compact: panel.compact,
    toast: showToast,
    // Fitted to an alert: the title bar hidden meanwhile (pluginHost.css).
    onFit: (fitted) => document.body.classList.toggle("pluginFitted", fitted),
  });

  // The window is frameless: its bar moves it, and the button closes it.
  document.querySelector(".pluginBar").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || e.target.closest("button, input")) return;
    e.preventDefault();
    invoke("start_tool_drag").catch(() => {});
  });
  document.querySelector(".pluginClose").addEventListener("click", () => {
    // Hidden: the plugin keeps running (its widgets and alerts) until it is
    // turned off in Settings, where Open shows it again.
    invoke("plugin_close").catch(() => {});
  });
  // Into the plugin dock, with the other plugins (plugins/dock.rs).
  document.querySelector(".pluginDock")?.addEventListener("click", () => {
    invoke("plugin_set_docked", { id, docked: true }).catch(() => {});
  });

  // The DPS meter itself, for players who use only plugins: show or hide it
  // (kept across restarts), and Settings, which is otherwise on the meter.
  const meterBtn = document.querySelector(".pluginMeter");
  const showMeterState = (shown) => {
    meterBtn.setAttribute("aria-pressed", String(shown));
    const label = shown ? "Hide the DPS meter" : "Show the DPS meter";
    meterBtn.setAttribute("aria-label", label);
    meterBtn.title = label;
  };
  invoke("meter_shown").then((shown) => showMeterState(shown !== false)).catch(() => {});
  listen("meter-shown", (event) => showMeterState(event.payload !== false));
  meterBtn.addEventListener("click", () => {
    invoke("set_meter_shown", { shown: meterBtn.getAttribute("aria-pressed") !== "true", toggle: true }).catch(() => {});
  });
  document.querySelector(".pluginSettings").addEventListener("click", () => {
    invoke("open_settings_window").catch(() => {});
  });

  const opacity = document.querySelector(".pluginOpacity");
  const compact = document.querySelector(".pluginCompact");
  const applyOpacity = () => document.body.style.setProperty("--panel-alpha", String(panel.opacity / 100));
  const applyCompact = () => {
    compact.setAttribute("aria-pressed", String(panel.compact));
    const label = panel.compact ? "Show the controls" : "Hide the controls";
    compact.setAttribute("aria-label", label);
    compact.title = label;
  };
  opacity.value = String(panel.opacity);
  applyOpacity();
  applyCompact();
  opacity.addEventListener("input", () => {
    panel.opacity = Number(opacity.value) || 100;
    applyOpacity();
    savePanel();
  });
  compact.addEventListener("click", () => {
    panel.compact = !panel.compact;
    applyCompact();
    savePanel();
    plugin.setCompact(panel.compact);
  });

  const onEvent = (payload) => {
    payload ||= {};
    if (typeof payload.event !== "string") return;
    // `listen` hears the event whichever window it was sent to, so each plugin
    // window heard every plugin's, and another plugin's decoder of the same
    // name passed the permission check: with a copy of the Pet Soul Tracker
    // open beside it, every soul counted twice in both.
    if (payload.plugin !== undefined && payload.plugin !== id) return;
    if (payload.event === "theme") applyTheme(payload.data);
    plugin.receive(payload);
  };
  listen("plugin-event", (event) => onEvent(event.payload));
  // The meter's numbered events, several at once (a burst sent one by one
  // took the page past 1 GB): each as a "plugin-event".
  listen("plugin-events", (event) => {
    if (Array.isArray(event.payload)) for (const e of event.payload) onEvent(e);
  });

  plugin.start().then((hello) => {
    const manifest = hello.manifest;
    document.title = manifest.name;
    document.querySelector(".pluginName").textContent = `${manifest.name} ${manifest.version}`;
    applyTheme(hello.theme);
  }).catch((e) => {
    stage.textContent = `This plugin could not start: ${e}`;
  });
})();
