// One plugin's frame and the calls it may make: shared by a plugin's own
// window (pluginHost.js) and the plugin dock (pluginDock.js). The plugin runs
// in a sandboxed frame with an origin of its own and no access to the page or
// the meter; it asks for things by message, and this passes on only what the
// plugin's manifest permits.
//
// window.createPluginFrame({ id, stage, docked, compact, toast, onFit }) returns
//   { deliver(event, data), receive(payload), start(), setCompact(on), destroy() }
// `docked`: the page is the dock's, so every call names its plugin (the meter
// accepts that only for a plugin docked there). `receive` takes a
// "plugin-event" payload meant for this plugin. `start` resolves to the
// plugin's hello once the frame is in.
(() => {
  const { invoke, convertFileSrc } = window.__TAURI__.core;

  window.createPluginFrame = ({ id, stage, docked = false, compact: folded = false, toast = () => {}, onFit = () => {} }) => {
    let frame = null;
    let manifest = null;
    let ready = false;
    let compact = !!folded;
    const queue = [];
    // The dock names the plugin each call is for; a plugin's own window is
    // its plugin's.
    const call = (name, args = {}) => invoke(name, docked ? { ...args, plugin: id } : args);

    const allowed = (permission) => !!manifest?.permissions?.includes(permission);
    const send = (msg) => frame?.contentWindow?.postMessage({ a2: 1, ...msg }, "*");

    // Decoded events are numbered for the meter's run. The page remembers the
    // last one it handed the plugin, so a window opened again (or reloaded)
    // gets each event once: the ones it missed while closed, none twice.
    const CURSOR_KEY = `a2plugin.cursor.${id}`;
    let run = null;
    let generation = null;
    let delivered = 0;
    const saveCursor = () => {
      try { localStorage.setItem(CURSOR_KEY, JSON.stringify({ run, seq: delivered })); } catch {}
    };

    const deliver = (event, data, extra = {}) => {
      if (event.startsWith("decoded:") && !allowed(`decoder:${event.slice(8)}`)) return;
      if (event === "character" && !allowed("events:character")) return;
      if ((event === "bossMap" || event === "bossLive" || event === "abyss") && !allowed("feed:boss-timers")) return;
      if (event === "deathRecap" && !allowed("events:death-recap")) return;
      const msg = { event, data, ...extra };
      if (ready) send(msg);
      else queue.push(msg);
    };
    // A numbered event from the meter: skipped if already handed over.
    const deliverLogged = (e) => {
      if (e?.generation !== generation) return;
      if (typeof e?.seq !== "number" || e.seq <= delivered) return;
      delivered = e.seq;
      deliver(e.event, e.data, { character: e.character ?? null, at: e.at ?? null });
      if (ready) saveCursor();
    };
    // Events that arrive before the page knows the run and the manifest.
    let early = [];

    const calls = {
      ready: () => {
        // Ready again: the plugin's page started over (reloaded), and lost
        // what it was told at start. Told again, as now: without it Ping
        // took the game for closed and kept its widget hidden.
        if (ready) {
          call("plugin_hello").then((hello) => {
            for (const [event, data] of startState(hello)) send({ event, data });
          }).catch(() => {});
          return true;
        }
        ready = true;
        for (const m of queue.splice(0)) send(m);
        saveCursor();
        return true;
      },
      "storage.get": ([key]) => call("plugin_storage_get", { key: String(key) }),
      // The field boss timers players' meters share, for one server.
      bossTimers: ([server]) => {
        if (!allowed("feed:boss-timers")) throw new Error('the plugin lacks the "feed:boss-timers" permission');
        return call("plugin_boss_timers", { server: Number(server) });
      },
      // A server's Abyss siege: the artifacts' holders and the Dimensional
      // Cores (meter 2.0.72).
      abyssSiege: ([server]) => {
        if (!allowed("feed:boss-timers")) throw new Error('the plugin lacks the "feed:boss-timers" permission');
        return call("plugin_abyss_siege", { server: Number(server) });
      },
      // Follow some of a server's field bosses: each change comes at once as a
      // "bossLive" event (meter 2.0.68). An empty list stops following.
      "bossTimers.follow": ([server, bosses]) => {
        if (!allowed("feed:boss-timers")) throw new Error('the plugin lacks the "feed:boss-timers" permission');
        return call("plugin_boss_follow", { server: Number(server), bosses: (Array.isArray(bosses) ? bosses : []).map(String) });
      },
      // The game's frame rate (meter 2.0.78): "fps" events while asked.
      "fps.watch": ([on]) => {
        if (!allowed("fps")) throw new Error('the plugin lacks the "fps" permission');
        return call("plugin_fps_watch", { on: !!on });
      },
      // Run the meter as administrator, for the frame rate: the meter asks.
      "fps.admin": () => {
        if (!allowed("fps")) throw new Error('the plugin lacks the "fps" permission');
        return call("plugin_fps_admin");
      },
      "storage.set": ([key, value]) => call("plugin_storage_set", { key: String(key), value: String(value) }),
      "window.show": () => {
        if (!allowed("window")) throw new Error('the plugin lacks the "window" permission');
        return call("plugin_window", { visible: true });
      },
      // Fit the window to a height of content (an alert, say), its title bar
      // hidden meanwhile; or put its size and title bar back (no height). In
      // the dock, its tab opens instead (plugins::plugin_window_fit).
      "window.fit": ([height]) => {
        if (!allowed("window")) throw new Error('the plugin lacks the "window" permission');
        const h = Number(height);
        const fitted = Number.isFinite(h) && h > 0;
        onFit(fitted);
        return call("plugin_window_fit", { height: fitted ? h : null });
      },
      // A screenshot of part of the plugin's page (permission "screenshot"):
      // its rect, from the frame's corner, measured here from the window's.
      screenshot: ([r]) => {
        if (!allowed("screenshot")) throw new Error('the plugin lacks the "screenshot" permission');
        const f = frame?.getBoundingClientRect();
        if (!f) throw new Error("no frame");
        const x = Math.max(0, Number(r?.x) || 0), y = Math.max(0, Number(r?.y) || 0);
        const width = Math.min(Number(r?.width) || f.width, f.width - x), height = Math.min(Number(r?.height) || f.height, f.height - y);
        return call("plugin_screenshot", { x: f.left + x, y: f.top + y, width, height, scale: window.devicePixelRatio || 1 });
      },
      "window.hide": () => {
        if (!allowed("window")) throw new Error('the plugin lacks the "window" permission');
        return call("plugin_window", { visible: false });
      },
      // Small windows of the plugin's own, which the player can move and lock
      // (plugin widgets, #77; permission "widgets").
      "widgets.open": ([name, size]) => {
        if (!allowed("widgets")) throw new Error('the plugin lacks the "widgets" permission');
        return call("plugin_widget_open", { name: String(name), width: Number(size?.width) || 64, height: Number(size?.height) || 64 });
      },
      "widgets.close": ([name]) => {
        if (!allowed("widgets")) throw new Error('the plugin lacks the "widgets" permission');
        return call("plugin_widget_close", { name: String(name) });
      },
      "widgets.send": ([name, data]) => {
        if (!allowed("widgets")) throw new Error('the plugin lacks the "widgets" permission');
        return call("plugin_widget_send", { name: String(name), data: data ?? null });
      },
      "widgets.lock": ([locked]) => {
        if (!allowed("widgets")) throw new Error('the plugin lacks the "widgets" permission');
        return call("plugin_widgets_lock", { locked: !!locked });
      },
      "widgets.locked": () => {
        if (!allowed("widgets")) throw new Error('the plugin lacks the "widgets" permission');
        return call("plugin_widgets_locked");
      },
      "widgets.where": ([name]) => {
        if (!allowed("widgets")) throw new Error('the plugin lacks the "widgets" permission');
        return call("plugin_widget_where", { name: String(name) });
      },
      "widgets.place": ([name, at]) => {
        if (!allowed("widgets")) throw new Error('the plugin lacks the "widgets" permission');
        return call("plugin_widget_place", { name: String(name), x: Math.round(Number(at?.x)), y: Math.round(Number(at?.y)) });
      },
      notify: ([text]) => {
        if (!allowed("notify")) throw new Error('the plugin lacks the "notify" permission');
        toast(String(text));
        return true;
      },
      // The native host asks before opening the exact destination. A message
      // from this frame alone cannot establish that the player clicked a link.
      openUrl: ([raw]) => {
        const url = new URL(String(raw));
        if (url.protocol !== "https:" || url.username || url.password || url.href.length > 2048) {
          throw new Error("only https links open");
        }
        const now = Date.now();
        if (now - lastLinkAt < 1000) throw new Error("one link a second");
        lastLinkAt = now;
        return call("plugin_open_url", { url: url.href });
      },
    };
    let lastLinkAt = 0;
    let pendingCalls = 0;
    let callTokens = 120;
    let tokenTime = Date.now();

    // Only this plugin's frame speaks for it: the source is checked against
    // its frame's window, whatever the message says.
    const onMessage = async (event) => {
      if (!frame || event.source !== frame.contentWindow) return;
      const msg = event.data;
      if (!msg || msg.a2 !== 1 || !msg.id || typeof msg.call !== "string") return;
      if (!['string', 'number'].includes(typeof msg.id) || String(msg.id).length > 128 || msg.call.length > 80) return;
      const fn = calls[msg.call];
      const now = Date.now();
      callTokens = Math.min(120, callTokens + Math.max(0, now - tokenTime) * 0.06);
      tokenTime = now;
      if (pendingCalls >= 64 || callTokens < 1) {
        send({ id: msg.id, error: 'too many plugin requests; wait before retrying' });
        return;
      }
      callTokens -= 1;
      try {
        if (JSON.stringify(msg).length > 2 * 1024 * 1024 + 4096) throw new Error('plugin request too large');
      } catch {
        send({ id: msg.id, error: 'plugin request too large or not serializable' });
        return;
      }
      pendingCalls += 1;
      try {
        if (!fn) throw new Error(`unknown call ${msg.call}`);
        send({ id: msg.id, result: await fn(Array.isArray(msg.args) ? msg.args : []) });
      } catch (e) {
        send({ id: msg.id, error: String(e?.message || e) });
      } finally {
        pendingCalls -= 1;
      }
    };
    window.addEventListener("message", onMessage);

    // A "plugin-event" for this plugin (the page has checked it is).
    const receive = (payload) => {
      if (typeof payload?.event !== "string") return;
      // Keep the original epoch until hello establishes this instance's.
      // Passing an early unnumbered event straight to deliver loses it.
      if (early) {
        if (early.length < 2048) early.push(payload);
        return;
      }
      if (generation !== null && typeof payload.generation === "number" && payload.generation !== generation) return;
      // Sent to this plugin's widgets, not to its window.
      if (payload.event === "widget" || payload.event === "locked") return;
      if ((payload.event === "widget-message" || payload.event === "widget-moved") && !allowed("widgets")) return;
      if (typeof payload.seq !== "number") deliver(payload.event, payload.data);
      else deliverLogged(payload);
    };

    // What a plugin is told as it starts, from its hello: [event, data] each.
    const startState = (hello) => {
      const out = [["language", hello.language || "en"], ["theme", hello.theme], ["compact", compact]];
      if (hello.character && allowed("events:character")) out.push(["character", hello.character]);
      if (typeof hello.game === "boolean") out.push(["game", { running: hello.game }]);
      // Its window (or dock tab) shown or hidden (meter 2.0.77 on).
      if (typeof hello.visible === "boolean") out.push(["visible", hello.visible]);
      // The ping to the game server (meter 2.0.73 on); null until it is known.
      if ("ping" in hello) out.push(["ping", { ms: typeof hello.ping === "number" ? hello.ping : null }]);
      return out;
    };

    const start = () =>
      // Fresh: the meter closes widgets an earlier run of the plugin left.
      call("plugin_hello", { fresh: true }).then((hello) => {
        manifest = hello.manifest;
        for (const [event, data] of startState(hello)) deliver(event, data);
        run = hello.run;
        generation = hello.generation;
        try {
          const cursor = JSON.parse(localStorage.getItem(CURSOR_KEY) || "null");
          if (cursor?.run === run) delivered = Number(cursor.seq) || 0;
        } catch {}
        const arrived = early;
        const logged = [...(hello.replay || []), ...arrived.filter(e => typeof e.seq === "number")].sort((a, b) => a.seq - b.seq);
        early = null;
        for (const e of logged) deliverLogged(e);
        for (const e of arrived.filter(e => typeof e.seq !== "number")) receive(e);
        frame = document.createElement("iframe");
        frame.setAttribute("sandbox", "allow-scripts");
        frame.setAttribute("referrerpolicy", "no-referrer");
        // The scheme's base (http://a2plugin.localhost/ on Windows), then a plain
        // path: convertFileSrc would encode its "/", and the page's relative
        // links (style.css, plugin.js) would then resolve outside its folder.
        const base = convertFileSrc("", "a2plugin").replace(/\/*$/, "/");
        frame.src = `${base}${encodeURIComponent(id)}/index.html`;
        stage.appendChild(frame);
        return hello;
      });

    return {
      deliver,
      receive,
      start,
      // The fold button ("compact"), which the plugin hears and handles itself.
      setCompact(on) {
        compact = !!on;
        deliver("compact", compact);
      },
      destroy() {
        window.removeEventListener("message", onMessage);
        frame?.remove();
        frame = null;
      },
    };
  };
})();
