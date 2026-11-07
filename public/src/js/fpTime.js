// Flight power, for the footer's FP timer and the FP pin: the backend's last
// reading (capture/flight_power.rs, in hundredths) carried on to now.
// In flight: the flight time left at the drain seen. Otherwise: the time
// until full. Null when there is nothing to show (full, or the maximum not
// known yet).
window.fpTime = (fp, max) => {
  if (!fp || typeof fp.current !== "number" || !(max > 0)) return null;
  const since = Math.max(0, Date.now() - Number(fp.atMs || 0));
  // Readings come several a second in the air: one this old means you landed.
  // A glide whose fall the refill (still going in the air) about cancels
  // out is no countdown: the time to full shows instead.
  const drain = Number(fp.drainPerS) || 0;
  if (fp.flying && since < 1500 && drain >= 200) {
    const now = Math.max(0, fp.current - (drain * since) / 1000);
    return { draining: true, ms: (now / drain) * 1000, now, max, drain };
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

// What the FP timer shows: it counts down at real time, one second a second.
// A change in how fast FP goes (gliding, flying, taking off, landing, a Wind
// Serum) shows at once; within one way of flying, each fresh reading only
// nudges it, so the refill's +40 a second and a packet's timing don't make
// the seconds skip back and forth.
window.fpSmoother = () => {
  let shown = null;
  return (left) => {
    const now = Date.now();
    if (!left) {
      shown = null;
      return null;
    }
    const changed = !shown
      || shown.draining !== left.draining
      || (left.draining && Math.abs(left.drain - shown.drain) > shown.drain * 0.08);
    let ms;
    if (changed) {
      ms = left.ms;
    } else {
      const elapsed = Math.max(0, now - shown.at);
      ms = shown.ms - elapsed;
      const off = left.ms - ms;
      if (Math.abs(off) > 3000) ms = left.ms;
      else ms += off * Math.min(1, elapsed / 2500);
    }
    shown = { draining: left.draining, drain: left.drain, ms: Math.max(0, ms), at: now };
    return { ...left, ms: shown.ms };
  };
};
