// The plugin API (apiVersion 1), loaded by a plugin's page from /_sdk/a2.js.
// A plugin runs in a sandboxed frame: everything goes to the host page as a
// message, and the host passes on only what the manifest's permissions allow.
//
//   a2.on("decoded:pet-list", (data, { character }) => ...)
//                                              one of the plugin's decoders. Each event arrives once,
//                                              in order, even those from while the window was closed
//                                              (since the meter started); `character` is who was
//                                              playing (with events:character), else null; `at` is
//                                              when the meter read it (unix ms), or null on meters
//                                              that don't say
//   a2.on("character", (c) => ...)             your character (name, serverId, class, level), at start and
//                                              whenever it changes (meter 2.0.65 on); meter 2.0.78 on
//                                              also combatPower, gearScore, odyleEnergy ({ basic,
//                                              charged }) and contentCounts ({ "<entry id>": [first,
//                                              second] }: 1 Shugo keys, 4 Nightmare, 8 Invasion,
//                                              101 Daily Dungeon, 60000001 Odyle energy (as in
//                                              odyleEnergy), 80000001 Ascension Trial; 0 Daily Duty
//                                              today [completed, taken]; other ids may change meaning
//                                              or go: don't rely on them), each null until the game has
//                                              said. A meter started mid-session has only the counts
//                                              that changed since, until the next login or server
//                                              transfer. Also shopLimits ({ "<item id>": [bought, next
//                                              reset ms] }: 100134 Buy Odyle Energy, 100135 Buy Rift
//                                              Stones; [0, null] once its reset has passed, until the
//                                              shop opens again) once the shop has opened, crafts ({
//                                              "<recipe id>": made }: 32070001 Odyle Energy
//                                              server-wide, 32070002 per character) once the crafting
//                                              window has opened, kinahToday (Kinah gained today) and
//                                              abyssPoints ([this week, the week's limit]) from login,
//                                              0 once the daily reset (07:00 UTC) or the weekly
//                                              (Wednesday) has passed, and contentTimes ({ "<entry
//                                              id>": [time, seen at ms] }: 10000001-10000003 the Abyss
//                                              passages' usage time left (ms), gone once used; the time
//                                              as the game last sent it)
//   a2.on("bossMap", ({ map, bosses }) => ...) a map the player just opened, its field bosses as
//                                              a2.bossTimers gives them (permission
//                                              "feed:boss-timers", meter 2.0.65 on)
//   a2.on("deathRecap", (r) => ...)            a player killed you (permission "events:death-recap", meter
//                                              2.0.81 on): r = { at, killer: { name, serverId, legion,
//                                              class ("gladiator", ...), level, gearScore, combatPower },
//                                              damage, hits, durationMs, dps, skills: [{ id, name, damage,
//                                              hits }], assists: [{ name, legion, serverId, class, level, gearScore,
//                                              combatPower, damage, hits }] (the others who damaged you
//                                              in the fight, your party never) }, null where the game
//                                              did not say. Once each, kept like a decoder's events
//   a2.on("lootRoll", (e) => ...)              Dice Roll loot (permission "events:loot-rolls", meter
//                                              2.0.88 on), each as the game says it, with `at` (unix ms):
//                                              { type: "open", rollId, itemId, count, endsAt } an item is
//                                              up for a roll until endsAt (unix ms); { type: "mine", rollId,
//                                              roll } your roll (no item: match by rollId); { type: "roll",
//                                              itemId, player: { name, serverId }, roll } another's roll
//                                              (no rollId: match by itemId); { type: "pass", rollId, itemId,
//                                              player } a player passed; { type: "won", rollId, itemId,
//                                              player } who won it. The highest roll wins. A roll can
//                                              come before its "open" or without one. Once each, kept
//                                              like a decoder's events
//   a2.on("game", ({ running }) => ...)        whether AION2 is open, at start and on change
//                                              (meter 2.0.65 on)
//   a2.on("ping", ({ ms }) => ...)             the ping to the game server in ms (null while not known),
//                                              at start and on change, about every 10 s (meter 2.0.73 on)
//   a2.on("language", (code) => ...)           the meter's language ("en", "ko", "zh-Hans", ...), at start and on change
//   a2.on("theme", ({ bg, panel, accent }) => ...)  the Settings colours (#rrggbb each), at start and on change
//                                              (meter 2.0.76 on). The page has them as CSS too: var(--a2-accent),
//                                              var(--a2-bg), var(--a2-panel); and selects, checkboxes, radios,
//                                              sliders and scrollbars are styled to match unless the plugin styles them itself
//   a2.on("compact", (on) => ...)              the window's fold button: show only the essentials
//
// Keep the page's background transparent: the window paints the panel at the
// opacity the user picks.
//   await a2.storage.get("state")              the plugin's own saved data
//   await a2.storage.set("state", value)
//   a2.notify("Text")                          a short message in the plugin's window
//   await a2.bossTimers(serverId)              a server's field boss timers, as players' meters
//                                              share them through a2tools.app (permission
//                                              "feed:boss-timers", meter 2.0.65 on): { bosses: { <id>:
//                                              { state, upSince, nextSpawn, reportedAt, confirmed } } }
//                                              or null
//   await a2.followBosses(serverId, ["101006", ...])  follow these field bosses live (permission
//                                              "feed:boss-timers", meter 2.0.68 on): each change comes at
//                                              once as a2.on("bossLive", (m) => ...), m being
//                                              { type: "state", server, bosses } on following,
//                                              { type: "boss", server, boss, entry } on each change (an
//                                              entry as a2.bossTimers gives it), or
//                                              { type: "status", server, connected }; [] stops
//   await a2.watchFps(true)                    the game's frame rate (permission "fps", meter 2.0.78 on):
//                                              comes about every second as a2.on("fps", (f) => ...), f
//                                              being { fps, state }: state "ok" (fps a number), "noGame",
//                                              "needsAdmin" (Windows: the meter must run as administrator)
//                                              or "unsupported"; resolves to the reading now; false stops
//   await a2.fpsAdmin()                        ask the player to run the meter as administrator for the
//                                              frame rate (permission "fps"): the meter's prompt, then
//                                              Windows'; yes restarts the meter elevated (and it stays so)
//   await a2.screenshot({ x, y, width, height })  part of its page (CSS px; none: all of it) to the
//                                              clipboard and a PNG in the meter's screenshot folder
//                                              (permission "screenshot", meter 2.0.81 on): { clipboard,
//                                              file } (file null if not saved)
//   a2.window.show() / a2.window.hide()        its own window, shown on top without taking focus
//   a2.window.fit(height) / a2.window.fit()    the window fitted to that much content (an alert),
//                                              its title bar hidden; then back to its size and
//                                              title bar (meter 2.0.65 on)
//                                              (permission "window", plugin API 2)
//   Widgets (permission "widgets", meter 2.0.68 on): small windows of the plugin's own, each
//   showing its widget.html (?name=<name>), which the player can drag anywhere and lock in place.
//   From the plugin's page:
//   await a2.widgets.open("name", { width, height })  open one, or show it again; at most 16
//   await a2.widgets.close("name")
//   await a2.widgets.send("name", data)        the widget hears it: a2.on("widget", (data) => ...)
//   await a2.widgets.lock(true / false)        locked, clicks go through them and they stay put
//   await a2.widgets.locked()                  whether they are locked
//   a2.on("widget-message", ({ name, data }) => ...)  what a widget posted
//   From a widget's page (widget.html):
//   a2.widget.name                             its name
//   a2.widget.drag()                           call on mousedown: the widget follows the pointer
//                                              (not while locked)
//   a2.widget.area({ x, y, width, height })    the part of the page that takes the mouse (CSS px);
//                                              elsewhere clicks go through to what is under the
//                                              window, so widgets set close together don't catch
//                                              each other's drags (Linux, meter 2.0.70 on; Windows 2.0.77)
//   a2.widget.areas([{ x, y, width, height }, ...])  the same as several parts (up to 64), so the
//                                              gaps between them let clicks through too (meter 2.0.77 on)
//   a2.widget.post(data)                       to the plugin's page ("widget-message")
//   a2.on("widget", (data) => ...)             what the plugin sent it
//   a2.on("locked", (locked) => ...)           whether the widgets are locked, at start and on change
//
//   a2.ready()                                 call once the handlers are set
//
// A link (<a href="https://...">) clicked in the page opens in the browser
// (meter 2.0.65 on; before, the click did nothing).
(() => {
  // The Settings colours as CSS variables, and the page's form controls
  // styled with them: a select was the webview's own grey box. All in
  // :where(), so any rule of the plugin's own wins.
  const root = document.documentElement;
  const base = document.createElement("style");
  base.textContent = `
    /* Dark controls only: on the whole page it painted the frame opaque,
       and the window's opacity slider did nothing. */
    :where(select, input, textarea, progress) { color-scheme: dark; }
    :where(input[type="checkbox"], input[type="radio"], input[type="range"], progress) {
      accent-color: var(--a2-accent, #8fc2ff);
    }
    :where(select) {
      appearance: none;
      -webkit-appearance: none;
      box-sizing: border-box;
      min-height: 26px;
      padding: 3px 26px 3px 8px;
      border: 1px solid rgba(255, 255, 255, 0.16);
      border-radius: 6px;
      background: rgba(255, 255, 255, 0.06) url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='6' viewBox='0 0 10 6'%3E%3Cpath d='M1 1l4 4 4-4' fill='none' stroke='white' stroke-opacity='.7' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E") no-repeat right 9px center / 10px 6px;
      color: inherit;
      font: inherit;
      cursor: pointer;
      transition: border-color 120ms ease, background-color 120ms ease;
    }
    :where(select):hover { border-color: rgba(255, 255, 255, 0.3); background-color: rgba(255, 255, 255, 0.09); }
    :where(select):focus-visible { outline: 2px solid var(--a2-accent, #8fc2ff); outline-offset: 1px; }
    :where(select):disabled { opacity: 0.5; cursor: default; }
    :where(select option, select optgroup) { background: var(--a2-panel, #0e1420); color: #fff; }
    /* A quiet scrollbar: thin, no track, brighter only while in use. Before
       the plugin's own styles, so a scrollbar it styles itself wins. */
    ::-webkit-scrollbar { width: 8px; height: 8px; }
    ::-webkit-scrollbar-track, ::-webkit-scrollbar-corner { background: transparent; }
    ::-webkit-scrollbar-thumb {
      min-height: 28px;
      border: 2px solid transparent;
      border-radius: 999px;
      background-color: rgba(255, 255, 255, 0.22);
      background-clip: padding-box;
    }
    ::-webkit-scrollbar-thumb:hover, ::-webkit-scrollbar-thumb:active { background-color: rgba(255, 255, 255, 0.35); }
  `;
  (document.head || root).prepend(base);
  const setTheme = (t) => {
    if (!t) return;
    for (const key of ["bg", "panel", "accent"]) {
      if (/^#[0-9a-f]{6}$/i.test(t[key] || "")) root.style.setProperty(`--a2-${key}`, t[key]);
    }
  };

  const handlers = {};
  const pending = new Map();
  let seq = 0;
  const call = (name, args) =>
    new Promise((resolve, reject) => {
      const id = ++seq;
      pending.set(id, { resolve, reject });
      window.parent.postMessage({ a2: 1, id, call: name, args }, "*");
    });
  window.addEventListener("message", (event) => {
    if (event.source !== window.parent) return;
    const msg = event.data;
    if (!msg || msg.a2 !== 1) return;
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(msg.error));
      else resolve(msg.result);
      return;
    }
    if (msg.event) {
      if (msg.event === "theme") setTheme(msg.data);
      for (const fn of handlers[msg.event] || []) {
        try {
          fn(msg.data, { character: msg.character ?? null, at: msg.at ?? null });
        } catch (e) {
          console.error(e);
        }
      }
    }
  });
  window.a2 = Object.freeze({
    on(event, fn) {
      (handlers[event] ||= []).push(fn);
    },
    storage: Object.freeze({
      get: (key) => call("storage.get", [key]).then((v) => (v == null ? null : JSON.parse(v))),
      set: (key, value) => call("storage.set", [key, JSON.stringify(value)]),
    }),
    notify: (text) => call("notify", [String(text)]),
    bossTimers: (server) => call("bossTimers", [Number(server)]),
    // Meter 2.0.72 on: the server's Abyss siege (artifact holders, cores).
    abyssSiege: (server) => call("abyssSiege", [Number(server)]),
    followBosses: (server, bosses) => call("bossTimers.follow", [Number(server), (bosses || []).map(String)]),
    // Meter 2.0.78 on: the game's frame rate.
    watchFps: (on) => call("fps.watch", [on !== false]),
    screenshot: (rect) => call("screenshot", [rect || {}]),
    fpsAdmin: () => call("fps.admin", []),
    window: Object.freeze({
      show: () => call("window.show", []),
      hide: () => call("window.hide", []),
      fit: (height) => call("window.fit", [height ?? null]),
    }),
    widgets: Object.freeze({
      open: (name, size) => call("widgets.open", [String(name), size || {}]),
      close: (name) => call("widgets.close", [String(name)]),
      send: (name, data) => call("widgets.send", [String(name), data]),
      lock: (locked) => call("widgets.lock", [!!locked]),
      locked: () => call("widgets.locked", []),
      // Meter 2.0.72 on: where a widget is, and moving it (physical screen
      // pixels), for a plugin that lays its widgets out itself.
      where: (name) => call("widgets.where", [String(name)]),
      place: (name, at) => call("widgets.place", [String(name), { x: Number(at?.x), y: Number(at?.y) }]),
    }),
    widget: Object.freeze({
      name: new URLSearchParams(location.search).get("name") || "",
      drag: () => call("widget.drag", []),
      area: (r) => call("widget.area", [r || {}]),
      areas: (rs) => call("widget.areas", [Array.isArray(rs) ? rs : []]),
      post: (data) => call("widget.post", [data]),
    }),
    ready: () => call("ready", []),
  });
  // The frame may not open windows or leave its page: links go to the browser.
  document.addEventListener("click", (event) => {
    const link = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (!link || !/^https:\/\//i.test(link.href)) return;
    event.preventDefault();
    call("openUrl", [link.href]).catch((e) => console.warn(e));
  });
})();
