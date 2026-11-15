// A plugin widget's host page (src-tauri/src/plugins/widgets.rs): the
// plugin's widget.html in a sandboxed frame, as in pluginHost.js. A widget can
// ask to be dragged and post a message to its plugin; it hears what its
// plugin sends it ("widget"), whether its plugin's widgets are locked
// ("locked"), and the meter's language.
(() => {
  const { invoke, convertFileSrc } = window.__TAURI__.core;
  const id = window.__A2_PLUGIN_ID__;
  const name = window.__A2_WIDGET__;
  let frame = null;
  let ready = false;
  const queue = [];
  const send = (msg) => frame?.contentWindow?.postMessage({ a2: 1, ...msg }, "*");
  const deliver = (event, data) => {
    const msg = { event, data };
    if (ready) send(msg);
    else queue.push(msg);
  };

  const rectOf = (r) => ({ x: Number(r?.x), y: Number(r?.y), width: Number(r?.width), height: Number(r?.height) });

  const calls = {
    ready: () => {
      ready = true;
      for (const m of queue.splice(0)) send(m);
      return true;
    },
    "widget.drag": () => invoke("plugin_widget_drag"),
    // The frame fills the window, so its CSS px are the window's.
    "widget.area": ([r]) => invoke("plugin_widget_area", {
      rects: [rectOf(r)],
      scale: window.devicePixelRatio || 1,
    }),
    // Several parts: the gaps between them let clicks through too.
    "widget.areas": ([rs]) => invoke("plugin_widget_area", {
      rects: (Array.isArray(rs) ? rs : []).slice(0, 64).map(rectOf),
      scale: window.devicePixelRatio || 1,
    }),
    "widget.post": ([data]) => invoke("plugin_widget_post", { data: data ?? null }),
  };

  window.addEventListener("message", async (event) => {
    if (!frame || event.source !== frame.contentWindow) return;
    const msg = event.data;
    if (!msg || msg.a2 !== 1 || !msg.id || typeof msg.call !== "string") return;
    const fn = calls[msg.call];
    try {
      if (!fn) throw new Error(`unknown call ${msg.call}`);
      send({ id: msg.id, result: await fn(Array.isArray(msg.args) ? msg.args : []) });
    } catch (e) {
      send({ id: msg.id, error: String(e?.message || e) });
    }
  });

  // Only events sent to this window: a plain `listen` hears every window's.
  const current = window.__TAURI__.webviewWindow.getCurrentWebviewWindow();
  current.listen("plugin-event", (event) => {
    const payload = event.payload || {};
    if (payload.plugin !== id || typeof payload.event !== "string") return;
    if (payload.event === "widget" || payload.event === "locked" || payload.event === "language") {
      deliver(payload.event, payload.data);
    }
    // The layout being edited (layout.rs): outlined, and its widget told.
    if (payload.event === "layout") {
      setLayout(!!payload.data?.editing);
      deliver("layout", payload.data);
    }
  });

  // An outline and the widget's name while the layout is edited, over the
  // widget, so even an empty or see-through one can be found and dragged.
  const layoutFrame = document.createElement("div");
  layoutFrame.className = "layoutFrame";
  const layoutName = document.createElement("span");
  layoutFrame.appendChild(layoutName);
  layoutFrame.addEventListener("mousedown", (event) => {
    if (event.button === 0) invoke("plugin_widget_drag").catch(() => {});
  });
  document.body.appendChild(layoutFrame);
  function setLayout(on) {
    document.body.classList.toggle("layoutEditing", on);
  }

  invoke("plugin_widget_hello").then((hello) => {
    document.title = name;
    layoutName.textContent = `${hello.pluginName || id} · ${name}`;
    layoutFrame.title = layoutName.textContent;
    setLayout(!!hello.layout);
    deliver("language", hello.language || "en");
    deliver("locked", !!hello.locked);
    if (hello.layout) deliver("layout", { editing: true });
    frame = document.createElement("iframe");
    frame.setAttribute("sandbox", "allow-scripts");
    // Copying text on a click (Event Timers' double right-click on an icon);
    // the browser allows it only during one, so no widget fills it unasked.
    frame.setAttribute("allow", "clipboard-write");
    frame.setAttribute("referrerpolicy", "no-referrer");
    frame.setAttribute("allowtransparency", "true");
    const base = convertFileSrc("", "a2plugin").replace(/\/*$/, "/");
    frame.src = `${base}${encodeURIComponent(id)}/widget.html?name=${encodeURIComponent(name)}`;
    document.body.appendChild(frame);
  }).catch(() => {});
})();
