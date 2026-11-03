// The plugin dock's page (src-tauri/src/plugins/dock.rs): the docked plugins
// as tabs, or stacked, each in its own sandboxed frame (pluginFrame.js), as
// in a window of its own. The meter tells this page which plugins are docked
// ("dock-layout") and which tab to open ("dock-focus"); the page keeps the
// tabs' order, the open tab, the layout, the fold and the opacity.
(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const tabsEl = document.querySelector(".dockTabs");
  const stage = document.querySelector(".dockStage");
  const toast = document.querySelector(".pluginToast");
  const template = document.querySelector(".dockPanelTemplate");
  const layoutBtn = document.querySelector(".dockLayout");
  const foldBtn = document.querySelector(".dockFold");
  const opacity = document.querySelector(".pluginOpacity");

  // The labels, in the meter's language (ui text "pluginDock"), English
  // until they load.
  let text = {};
  const t = (key, fallback, vars = {}) =>
    String(text[key] || fallback).replace(/\{(\w+)\}/g, (_, k) => (k in vars ? vars[k] : `{${k}}`));
  const loadText = async (lang) => {
    const load = async (l) => {
      try {
        const res = await fetch(`./i18n/ui/${l}.json`, { cache: "no-store" });
        return res.ok ? (await res.json())?.pluginDock || null : null;
      } catch {
        return null;
      }
    };
    text = (await load(lang)) || (await load("en")) || {};
  };

  let toastTimer = 0;
  const showToast = (msg) => {
    toast.textContent = msg.slice(0, 200);
    toast.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toast.hidden = true; }, 4000);
  };

  // The Settings colours, as on a plugin window (pluginHost.js).
  const applyTheme = (theme) => {
    if (!theme) return;
    const rgb = (hex) => [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)).join(", ");
    const root = document.documentElement.style;
    root.setProperty("--theme-bg", rgb(theme.bg));
    root.setProperty("--theme-panel", rgb(theme.panel));
    root.setProperty("--theme-accent", theme.accent);
  };

  // The dock's preferences, kept by this page.
  const PREFS_KEY = "a2plugin.dock";
  let prefs = { order: [], active: null, layout: "tabs", folded: false, opacity: 100, compact: {} };
  try { prefs = { ...prefs, ...JSON.parse(localStorage.getItem(PREFS_KEY) || "{}") }; } catch {}
  const savePrefs = () => { try { localStorage.setItem(PREFS_KEY, JSON.stringify(prefs)); } catch {} };

  // Each docked plugin: { id, name, version, shown, panel, tab, plugin, alert }.
  const docked = new Map();
  const ordered = () => [...docked.values()].sort((a, b) => {
    const ia = prefs.order.indexOf(a.id), ib = prefs.order.indexOf(b.id);
    return (ia < 0 ? 1e9 : ia) - (ib < 0 ? 1e9 : ib) || a.name.localeCompare(b.name);
  });

  const setLabel = (btn, label) => {
    btn.setAttribute("aria-label", label);
    btn.title = label;
  };

  // Keep a selected configuration even when its plugin hides an alert.
  // Selection never opens the native dock; only an explicit Open does that.
  const render = () => {
    const tabs = ordered();
    if (!tabs.some((d) => d.id === prefs.active)) prefs.active = tabs[0]?.id ?? null;
    const stacked = prefs.layout === "stack";
    document.body.classList.toggle("dockStacked", stacked);
    document.body.classList.toggle("dockFolded", prefs.folded);
    tabsEl.replaceChildren();
    // Panels keep their place in the page and are ordered by CSS: moving an
    // iframe reloads it, and every tab click restarted every plugin (Ping
    // then lost its widget).
    let first = true;
    ordered().forEach((d, index) => {
      d.panel.style.order = String(index);
    });
    for (const d of ordered()) {
      const active = d.id === prefs.active;
      d.panel.hidden = stacked ? !d.shown && !active : !active;
      d.panel.classList.toggle("dockPanelAlert", !!d.alert);
      d.panel.classList.toggle("dockPanelFirst", !d.panel.hidden && first);
      if (!d.panel.hidden) first = false;
      // Keep configuration reachable after a plugin hides its panel or alert.
      d.tab.classList.toggle("dockTabActive", active && !stacked);
      d.tab.classList.toggle("dockTabAlert", !!d.alert);
      d.tab.setAttribute("aria-selected", String(active));
      tabsEl.appendChild(d.tab);
    }
    // New tabs' icons, now that they're in the page (Lucide draws only
    // what the page holds; ones already drawn are left alone).
    try { window.lucide?.createIcons(); } catch {}
    layoutBtn.setAttribute("aria-pressed", String(stacked));
    setLabel(layoutBtn, stacked ? t("tabs", "Show the plugins as tabs") : t("stack", "Stack the plugins"));
    foldBtn.setAttribute("aria-pressed", String(prefs.folded));
    setLabel(foldBtn, prefs.folded ? t("unfold", "Unfold the dock") : t("fold", "Fold the dock"));
    savePrefs();
  };

  const activate = (id) => {
    prefs.active = id;
    if (prefs.folded) setFolded(false);
    render();
    if (prefs.layout === "stack") docked.get(id)?.panel.scrollIntoView({ block: "start", behavior: "smooth" });
  };

  // Folded: the dock is its tab strip alone, the window fitted to it.
  const setFolded = (folded) => {
    prefs.folded = folded;
    const height = folded ? Math.ceil(document.querySelector(".dockBar").getBoundingClientRect().height) + 2 : null;
    invoke("dock_fold", { height }).catch(() => {});
    render();
  };

  // A tab can be dragged to a new place among the others.
  let dragging = null;
  const makeTab = (d) => {
    const tab = document.createElement("button");
    tab.type = "button";
    tab.className = "dockTab";
    tab.setAttribute("role", "tab");
    tab.draggable = true;
    // The plugin's icon (or first letter) in its colour; the name is the
    // tooltip and what a screen reader says.
    const { glyph, hue, letter } = window.A2_PLUGIN_ICON(d.id, d.name);
    if (glyph) {
      const i = document.createElement("i");
      i.setAttribute("data-lucide", glyph);
      i.setAttribute("aria-hidden", "true");
      tab.append(i);
    } else {
      tab.textContent = letter;
    }
    tab.style.setProperty("--mark-hue", String(hue));
    tab.setAttribute("aria-label", d.name);
    tab.title = `${d.name} ${d.version}`;
    tab.addEventListener("click", () => {
      // The click owns selection immediately. The native acknowledgement
      // only keeps configuration open; it must not select an older tab.
      backTo.clear();
      activate(d.id);
      invoke("dock_select", { id: d.id }).catch((e) => showToast(String(e)));
    });
    tab.addEventListener("dragstart", (e) => {
      dragging = d.id;
      e.dataTransfer.effectAllowed = "move";
    });
    tab.addEventListener("dragover", (e) => {
      if (dragging && dragging !== d.id) e.preventDefault();
    });
    tab.addEventListener("drop", (e) => {
      e.preventDefault();
      if (!dragging || dragging === d.id) return;
      const ids = ordered().map((x) => x.id).filter((x) => x !== dragging);
      ids.splice(ids.indexOf(d.id), 0, dragging);
      prefs.order = ids;
      dragging = null;
      render();
    });
    tab.addEventListener("dragend", () => { dragging = null; });
    return tab;
  };

  const makePanel = (d) => {
    const panel = template.content.firstElementChild.cloneNode(true);
    panel.dataset.plugin = d.id;
    panel.querySelector(".dockPanelName").textContent = d.name;
    const compactBtn = panel.querySelector(".dockPanelCompact");
    const showCompact = () => {
      const on = !!prefs.compact[d.id];
      compactBtn.setAttribute("aria-pressed", String(on));
      setLabel(compactBtn, on ? t("showControls", "Show the controls") : t("hideControls", "Hide the controls"));
    };
    showCompact();
    compactBtn.addEventListener("click", () => {
      prefs.compact[d.id] = !prefs.compact[d.id];
      savePrefs();
      showCompact();
      d.plugin.setCompact(!!prefs.compact[d.id]);
    });
    const popOut = panel.querySelector(".dockPanelPopOut");
    setLabel(popOut, t("popOut", "Open in its own window"));
    popOut.addEventListener("click", () => {
      invoke("plugin_set_docked", { id: d.id, docked: false }).catch((e) => showToast(String(e)));
    });
    const hide = panel.querySelector(".dockPanelHide");
    setLabel(hide, t("hide", "Hide (the plugin keeps running)"));
    hide.addEventListener("click", () => {
      invoke("plugin_close", { plugin: d.id }).catch(() => {});
    });
    return panel;
  };

  // The meter's word on which plugins are docked: frames started for new
  // ones, stopped for the ones gone, tabs shown or hidden.
  const applyLayout = (layout) => {
    const tabs = Array.isArray(layout?.tabs) ? layout.tabs : [];
    const ids = new Set(tabs.map((x) => x.id));
    for (const [id, d] of docked) {
      if (ids.has(id)) continue;
      d.plugin.destroy();
      d.panel.remove();
      d.tab.remove();
      docked.delete(id);
    }
    for (const x of tabs) {
      let d = docked.get(x.id);
      if (!d) {
        d = { id: x.id, name: x.name, version: x.version, shown: !!x.shown, alert: false };
        d.panel = makePanel(d);
        d.tab = makeTab(d);
        d.plugin = window.createPluginFrame({
          id: d.id,
          stage: d.panel.querySelector(".dockFrame"),
          docked: true,
          compact: !!prefs.compact[d.id],
          toast: (msg) => showToast(`${d.name}: ${msg}`),
          // An alert (window.fit): its tab opens and is marked until the
          // plugin puts its window back. The dock keeps its size.
          onFit: (fitted) => {
            d.alert = fitted;
            // dock-focus remembers the previous tab before opening the alert.
            render();
          },
        });
        docked.set(d.id, d);
        stage.appendChild(d.panel);
        d.plugin.start().catch((e) => {
          d.panel.querySelector(".dockFrame").textContent = t("failed", "This plugin could not start: {error}", { error: String(e) });
        });
      }
      d.shown = !!x.shown;
      if (!prefs.order.includes(d.id)) prefs.order.push(d.id);
    }
    render();
  };

  listen("dock-layout", (event) => applyLayout(event.payload));
  // A plugin that showed itself (an alert) opens its tab; the tab open before
  // is gone back to when it hides itself again ("dock-return").
  const backTo = new Map();
  listen("dock-focus", (event) => {
    const id = event.payload;
    if (!docked.has(id)) return;
    if (prefs.active && prefs.active !== id && !backTo.has(id)) backTo.set(id, prefs.active);
    activate(id);
  });
  listen("dock-return", (event) => {
    const back = backTo.get(event.payload);
    backTo.delete(event.payload);
    // An earlier alert can finish while a newer one is still selected.
    for (const [id, previous] of backTo) {
      if (back && previous === event.payload) backTo.set(id, back);
    }
    if (prefs.active === event.payload && back && docked.has(back)) activate(back);
  });

  // Plugin events, each to its plugin's frame; one without a plugin to all.
  // `listen` hears events sent to any window, plugins' own windows too: the
  // ones for plugins not docked here are left alone.
  const onEvent = (payload) => {
    payload ||= {};
    if (typeof payload.event !== "string") return;
    if (payload.plugin === undefined) {
      if (payload.event === "theme") applyTheme(payload.data);
      for (const d of docked.values()) d.plugin.receive(payload);
      return;
    }
    docked.get(payload.plugin)?.plugin.receive(payload);
  };
  listen("plugin-event", (event) => onEvent(event.payload));
  // The meter's numbered events, several at once (pluginHost.js).
  listen("plugin-events", (event) => {
    if (Array.isArray(event.payload)) for (const e of event.payload) onEvent(e);
  });

  // The bar: it moves the window, and its buttons.
  document.querySelector(".dockBar").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || e.target.closest("button, input")) return;
    e.preventDefault();
    invoke("start_tool_drag").catch(() => {});
  });
  document.querySelector(".pluginClose").addEventListener("click", () => {
    invoke("dock_close").catch(() => {});
  });
  layoutBtn.addEventListener("click", () => {
    prefs.layout = prefs.layout === "stack" ? "tabs" : "stack";
    render();
  });
  foldBtn.addEventListener("click", () => setFolded(!prefs.folded));
  // A double-click on the tab strip's empty part folds or unfolds too.
  tabsEl.addEventListener("dblclick", (e) => {
    if (e.target === tabsEl) setFolded(!prefs.folded);
  });

  // The DPS meter and Settings, as on a plugin window.
  const meterBtn = document.querySelector(".pluginMeter");
  const showMeterState = (shown) => {
    meterBtn.setAttribute("aria-pressed", String(shown));
    setLabel(meterBtn, shown ? t("hideMeter", "Hide the DPS meter") : t("showMeter", "Show the DPS meter"));
  };
  invoke("meter_shown").then((shown) => showMeterState(shown !== false)).catch(() => {});
  listen("meter-shown", (event) => showMeterState(event.payload !== false));
  meterBtn.addEventListener("click", () => {
    invoke("set_meter_shown", { shown: meterBtn.getAttribute("aria-pressed") !== "true" }).catch(() => {});
  });
  // Settings, on its Plugins page.
  document.querySelector(".pluginSettings").addEventListener("click", () => {
    invoke("open_settings_at", { section: "plugins" }).catch(() => {});
  });

  // One opacity for the whole dock.
  const applyOpacity = () => document.body.style.setProperty("--panel-alpha", String(prefs.opacity / 100));
  opacity.value = String(prefs.opacity);
  applyOpacity();
  opacity.addEventListener("input", () => {
    prefs.opacity = Number(opacity.value) || 100;
    applyOpacity();
    savePrefs();
  });

  invoke("dock_hello").then(async (hello) => {
    await loadText(hello.language || "en");
    applyTheme(hello.theme);
    setLabel(opacity, t("opacity", "Background opacity"));
    setLabel(document.querySelector(".pluginSettings"), t("pluginSettings", "Plugin settings"));
    setLabel(document.querySelector(".pluginClose"), t("close", "Hide the dock (the plugins keep running)"));
    applyLayout(hello);
    // Built to show one plugin (a widget double-clicked): on its tab.
    if (typeof hello.focus === "string" && docked.has(hello.focus)) activate(hello.focus);
    try { window.lucide?.createIcons(); } catch {}
    if (prefs.folded) setFolded(true);
  }).catch((e) => {
    stage.textContent = String(e);
  });
})();
