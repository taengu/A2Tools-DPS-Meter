const createI18n = ({
  defaultLanguage = "en",
  storageKey = "dpsMeter.language",
  supportedLanguages = [
    "en", "de", "es", "fr", "ja", "ko", "pt", "ru", "zh-Hant", "zh-Hans",
  ],
} = {}) => {
  let currentLanguage = defaultLanguage;
  let loadedLanguage = null;
  let pendingLoad = null;
  let languageRequest = 0;
  let uiStrings = {};
  let skillStrings = {};
  let npcStrings = {};
  // Instances whose bosses carry levels of their own (getNpcLevel); built on
  // first use from the NPC table.
  let multiTierDungeons = null;
  let dungeonStrings = {};
  const listeners = new Set();

  const safeGetStorage = (key) => {
    try {
      const bridgeValue = window.javaBridge?.getSetting?.(key);
      if (bridgeValue !== undefined && bridgeValue !== null) {
        return bridgeValue;
      }
    } catch {
      // ignore
    }
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  };

  const safeSetStorage = (key, value) => {
    try {
      window.javaBridge?.setSetting?.(key, value);
    } catch {
      // ignore
    }
    try {
      localStorage.setItem(key, value);
    } catch {
      // ignore
    }
  };

  const normalizeLanguage = (lang) =>
    supportedLanguages.includes(lang) ? lang : defaultLanguage;

  const resolveUrl = (path) => {
    try {
      return new URL(path, document.baseURI || window.location.href).toString();
    } catch {
      return path;
    }
  };

  const parseJsonText = (text) => {
    if (typeof text !== "string") return {};
    try {
      const parsed = JSON.parse(text);
      return parsed && typeof parsed === "object" ? parsed : {};
    } catch {
      return {};
    }
  };

  const normalizeBridgePath = (path) => {
    if (!path) return "/";
    const trimmed = path.startsWith("./") ? path.slice(2) : path;
    return trimmed.startsWith("/") ? trimmed : `/${trimmed}`;
  };

  const loadJsonFromBridge = (path) => {
    const raw = window.javaBridge?.readResource?.(normalizeBridgePath(path));
    return parseJsonText(raw);
  };

  const loadJson = async (path) => {
    const url = resolveUrl(path);
    try {
      const res = await fetch(url, { cache: "no-store" });
      if (res.ok || res.status === 0) {
        const buffer = await res.arrayBuffer();
        const text = new TextDecoder("utf-8").decode(buffer);
        const data = parseJsonText(text);
        if (Object.keys(data).length) return data;
      }
    } catch {
      // ignore and fall back
    }

    const xhrText = await new Promise((resolve) => {
      try {
        const xhr = new XMLHttpRequest();
        xhr.open("GET", url, true);
        xhr.responseType = "arraybuffer";
        xhr.onload = () => {
          if (xhr.status && xhr.status !== 200) {
            resolve(null);
            return;
          }
          if (!xhr.response) {
            resolve("");
            return;
          }
          try {
            const decoded = new TextDecoder("utf-8").decode(xhr.response);
            resolve(decoded);
          } catch {
            resolve("");
          }
        };
        xhr.onerror = () => resolve(null);
        xhr.send();
      } catch {
        resolve(null);
      }
    });

    if (xhrText) {
      const parsed = parseJsonText(xhrText);
      if (Object.keys(parsed).length) return parsed;
    }

    return loadJsonFromBridge(path);
  };

  const resolveKey = (obj, key) => {
    if (!obj || !key) return undefined;
    return key.split(".").reduce((acc, part) => (acc ? acc[part] : undefined), obj);
  };

  const t = (key, fallback = "") => {
    const value = resolveKey(uiStrings, key);
    if (typeof value === "string") return value;
    return fallback;
  };

  const format = (key, vars = {}, fallback = "") => {
    const template = t(key, fallback);
    if (!template) return fallback;
    return template.replace(/\{(\w+)\}/g, (_, varKey) => {
      const replacement = vars[varKey];
      return replacement === undefined || replacement === null ? "" : String(replacement);
    });
  };

  const getSkillName = (code, fallback = "") => {
    const value = skillStrings?.[String(code)];
    if (typeof value === "string" && value.trim()) return value;
    // Theostone DOT codes: 7-digit codes (3000000-3099999) map to 8-digit IDs (code*10+1)
    const num = Number(code);
    if (num >= 3000000 && num <= 3099999) {
      const tsValue = skillStrings?.[String(num * 10 + 1)];
      if (typeof tsValue === "string" && tsValue.trim()) return tsValue;
    }
    return fallback;
  };

  const getNpcName = (id, fallback = "") => {
    const value = npcStrings?.[String(id)];
    if (typeof value === "string" && value.trim()) return value;
    if (value && typeof value === "object") {
      const name = value?.name;
      if (typeof name === "string" && name.trim()) return name;
    }
    return fallback;
  };

  // A boss's own level, in the table's language ("Level 2", "2단계"), when
  // its instance has several: Nightmare is one instance whose bosses come in
  // ten levels, a code each (Gatekeeper Pinopi 2980040-2980049). Elsewhere a
  // boss's tier is the instance's, which the dungeon label already says.
  const getNpcLevel = (id) => {
    const npc = npcStrings?.[String(id)];
    if (!npc || typeof npc !== "object" || !npc.tier || !npc.dungeonId) return "";
    if (!multiTierDungeons) {
      const tiers = new Map();
      for (const value of Object.values(npcStrings || {})) {
        if (!value || typeof value !== "object" || !value.tier || !value.dungeonId) continue;
        if (!tiers.has(value.dungeonId)) tiers.set(value.dungeonId, new Set());
        tiers.get(value.dungeonId).add(value.tier);
      }
      multiTierDungeons = new Set([...tiers].filter(([, set]) => set.size > 1).map(([d]) => d));
    }
    return multiTierDungeons.has(npc.dungeonId) ? String(npc.tier) : "";
  };

  const applyTranslations = () => {
    document.querySelectorAll("[data-i18n]").forEach((el) => {
      const key = el.dataset.i18n;
      const text = t(key, el.textContent ?? "");
      if (text) el.textContent = text;
    });

    document.querySelectorAll("[data-i18n-tip]").forEach((el) => {
      const key = el.dataset.i18nTip;
      const text = t(key, el.getAttribute("data-tip") ?? "");
      if (text) el.setAttribute("data-tip", text);
    });

    // Native tooltips, for windows without the meter's own (data-tip) one.
    document.querySelectorAll("[data-i18n-title]").forEach((el) => {
      const key = el.dataset.i18nTitle;
      const text = t(key, el.getAttribute("title") ?? "");
      if (text) el.setAttribute("title", text);
    });

    document.querySelectorAll("[data-i18n-placeholder]").forEach((el) => {
      const key = el.dataset.i18nPlaceholder;
      const text = t(key, el.getAttribute("placeholder") ?? "");
      if (text) el.setAttribute("placeholder", text);
    });

    document.querySelectorAll("[data-i18n-aria-label]").forEach((el) => {
      const key = el.dataset.i18nAriaLabel;
      const text = t(key, el.getAttribute("aria-label") ?? "");
      if (text) el.setAttribute("aria-label", text);
    });
  };

  const applyLanguage = () => {
    document.documentElement.setAttribute("lang", currentLanguage);
    applyTranslations();
    listeners.forEach((listener) => listener(currentLanguage));
  };

  // getLanguage() reports the requested language at once; the latest request
  // wins when an older dictionary load finishes after it.
  const setLanguage = async (lang, { persist = true } = {}) => {
    const next = normalizeLanguage(lang || defaultLanguage);
    currentLanguage = next;

    if (persist) {
      safeSetStorage(storageKey, next);
    }

    if (pendingLoad?.language === next) return pendingLoad.promise;
    const request = ++languageRequest;
    pendingLoad = null;
    if (loadedLanguage === next) {
      // Same language: re-apply to the current DOM without reloading.
      applyLanguage();
      return;
    }
    const localized = async (kind) => {
      const strings = await loadJson(`./i18n/${kind}/${next}.json`);
      return Object.keys(strings).length || next === "en"
        ? strings : loadJson(`./i18n/${kind}/en.json`);
    };
    // Settings has no combat names. Do not put the megabyte-sized game
    // dictionaries (or missing dungeon-locale fallbacks) on its startup path.
    const uiOnly = window.A2_VIEW === "settings";
    const promise = Promise.all([
      localized("ui"),
      uiOnly ? {} : localized("skills"),
      uiOnly ? {} : localized("npcs"),
      uiOnly ? {} : localized("dungeons"),
    ]).then(([ui, skills, npcs, dungeons]) => {
      if (request !== languageRequest) return;
      pendingLoad = null;
      loadedLanguage = next;
      uiStrings = ui || {};
      skillStrings = skills || {};
      npcStrings = npcs || {};
      multiTierDungeons = null;
      dungeonStrings = dungeons || {};
      applyLanguage();
    }, (error) => {
      if (request === languageRequest) pendingLoad = null;
      throw error;
    });
    pendingLoad = { language: next, promise };
    return promise;
  };

  // Buff and debuff names (i18n/abnormals), loaded the first time Details
  // shows buffs, not at startup. There are none in Chinese: those read the
  // English ones, and say so (`native: false`) so a caller can prefer the
  // applying skill's Chinese name.
  const ABNORMAL_LANGUAGES = ["de", "en", "es", "fr", "ja", "ko", "pt", "ru"];
  const abnormalLoads = new Map();
  const loadAbnormalNames = () => {
    const native = ABNORMAL_LANGUAGES.includes(currentLanguage);
    const lang = native ? currentLanguage : "en";
    if (!abnormalLoads.has(lang)) {
      abnormalLoads.set(lang, loadJson(`./i18n/abnormals/${lang}.json`).then((names) =>
        Object.keys(names).length || lang === "en" ? names : loadJson("./i18n/abnormals/en.json")));
    }
    return abnormalLoads.get(lang).then((names) => ({ names, native }));
  };

  const init = async () => {
    // The backend preference wins over stale localStorage on a newly opened window.
    if (window.A2_VIEW === "settings") await window.a2SettingsReady;
    const stored = safeGetStorage(storageKey);
    await setLanguage(stored || defaultLanguage, { persist: false });
  };

  const onChange = (listener) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  };

  // The instance's tier ({ key: "hard", label: "Hard" }), or null when it is
  // not known. An instance id's last digit is its tier, as on a2tools.app:
  // Krao Cave is 600001-600004, Ferocious Horn Den 600091-600093. 1, 2 and 3
  // are Exploration, Conquest Normal and Conquest Hard; a dungeon with nine
  // ids (Deus Research Base, Shattered Arkanis) has levels instead; the older
  // dungeons' fourth id is not named yet.
  const getDungeonDifficulty = (dungeonId) => {
    const id = Number(dungeonId) || 0;
    const entry = dungeonStrings?.[String(id)];
    if (!entry) return null;
    const n = id % 10;
    const group = id - n;
    const size = Object.keys(dungeonStrings).filter((k) => Number(k) - (Number(k) % 10) === group).length;
    if (size >= 9) {
      const label = format("dungeon.difficulty.level", { n }, "");
      return label ? { key: "level", label } : null;
    }
    const key = entry.difficulty || { 1: "exploration", 2: "normal", 3: "hard" }[n];
    const label = key ? t(`dungeon.difficulty.${key}`, "") : "";
    return label ? { key, label } : null;
  };

  // "Ferocious Horn Den (Hard)" for the instance the party roster reports, or
  // the name alone where the tier is not known.
  // The dungeon's name alone, or "" when the table has no entry for the id.
  const getDungeonName = (dungeonId) => {
    const entry = dungeonStrings?.[String(Number(dungeonId) || 0)];
    return entry?.name ? String(entry.name) : "";
  };

  // The category the NPC table gives a boss ("Nightmare"), for instances the
  // dungeon table does not name.
  const getNpcCategory = (id) => {
    const npc = npcStrings?.[String(id)];
    return npc && typeof npc === "object" && npc.category ? String(npc.category) : "";
  };

  const getDungeonLabel = (dungeonId) => {
    const entry = dungeonStrings?.[String(dungeonId)];
    if (!entry || !entry.name) return "";
    const tier = getDungeonDifficulty(dungeonId);
    return tier ? `${entry.name} (${tier.label})` : entry.name;
  };

  return {
    init,
    setLanguage,
    t,
    format,
    getSkillName,
    getNpcName,
    getNpcLevel,
    loadAbnormalNames,
    getDungeonLabel,
    getDungeonName,
    getNpcCategory,
    getDungeonDifficulty,
    getLanguage: () => currentLanguage,
    onChange,
  };
};

window.i18n = createI18n();
