// Settings → Plugins: the installed plugins (Open, Update, Remove), the
// reviewed plugins on offer from the signed catalog (Install), checking for
// updates, and developer mode with its local folders (src-tauri/src/plugins).
(() => {
  if (window.__A2_VIEW__ !== "settings") return;
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  const t = (key, fallback, vars = {}) => {
    let text = window.i18n?.t?.(key, fallback) ?? fallback;
    for (const [name, value] of Object.entries(vars)) text = text.replace(`{${name}}`, value);
    return text;
  };

  // Update plugins automatically (src-tauri/src/plugins AUTO_UPDATE_KEY):
  // off unless turned on; the meter reads it when it starts.
  const AUTO_UPDATE_KEY = "dpsMeter.pluginAutoUpdate";

  const start = async () => {
    // Read controls and version history from the same saved snapshot as the
    // settings form, rather than from an empty or stale localStorage cache.
    await window.a2SettingsReady;
    const autoUpdate = document.querySelector(".pluginAutoUpdateCheckbox");
    if (autoUpdate) {
      autoUpdate.checked = window.javaBridge?.getSetting?.(AUTO_UPDATE_KEY) === "true";
      autoUpdate.addEventListener("change", () => {
        window.javaBridge?.setSetting?.(AUTO_UPDATE_KEY, autoUpdate.checked ? "true" : "false");
      });
    }

    const toggle = document.querySelector(".pluginDevModeCheckbox");
    const devRow = document.querySelector(".pluginDevRow");
    const list = document.querySelector(".pluginList");
    const empty = document.querySelector(".pluginListEmpty");
    const catalogList = document.querySelector(".pluginCatalog");
    const catalogStatus = document.querySelector(".pluginCatalogStatus");
    const add = document.querySelector(".pluginAddBtn");
    const update = document.querySelector(".pluginUpdateBtn");
    const status = document.querySelector(".pluginStatus");
    const consent = document.querySelector(".pluginConsent");
    if (!toggle || !devRow || !list || !empty || !catalogList || !add || !update || !status || !consent) return;

    // The status line shows errors in red and other news in the hint colour.
    const say = (text, isError = true) => {
      status.textContent = text;
      status.classList.toggle("pluginError", isError && !!text);
    };

    let view = { devMode: false, online: true, plugins: [], disabled: [] };
    let catalog = null; // the catalog's entries, once fetched
    // Plugins the last update changed, by id → the version before; marked
    // green until Settings closes.
    const updatedFrom = new Map();
    // Newer reviewed versions, by id → { to, newPermissions, changes }.
    const available = new Map();

    // What's new. Each plugin's version when Settings last showed it, kept
    // in settings: a plugin updated since (automatically, or from the
    // title's button) is marked, its notes shown, until Settings has been
    // opened once. The first time, the versions are only noted down.
    const SEEN_KEY = "dpsMeter.pluginsSeen";
    let seen = null;
    try {
      seen = JSON.parse(window.javaBridge?.getSetting?.(SEEN_KEY) || "null");
    } catch {
      seen = null;
    }
    const firstLook = !seen || typeof seen !== "object";
    if (firstLook) seen = {};
    // By id → the version Settings last showed (this window's).
    const updatedSince = new Map();
    const noteSeen = (plugins) => {
      let changed = false;
      for (const p of plugins) {
        if (!p.reviewed || p.error) continue;
        const was = seen[p.id];
        if (!firstLook && was && newerVersion(p.version, was) && !updatedSince.has(p.id)) updatedSince.set(p.id, was);
        if (was !== p.version) {
          seen[p.id] = p.version;
          changed = true;
        }
      }
      if (changed) window.javaBridge?.setSetting?.(SEEN_KEY, JSON.stringify(seen));
    };
    // "1.2.10" after "1.2.9".
    function newerVersion(a, b) {
      const parts = (v) => String(v || "").split(".").map((n) => parseInt(n, 10) || 0);
      const x = parts(a);
      const y = parts(b);
      for (let i = 0; i < Math.max(x.length, y.length); i++) {
        if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) > (y[i] || 0);
      }
      return false;
    }
    // The notes of a plugin's versions after `from`, up to `to`, newest first,
    // from the signed catalog (or an update's own).
    const notesOf = (id, from, to, own) => {
      const all = own || (catalog || []).find((e) => e.id === id)?.changes || [];
      return all.filter((c) => newerVersion(c.version, from) && !newerVersion(c.version, to));
    };
    const noteText = (c) => c[window.i18n?.getLanguage?.() || "en"] || c.en || "";
    // "What's new in 1.4.2", and each version's note.
    const whatsNew = (notes, version) => {
      const box = el("div", "pluginWhatsNew");
      box.append(el("div", "pluginWhatsNewTitle", t("settings.plugins.whatsNew", "What's new in {version}", { version })));
      const items = el("ul", "pluginWhatsNewList");
      for (const c of notes) {
        const text = noteText(c);
        if (!text) continue;
        const li = el("li", null, text);
        if (notes.length > 1) li.prepend(el("span", "pluginWhatsNewVersion", c.version));
        items.append(li);
      }
      box.append(items);
      return box;
    };

    // A permission, in plain words.
    const describe = (permission) => {
      if (permission === "storage") return t("settings.plugins.permStorage", "Save its own data on this computer");
      if (permission === "notify") return t("settings.plugins.permNotify", "Show short messages in its window");
      if (permission === "widgets") {
        return t("settings.plugins.permWidgets", "Show small windows of its own, which you can move anywhere and lock in place");
      }
      if (permission === "window") {
        return t("settings.plugins.permWindow", "Show and hide its own window, without taking focus from the game");
      }
      if (permission === "events:character") {
        return t("settings.plugins.permCharacter", "See your character's name, server, class and level");
      }
      if (permission === "feed:boss-timers") {
        return t("settings.plugins.permBossTimers", "Read the field boss timers players share through a2tools.app");
      }
      if (permission === "fps") {
        return t("settings.plugins.permFps", "Show the game's frame rate (on Windows, the meter asks to run as administrator for it)");
      }
      if (permission === "screenshot") {
        return t("settings.plugins.permScreenshot", "Take screenshots of its own window, to the clipboard and your screenshot folder");
      }
      if (permission === "events:death-recap") {
        return t("settings.plugins.permDeathRecap", "After a player kills you: who they were and the damage they did to you");
      }
      if (permission.startsWith("decoder:")) {
        return t("settings.plugins.permDecoderFields", "Read selected game fields (may include other players): {name}", { name: permission.slice(8) });
      }
      return permission;
    };

    // Ask before installing, listing what the plugin may do. Resolves true to go ahead.
    const ask = ({ title, intro, permissions, confirm }) =>
      new Promise((resolve) => {
        consent.querySelector(".hotkeyConsentTitle").textContent = title;
        consent.querySelector(".pluginConsentIntro").textContent = intro;
        const items = permissions.length
          ? permissions.map(describe)
          : [t("settings.plugins.permNone", "Nothing beyond showing its window")];
        consent.querySelector(".pluginConsentList").replaceChildren(
          ...items.map((text) => Object.assign(document.createElement("li"), { textContent: text })),
        );
        const yes = consent.querySelector(".pluginConsentConfirm");
        const no = consent.querySelector(".pluginConsentCancel");
        yes.textContent = confirm;
        const done = (answer) => {
          consent.hidden = true;
          yes.removeEventListener("click", onYes);
          no.removeEventListener("click", onNo);
          consent.removeEventListener("keydown", onKey);
          resolve(answer);
        };
        const onYes = () => done(true);
        const onNo = () => done(false);
        const onKey = (e) => { if (e.key === "Escape") done(false); };
        yes.addEventListener("click", onYes);
        no.addEventListener("click", onNo);
        consent.addEventListener("keydown", onKey);
        consent.hidden = false;
        yes.focus();
      });

    const button = (label, onClick, extra = "") => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = `settingsAction ${extra}`.trim();
      b.textContent = label;
      b.addEventListener("click", onClick);
      return b;
    };

    const el = (tag, className, text) => {
      const node = document.createElement(tag);
      if (className) node.className = className;
      if (text != null) node.textContent = text;
      return node;
    };

    const icon = (name) => {
      const i = document.createElement("i");
      i.setAttribute("data-lucide", name);
      i.setAttribute("aria-hidden", "true");
      return i;
    };

    // A square with the plugin's icon or first letter, in its colour (from
    // its id when it has none), so a long list scans by shape and colour
    // before reading.
    // (The icons: pluginIcons.js.)
    const mark = (id, name) => {
      const { glyph, hue, letter } = window.A2_PLUGIN_ICON(id, name);
      const m = el("span", "pluginMark", glyph ? null : letter);
      if (glyph) m.append(icon(glyph));
      m.style.setProperty("--mark-hue", String(hue));
      m.setAttribute("aria-hidden", "true");
      return m;
    };

    // The plugin whose details are open (one at a time), kept across renders.
    let openId = null;

    // A plugin's row: one line (mark, name and version, description cut to
    // fit, then its controls), and its details folded under it until the
    // row is clicked.
    const pluginRow = ({ id, name, version, tags = [], description, isError, controls, details }) => {
      const item = el("div", "pluginItem");
      item.dataset.id = id;
      const head = el("div", "pluginHead");
      head.tabIndex = 0;
      head.setAttribute("role", "button");
      const main = el("span", "pluginMain");
      const title = el("span", "pluginName");
      title.append(el("span", "pluginNameText", name));
      if (version) title.append(el("span", "pluginVersion", version));
      title.append(...tags);
      const desc = el("span", isError ? "pluginDesc pluginError" : "pluginDesc", description || "");
      if (description) desc.title = description;
      main.append(title, desc);
      const end = el("span", "pluginEnd");
      end.append(...controls);
      const chevron = el("span", "pluginChevron");
      chevron.append(icon("chevron-down"));
      head.append(mark(id, name), main, end, chevron);
      const body = el("div", "pluginDetails");
      body.append(...details);
      const setOpen = (open) => {
        item.classList.toggle("isOpen", open);
        body.hidden = !open;
        head.setAttribute("aria-expanded", String(open));
      };
      setOpen(openId === id);
      const flip = () => {
        const open = !item.classList.contains("isOpen");
        // Close the one open before.
        for (const other of document.querySelectorAll(".settingsPanel .pluginItem.isOpen")) {
          if (other !== item) {
            other.classList.remove("isOpen");
            other.querySelector(".pluginDetails").hidden = true;
            other.querySelector(".pluginHead").setAttribute("aria-expanded", "false");
          }
        }
        openId = open ? id : null;
        setOpen(open);
      };
      head.addEventListener("click", (e) => {
        if (e.target.closest(".pluginEnd")) return;
        flip();
      });
      head.addEventListener("keydown", (e) => {
        if (e.target !== head || (e.key !== "Enter" && e.key !== " ")) return;
        e.preventDefault();
        flip();
      });
      item.append(head, body);
      return item;
    };

    // What it may do, in its details.
    const permissionList = (permissions, installed) => {
      const box = el("div", "pluginPerms");
      box.append(el("div", "pluginDetailTitle", installed
        ? t("settings.plugins.permsTitle", "It can:")
        : t("settings.plugins.consentIntro", "It will be able to:")));
      const ul = el("ul");
      const items = permissions.length ? permissions.map(describe) : [t("settings.plugins.permNone", "Nothing beyond showing its window")];
      ul.append(...items.map((text) => el("li", "", text)));
      box.append(ul);
      return box;
    };

    // A small switch with its label, for a plugin's options.
    const option = (text, checked, onChange) => {
      const row = el("label", "pluginOption");
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = checked;
      input.addEventListener("change", () => onChange(input.checked));
      const control = el("span", "settingsToggleControl");
      control.append(input, Object.assign(el("span", "settingsToggleTrack"), { ariaHidden: "true" }));
      row.append(el("span", "pluginOptionText", text), control);
      return row;
    };

    const iconButton = (name, label, onClick, extra = "") => {
      const b = el("button", `pluginIconBtn ${extra}`.trim());
      b.type = "button";
      b.title = label;
      b.setAttribute("aria-label", label);
      b.append(icon(name));
      b.addEventListener("click", onClick);
      return b;
    };

    // On: the plugin starts with the meter. Off: installed, but not running.
    const enabledSwitch = (p, on) => {
      const control = document.createElement("label");
      control.className = "settingsToggleControl pluginEnabledToggle";
      const tip = t("settings.plugins.enabledTip", "On: starts with the meter. Off: stays installed but doesn't run.");
      control.title = tip;
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = on;
      input.setAttribute("aria-label", `${t("settings.plugins.enabled", "Enabled")}: ${p.name}`);
      input.addEventListener("change", () => {
        say("");
        run(invoke("plugin_set_enabled", { id: p.id, enabled: input.checked }));
      });
      const track = document.createElement("span");
      track.className = "settingsToggleTrack";
      track.setAttribute("aria-hidden", "true");
      control.append(input, track);
      return control;
    };

    // Installed or Available, and the search over both (shown once there are
    // enough plugins to need it).
    let tab = null;
    let query = "";
    const tabs = [...document.querySelectorAll(".settingsPanel .pluginTab")];
    const panes = [...document.querySelectorAll(".settingsPanel .pluginPane")];
    const search = document.querySelector(".pluginSearch");
    const searchInput = document.querySelector(".pluginSearchInput");
    const noMatch = document.querySelector(".pluginNoMatch");
    const SEARCH_FROM = 6;

    const matches = (...texts) => !query || texts.some((x) => String(x || "").toLowerCase().includes(query));

    const showTab = (name) => {
      tab = name;
      for (const b of tabs) {
        const on = b.dataset.tab === name;
        b.classList.toggle("isActive", on);
        b.setAttribute("aria-selected", String(on));
      }
      for (const pane of panes) pane.hidden = pane.dataset.pane !== name;
      refreshNoMatch();
    };
    for (const b of tabs) b.addEventListener("click", () => showTab(b.dataset.tab));

    const refreshNoMatch = () => {
      const shown = tab === "browse" ? catalogList.childElementCount : list.childElementCount;
      noMatch.hidden = !query || shown > 0;
    };

    const refreshBar = () => {
      const installed = (view.plugins || []).length;
      const offered = catalog ? catalog.filter((e) => !e.installed).length : 0;
      document.querySelector(".pluginInstalledCount").textContent = installed ? String(installed) : "";
      document.querySelector(".pluginAvailableCount").textContent = offered ? String(offered) : "";
      search.hidden = installed + offered < SEARCH_FROM && !query;
      if (!tab) showTab(installed ? "installed" : "browse");
      refreshNoMatch();
    };

    searchInput?.addEventListener("input", () => {
      query = searchInput.value.trim().toLowerCase();
      render();
      renderCatalog();
    });

    const run = (promise) => promise.then(render).catch((e) => say(String(e)));

    const renderCatalog = () => {
      catalogList.replaceChildren();
      catalogStatus.classList.remove("pluginError");
      if (!view.online) {
        catalogStatus.textContent = t("settings.plugins.offline", "The offline build does not download plugins.");
        refreshBar();
        return;
      }
      if (!catalog) return refreshBar();
      // Reviewed plugins, then the test plugins under their own heading:
      // early versions, signed the same way, there for anyone to try.
      const offered = catalog.filter((e) => !e.installed);
      const reviewed = offered.filter((e) => !e.test);
      const tests = offered.filter((e) => e.test);
      const rowFor = (entry) => {
        if (!matches(entry.name, entry.description, entry.id, entry.author)) return;
        const details = [el("p", "pluginDescFull", entry.description || "")];
        if (entry.author) details.push(el("div", "pluginAuthor", t("settings.plugins.by", "by {author}", { author: entry.author })));
        details.push(permissionList(entry.permissions || []));
        catalogList.append(
          pluginRow({
            id: entry.id,
            name: entry.name,
            version: entry.version,
            description: entry.description,
            controls: [button(t("settings.plugins.install", "Install"), () => install(entry, null), "pluginInstallBtn")],
            details,
          }),
        );
        if (entry.test) catalogList.lastElementChild?.classList.add("pluginTest");
      };
      reviewed.forEach(rowFor);
      const shownBefore = catalogList.childElementCount;
      if (tests.length) {
        const head = el("div", "pluginTestHead");
        head.append(
          el("div", "pluginTestTitle", t("settings.plugins.testTitle", "Test plugins")),
          el("div", "settingsHint", t("settings.plugins.testHint",
            "Early versions for anyone to try, signed like the others. They may change or have rough edges: tell us what you find.")),
        );
        catalogList.append(head);
        tests.forEach(rowFor);
        // The heading only with a test plugin under it (a search can hide them all).
        if (catalogList.childElementCount === shownBefore + 1) head.remove();
      }
      catalogStatus.textContent = offered.length ? "" : t("settings.plugins.catalogEmpty", "Every reviewed plugin is installed.");
      window.lucide?.createIcons?.({ root: catalogList });
      refreshBar();
    };

    const loadCatalog = async () => {
      if (!view.online) return renderCatalog();
      if (!catalog) catalogStatus.textContent = t("settings.plugins.catalogLoading", "Loading the plugin list…");
      try {
        catalog = await invoke("plugin_catalog");
        // The installed list again, with each one's notes.
        render();
      } catch (e) {
        catalog = null;
        catalogList.replaceChildren();
        catalogStatus.textContent = t("settings.plugins.catalogError", "Couldn't load the plugin list: {error}", { error: String(e) });
        catalogStatus.classList.add("pluginError");
        return;
      }
      renderCatalog();
    };

    // Install a catalog entry, or update to it (`from` is the installed
    // version). Asks first, listing the permissions: all of them for a new
    // plugin, only the new ones for an update.
    const install = async (entry, from) => {
      const isUpdate = !!from;
      const permissions = isUpdate ? available.get(entry.id)?.newPermissions || [] : entry.permissions || [];
      if (!isUpdate || permissions.length) {
        const ok = await ask({
          title: isUpdate
            ? t("settings.plugins.consentUpdateTitle", "Update {name}?", { name: entry.name })
            : t("settings.plugins.consentTitle", "Install {name}?", { name: entry.name }),
          intro: isUpdate
            ? t("settings.plugins.consentNew", "The new version also asks to:")
            : t("settings.plugins.consentIntro", "It will be able to:"),
          permissions,
          confirm: isUpdate ? t("settings.plugins.update", "Update") : t("settings.plugins.install", "Install"),
        });
        if (!ok) return;
      }
      say(t("settings.plugins.installing", "Installing…"), false);
      try {
        const next = await invoke("plugin_install", { id: entry.id });
        if (isUpdate) updatedFrom.set(entry.id, from);
        available.delete(entry.id);
        render(next);
        say(t("settings.plugins.installDone", "{name} {version} is installed.", { name: entry.name, version: entry.version }), false);
        await loadCatalog();
      } catch (e) {
        say(String(e));
      }
    };

    const render = (next) => {
      if (next) view = next;
      toggle.checked = !!view.devMode;
      devRow.hidden = !view.devMode;
      list.replaceChildren();
      noteSeen(view.plugins || []);
      // The first plugin updated since Settings was last open shows its
      // notes without a click.
      if (openId == null) {
        const first = (view.plugins || []).find((p) => updatedSince.has(p.id) && notesOf(p.id, updatedSince.get(p.id), p.version).length);
        if (first) openId = first.id;
      }
      for (const p of view.plugins || []) {
        if (!matches(p.name, p.description, p.id)) continue;
        const tags = [];
        const wasUpdated = !p.error && (updatedFrom.has(p.id) || updatedSince.has(p.id));
        if (wasUpdated) {
          const from = updatedFrom.has(p.id) ? updatedFrom.get(p.id) : updatedSince.get(p.id);
          const badge = icon("circle-arrow-up");
          badge.className = "pluginUpdatedIcon";
          badge.setAttribute("role", "img");
          badge.removeAttribute("aria-hidden");
          const label = from
            ? t("settings.plugins.updatedFrom", "Updated from {version}", { version: from })
            : t("settings.plugins.added", "Newly loaded");
          badge.setAttribute("aria-label", label);
          badge.title = label;
          tags.push(badge);
        }
        if (!p.reviewed) {
          const tag = el("span", "pluginUnreviewedTag", t("settings.plugins.unreviewed", "Dev"));
          tag.title = t("settings.plugins.hint", "Plugins loaded here are not reviewed. Only load plugins you trust.");
          tags.push(tag);
        }
        const on = !(view.disabled || []).includes(p.id);
        const controls = [];
        const newer = p.reviewed && available.get(p.id);
        if (newer) {
          controls.push(button(
            t("settings.plugins.updateTo", "Update to {version}", { version: newer.to }),
            () => install({ id: p.id, name: p.name, version: newer.to }, p.version),
            "hotkeyConsentConfirm pluginInstallBtn",
          ));
        }
        if (!p.error && on) {
          controls.push(iconButton("app-window", t("settings.plugins.open", "Open"), () => invoke("plugin_open", { id: p.id }).catch((e) => say(String(e)))));
        }
        if (!p.error) controls.push(enabledSwitch(p, on));

        // Its details: the whole description, when it hides, what it may
        // do, and Remove.
        const details = [];
        // What's new: in the update waiting, or in the one just installed.
        if (!p.error && p.reviewed) {
          const from = updatedFrom.has(p.id) ? updatedFrom.get(p.id) : updatedSince.get(p.id);
          const notes = newer
            ? notesOf(p.id, p.version, newer.to, newer.changes)
            : from != null ? notesOf(p.id, from, p.version) : [];
          if (notes.length) details.push(whatsNew(notes, newer ? newer.to : p.version));
        }
        if (p.error) details.push(el("p", "pluginDescFull pluginError", p.error));
        else if (p.description) details.push(el("p", "pluginDescFull", p.description));
        if (!p.reviewed && p.path) details.push(el("div", "pluginAuthor", p.path));
        if (!p.error) {
          // Its windows and widgets hidden while AION2 is closed, or with the
          // meter while AION2 isn't in front (off: an alert's icon stays up).
          const options = el("div", "pluginOptions");
          for (const [when, ids, key, text] of [
            ["closed", view.hideClosed, "settings.plugins.hideClosed", "Hide while AION2 is closed"],
            ["unfocused", view.hideUnfocused, "settings.plugins.hideUnfocused", "Hide when AION2 isn't focused"],
          ]) {
            options.append(option(t(key, text), (ids || []).includes(p.id), (hide) => run(invoke("plugin_set_hide", { id: p.id, when, hide }))));
          }
          // In the plugin dock with the others, or a window of its own.
          options.append(option(
            t("settings.plugins.inDock", "In the plugin dock (off: its own window)"),
            !(view.ownWindow || []).includes(p.id),
            (docked) => run(invoke("plugin_set_docked", { id: p.id, docked }).then(() => invoke("plugin_list"))),
          ));
          details.push(options, permissionList(p.permissions || [], true));
        }
        const actions = el("div", "pluginDetailActions");
        actions.append(button(t("settings.plugins.remove", "Remove"), () => {
          say("");
          if (p.reviewed) run(invoke("plugin_uninstall", { id: p.id })).then(loadCatalog);
          else run(invoke("plugin_remove_folder", { path: p.path }));
        }, "pluginRemoveBtn"));
        details.push(actions);

        const item = pluginRow({
          id: p.id,
          name: p.name,
          version: p.error ? "" : p.version,
          tags,
          description: p.error || p.description || p.path,
          isError: !!p.error,
          controls,
          details,
        });
        if (wasUpdated) item.classList.add("pluginUpdated");
        if (!p.error && !on) item.classList.add("pluginOff");
        list.append(item);
      }
      empty.hidden = list.childElementCount > 0 || !!query;
      window.lucide?.createIcons?.({ root: list });
      refreshBar();
    };

    // The list comes with whether developer mode is on.
    invoke("plugin_list").then((v) => { render(v); loadCatalog(); }).catch(() => {});
    // An update installed from the prompt at start (plugins::alert_updates_at_start).
    window.__TAURI__?.window?.getCurrentWindow?.().listen("plugins-changed", () => {
      invoke("plugin_list").then((v) => { render(v); loadCatalog(); }).catch(() => {});
    });

    toggle.addEventListener("change", () => {
      say("");
      run(invoke("plugin_set_dev_mode", { enabled: toggle.checked }));
    });
    update.addEventListener("click", () => {
      update.disabled = true;
      say(t("settings.plugins.checking", "Checking…"), false);
      invoke("plugin_check_updates")
        .then(({ view: next, updated, available: newer, catalogError }) => {
          for (const u of updated) updatedFrom.set(u.id, u.from || "");
          available.clear();
          for (const a of newer) available.set(a.id, a);
          render(next);
          const lines = [];
          if (updated.length) {
            const changes = updated.map((u) => (u.from ? `${u.name} ${u.from} → ${u.to}` : `${u.name} ${u.to}`)).join(", ");
            lines.push(t("settings.plugins.updated", "Updated: {list}", { list: changes }));
          }
          if (newer.length) {
            const versions = newer.map((a) => `${a.name} ${a.to}`).join(", ");
            lines.push(t("settings.plugins.updatesAvailable", "Updates available: {list}", { list: versions }));
          }
          if (catalogError) {
            lines.push(t("settings.plugins.catalogError", "Couldn't load the plugin list: {error}", { error: catalogError }));
          }
          if (!lines.length) lines.push(t("settings.plugins.upToDate", "Every plugin is up to date."));
          say(lines.join(" "), !!catalogError && !updated.length && !newer.length);
          return loadCatalog();
        })
        .catch((e) => say(String(e)))
        .finally(() => { update.disabled = false; });
    });
    add.addEventListener("click", () => {
      say("");
      run(invoke("plugin_add_folder"));
    });
  };

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start, { once: true });
  else start();
})();
