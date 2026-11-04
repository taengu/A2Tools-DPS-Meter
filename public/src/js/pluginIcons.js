// The reviewed plugins' icons (Lucide) and colours (a hue), by id: Settings
// → Plugins and the plugin dock's tabs. Any other plugin shows its first
// letter, in a colour from its id.
window.A2_PLUGIN_ICONS = Object.freeze({
  "pet-souls": ["paw-print", 25],
  "event-timers": ["alarm-clock", 45],
  ping: ["activity", 140],
  cooldowns: ["hourglass", 265],
  checklist: ["list-checks", 185],
  "heal-alert": ["heart-pulse", 350],
  "mantra-watch": ["orbit", 215],
  "hidden-cubes": ["box", 300],
  // Calls a boss's mechanics out loud; yellow, as its callout text.
  "boss-telegraph": ["megaphone", 75],
  // Every character you play, side by side.
  "alt-roster": ["users", 160],
  // Who killed you; red, as its "Killed by".
  "death-recap": ["swords", 0],
  // Dice Roll loot; gold, as its winner.
  "dice-rolls": ["dices", 45],
});

// A plugin's glyph (a Lucide name) or null, and its hue.
window.A2_PLUGIN_ICON = (id, name) => {
  let hash = 0;
  for (const ch of String(id)) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  const [glyph, hue] = window.A2_PLUGIN_ICONS[id] || [];
  return { glyph: glyph || null, hue: hue ?? hash % 360, letter: (String(name || id).trim()[0] || "?").toUpperCase() };
};
