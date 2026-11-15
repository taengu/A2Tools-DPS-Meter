// The layout editor's bar (layout-bar.html, layout.rs): snap on or off, the
// grid's step, and Cancel (every piece back) or Done (the new places kept).
// Esc cancels, Enter is Done.
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const snapBtn = document.querySelector(".snap");
  const gridBtn = document.querySelector(".grid");
  const GRIDS = [0, 8, 16, 32];
  let words = {};
  let snap = true;
  let grid = 0;

  const t = (key, fallback) => (typeof words[key] === "string" ? words[key] : fallback);
  const render = () => {
    snapBtn.setAttribute("aria-pressed", String(snap));
    gridBtn.setAttribute("aria-pressed", String(grid > 0));
    gridBtn.textContent = grid > 0
      ? t("gridOn", "Grid: {px} px").replace("{px}", String(grid))
      : t("gridOff", "Grid: off");
    for (const el of document.querySelectorAll("[data-key]")) {
      el.textContent = t(el.dataset.key, el.textContent);
    }
  };
  const save = () => invoke?.("layout_set_options", { snap, grid }).catch(() => {});
  const stop = (keep) => invoke?.("layout_edit_stop", { keep }).catch(() => {});

  snapBtn.addEventListener("click", () => { snap = !snap; render(); save(); });
  gridBtn.addEventListener("click", () => {
    grid = GRIDS[(GRIDS.indexOf(grid) + 1) % GRIDS.length];
    render();
    save();
  });
  document.querySelector(".cancel").addEventListener("click", () => stop(false));
  document.querySelector(".done").addEventListener("click", () => stop(true));
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") stop(false);
    if (event.key === "Enter") stop(true);
  });
  // The bar moves too, by anything that isn't a button.
  document.addEventListener("mousedown", (event) => {
    if (event.button === 0 && !event.target.closest("button")) invoke?.("layout_bar_drag").catch(() => {});
  });

  invoke?.("layout_options").then(async (options) => {
    snap = options?.snap !== false;
    grid = GRIDS.includes(Number(options?.grid)) ? Number(options.grid) : 0;
    try {
      const response = await fetch(`/i18n/ui/${encodeURIComponent(options?.language || "en")}.json`);
      words = (await response.json())?.layout || {};
    } catch {
      words = {};
    }
    render();
  }).catch(() => render());
  render();
})();
