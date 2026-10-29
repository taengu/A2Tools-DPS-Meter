(() => {
  const START_DELAY = 800;

  // One check: "installing" (accepted in the update window), "update" (one
  // is out: declined, or this install's package manager updates it),
  // "current" or "failed". `latest`: the newest version, when known.
  // `ask` false: no window; "offer" when this install can take the update,
  // kept for `offer` (the green button in the meter's title).
  // One check, done by the meter (2.0.80 on: it fetches the manifest, checks
  // its signature and that it is newer, update_check.rs): "installing"
  // (accepted in the update window), "update" (one is out: declined, or this
  // install's package manager updates it), "current" or "failed". `latest`:
  // the newest version, when known. `ask` false: no window; "offer" when this
  // install can take the update, for `offer` (the green button in the title).
  const check = async ({ ask = true } = {}) => {
    const invoke = window.__TAURI__?.core?.invoke;
    if (!invoke) return { state: "failed" };
    try {
      const found = await invoke("check_for_update", { ask });
      if (found?.state === "offer") pending = true;
      return { state: found?.state || "failed", latest: found?.latest ? `v${found.latest}` : undefined,
        message: typeof found?.message === "string" ? found.message : undefined };
    } catch (e) {
      console.error("[A2Tools] Update check failed:", e);
      return { state: "failed" };
    }
  };

  let once = false;
  // The update found at start, for the title's button to offer.
  let pending = null;

  // At start nothing pops up: an update this install can take shows the
  // green button in the meter's title (`onAvailable`), which asks. Then
  // again every 6 hours, for a meter left running for days.
  const RECHECK_MS = 6 * 60 * 60 * 1000;
  const start = (onAvailable) => {
    const run = async () => {
      try {
        const found = await check({ ask: false });
        if (found.state === "offer") onAvailable?.(found.latest);
      } catch (e) {
        console.error("[A2Tools] Update check error:", e);
      }
    };
    setTimeout(() => {
      if (once) return;
      once = true;
      run();
      setInterval(run, RECHECK_MS);
    }, START_DELAY);
  };

  // The button: the update window for the update found at start. True when
  // it is installing.
  const offer = async () => {
    if (!pending) return false;
    const found = await check({ ask: true });
    const accepted = found.state === "installing";
    if (accepted) pending = null;
    return accepted;
  };

  // Settings' check: whenever asked, with what it found.
  const checkNow = () =>
    check().catch((e) => {
      console.error("[A2Tools] Update check error:", e);
      return { state: "failed" };
    });

  window.ReleaseChecker = { start, checkNow, offer };
})();
