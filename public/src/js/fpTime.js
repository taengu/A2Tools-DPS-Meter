// Flight power, for the footer's FP timer and the FP pin: the backend's last
// reading (capture/flight_power.rs, in hundredths) carried on to now.
// In flight: the flight time left at the drain seen. Otherwise: the time
// until full. Null when there is nothing to show (full, or the maximum not
// known yet).
window.fpTime = (fp, max) => {
  if (!fp || typeof fp.current !== "number" || !(max > 0)) return null;
  const since = Math.max(0, Date.now() - Number(fp.atMs || 0));
  // Readings come every ~0.1 s in flight: one this old means you landed.
  if (fp.flying && since < 1500) {
    const drain = Number(fp.drainPerS) || 0;
    if (!(drain > 0)) return null;
    const now = Math.max(0, fp.current - (drain * since) / 1000);
    return { draining: true, ms: (now / drain) * 1000, now, max };
  }
  const rate = Number(fp.ratePerS) || 0;
  if (!(rate > 0)) return null;
  const now = Math.min(max, fp.current + (rate * since) / 1000);
  const ms = ((max - now) / rate) * 1000;
  return ms < 500 ? null : { draining: false, ms, now, max };
};

window.fpTimeText = (ms) => {
  const sec = Math.ceil(Math.max(0, ms) / 1000);
  return `${Math.floor(sec / 60)}:${String(sec % 60).padStart(2, "0")}`;
};
