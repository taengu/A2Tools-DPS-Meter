// The flight power pin's page (fp-pin.html): the backend's flight-power
// updates as big text, counted on between them. The maximum comes from the
// update, or the one the meter keeps per character (core.js, fpMaxLast).
(() => {
  const pin = document.querySelector(".pin");
  const timeEl = document.querySelector(".time");
  const invoke = window.__TAURI__?.core?.invoke;
  let fp = null;
  const smooth = window.fpSmoother();

  const maxOf = (reading) => {
    if (Number(reading?.max) > 0) return Number(reading.max);
    try {
      const saved = JSON.parse(localStorage.getItem("dpsMeter.fpMaxLast") || "{}") || {};
      return Number(saved[String(reading?.name ?? "").trim()]) || 0;
    } catch {
      return 0;
    }
  };

  const render = () => {
    const t = smooth(window.fpTime(fp, maxOf(fp)));
    if (!t) {
      pin.classList.remove("isShown");
      // Nothing to show, but the layout being edited: a stand-in to place.
      if (document.body.classList.contains("layoutEditing") && !timeEl.textContent) timeEl.textContent = "FP";
      return;
    }
    const text = window.fpTimeText(t.ms);
    if (timeEl.textContent !== text) timeEl.textContent = text;
    pin.classList.toggle("isDraining", t.draining);
    pin.classList.add("isShown");
  };

  window.__TAURI__?.event?.listen?.("flight-power", (event) => {
    fp = event.payload;
    render();
  });
  setInterval(render, 100);
  window.__TAURI__?.event?.listen?.("layout-editing", (event) => {
    document.body.classList.toggle("layoutEditing", !!event.payload);
    render();
  });

  document.addEventListener("mousedown", (event) => {
    if (event.button === 0) invoke?.("fp_pin_drag").catch(() => {});
  });
})();
