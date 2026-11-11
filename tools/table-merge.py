"""Add what a new game client has to the meter's NPC, skill, buff and dungeon data.

    python tools/table-merge.py <TW Table folder> [<Global Table folder>]

Reads the extracted client tables (NpcData, Skill, SkillAbnormal, Dungeon and
L10N; Aion2DataExtractor). Only adds: ids already in src/data keep their names
and flags, so the hand-made overrides stay (isBoss flips, our English).

- NPCs: every named NPC not yet listed, a boss when its NpcSubType is
  HeroMonster or RvRHeroMonster (what 1,376 of our 1,395 boss flags say).
  New dungeon bosses are linked by the patterns the listed ones follow:
  NightMare_<Boss>_<NN> is Nightmare (200003) Level NN; *_G_<n> dungeons are
  Transcendence Stage n; a boss named after a dungeon's map
  (M_PD_<Map>_..., difficulty by suffix) is that dungeon's.
- Skills and buffs: the name of every id not yet listed.
- Dungeons: names of new party dungeons; tiers and final bosses in
  dungeon_bosses.json as its note describes.
Korean, English and Traditional Chinese come from the TW client; Simplified
is converted from Traditional; de/es/fr/ja/pt/ru from the Global client's
text where it has the key, else English.
"""

import json
import pathlib
import re
import sys

import opencc

ROOT = pathlib.Path(__file__).resolve().parents[1]
DATA = ROOT / "src" / "data"
I18N = DATA / "i18n"
TW = pathlib.Path(sys.argv[1])
GLOBAL = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else None
LANGS = ["de", "en", "es", "fr", "ja", "ko", "pt", "ru", "zh-Hans", "zh-Hant"]
GLOBAL_DIRS = {"de": "de-DE", "es": "es-ES", "fr": "fr-FR", "ja": "ja-JP", "pt": "pt-BR", "ru": "ru-RU"}
t2s = opencc.OpenCC("t2s")


def table(name):
    return json.loads((TW / f"{name}.json").read_text(encoding="utf-8"))["Properties"]["Data"]


def l10n(folder, lang):
    path = folder / "L10N" / lang / "L10NString.json"
    return json.loads(path.read_text(encoding="utf-8"))["Entries"] if path.is_file() else {}


TEXT = {"en": l10n(TW, "en-US"), "ko": l10n(TW, "ko-KR"), "zh-Hant": l10n(TW, "zh-TW")}
for lang, d in GLOBAL_DIRS.items():
    TEXT[lang] = l10n(GLOBAL, d) if GLOBAL else {}


def usable(s):
    return isinstance(s, str) and s.strip() and not re.fullmatch(r"\?+|None|\[DNT\].*", s.strip())


def texts(key):
    """The text of an L10N key in every language, or None without English."""
    en = TEXT["en"].get(key)
    if not usable(en):
        return None
    out = {"en": en.strip()}
    for lang in ("ko", "zh-Hant", *GLOBAL_DIRS):
        v = TEXT[lang].get(key)
        out[lang] = v.strip() if usable(v) else out["en"]
    out["zh-Hans"] = t2s.convert(out["zh-Hant"])
    return out


def load(path):
    raw = path.read_text(encoding="utf-8")
    return json.loads(raw), ("\r\n" if "\r\n" in raw else "\n"), raw.endswith("\n")


def save(path, data, nl, trailing, indent=2):
    text = json.dumps(data, ensure_ascii=False, indent=indent).replace("\n", nl)
    path.write_text(text + (nl if trailing else ""), encoding="utf-8", newline="")


def by_id(d):
    return dict(sorted(d.items(), key=lambda kv: int(kv[0])))


# --- dungeons ---------------------------------------------------------------
dungeons = table("Dungeon")
dungeon_by_name = {r["Name"]: r for r in dungeons}
DIFFICULTY = {"EDungeonDifficulty::Easy": "exploration", "EDungeonDifficulty::Normal": "normal",
              "EDungeonDifficulty::Advanced": "hard", "EDungeonDifficulty::Hard": "hard"}
dungeon_files = {lang: load(I18N / "dungeons" / f"{lang}.json") for lang in ("en", "ko", "zh-Hans", "zh-Hant")}
added_dungeons = []
for r in dungeons:
    if r["DungeonType"] != "EDungeonType::Party":
        continue
    did = str(r["ID"]["Value"])
    if did in dungeon_files["en"][0]:
        continue
    names = texts("String_" + r["Title"]["Key"] + "_body")
    if not names:
        continue
    for lang, (d, _, _) in dungeon_files.items():
        entry = {"name": names[lang]}
        if r["DungeonDifficulty"] in DIFFICULTY and not re.search(r"_G_\d+$", r["Name"]):
            entry["difficulty"] = DIFFICULTY[r["DungeonDifficulty"]]
        d[did] = entry
    added_dungeons.append(did)
for lang, (d, nl, tr) in dungeon_files.items():
    save(I18N / "dungeons" / f"{lang}.json", by_id(d), nl, tr)

bosses_path = DATA / "dungeon_bosses.json"
bosses, b_nl, b_tr = load(bosses_path)
for r in dungeons:
    m = re.search(r"PartyDungeon_(\d+)Tier", r.get("PartDungeonTier") or "")
    if m and str(r["ID"]["Value"]) not in bosses["tiers"]:
        bosses["tiers"][str(r["ID"]["Value"])] = int(m.group(1))
bosses["tiers"] = by_id(bosses["tiers"])

# --- NPCs -------------------------------------------------------------------
npc_files = {lang: load(I18N / "npcs" / f"{lang}.json") for lang in LANGS}
listed = npc_files["en"][0]
# Localised category and tier words, as the listed entries have them.
words = {}
for nid, e in listed.items():
    for field in ("category", "tier"):
        if e.get(field):
            for lang in LANGS:
                other = npc_files[lang][0].get(nid, {}).get(field)
                if other:
                    words.setdefault((field, e[field]), {}).setdefault(lang, other)


def link(name):
    """(dungeon id, category, tier) for a new boss, from its internal name."""
    m = re.match(r"NightMare_.+_(\d{3})$", name)
    if m and 1 <= int(m.group(1)) <= 10:
        return 200003, "Nightmare", f"Level {int(m.group(1))}"
    m = re.match(r"M_PD_([A-Za-z]+)_.*?_Grade(\d)$", name)
    if m:
        for dname, r in dungeon_by_name.items():
            if dname.startswith(m.group(1)) and dname.endswith(f"_G_{int(m.group(2)):02d}"):
                return r["ID"]["Value"], "Transcendence", f"Stage {int(m.group(2))}"
    m = re.match(r"M_PD_([A-Za-z]+)_.*?(_Hard|_hard|_T_01)?$", name)
    if m:
        suffix = {"_Hard": "Hard", "_hard": "Hard", "_T_01": "T_01"}.get(m.group(2) or "", "Normal")
        prefix = m.group(1)
        for key in (f"{prefix}_{suffix}", f"{prefix}_{suffix}".replace("Fire_T_01", "Fire_Temple_T_01")):
            if key in dungeon_by_name:
                return dungeon_by_name[key]["ID"]["Value"], "Dungeon", None
        if prefix == "Fire" and suffix == "T_01" and "Fire_Temple_T_01" in dungeon_by_name:
            return dungeon_by_name["Fire_Temple_T_01"]["ID"]["Value"], "Dungeon", None
    return None


added_npcs = linked = 0
for r in table("NpcData"):
    nid = str(r["ID"]["Value"])
    if nid in listed:
        continue
    names = texts("String_" + r["Desc"]["Key"] + "_body")
    if not names:
        continue
    boss = r.get("NpcSubType") in ("ENpcSubType::HeroMonster", "ENpcSubType::RvRHeroMonster")
    where = link(r["Name"]) if boss else None
    for lang in LANGS:
        entry = {"name": names[lang], "isBoss": boss}
        if where:
            did, category, tier = where
            entry["category"] = words.get(("category", category), {}).get(lang, category)
            if tier:
                entry["tier"] = words.get(("tier", tier), {}).get(lang, tier)
            entry["dungeonId"] = did
        npc_files[lang][0][nid] = entry
    added_npcs += 1
    if where:
        linked += 1
        if r.get("Grade") == 2 and r.get("bNamed") and where[1] == "Dungeon" and r["ID"]["Value"] not in bosses["finalBosses"]:
            bosses["finalBosses"].append(r["ID"]["Value"])
for lang, (d, nl, tr) in npc_files.items():
    save(I18N / "npcs" / f"{lang}.json", by_id(d), nl, tr)
bosses["finalBosses"].sort()
save(bosses_path, bosses, b_nl, b_tr, indent=1)

# --- skills and buffs ---------------------------------------------------------
def merge_names(kind, rows, key_of, order):
    files = {lang: load(I18N / kind / f"{lang}.json") for lang in LANGS if (I18N / kind / f"{lang}.json").is_file()}
    added = 0
    for r in rows:
        rid = str(r["ID"]["Value"])
        if rid in files["en"][0]:
            continue
        names = texts(key_of(r))
        if not names:
            continue
        for lang, (d, _, _) in files.items():
            d[rid] = names[lang]
        added += 1
    for lang, (d, nl, tr) in files.items():
        save(I18N / kind / f"{lang}.json", order(d), nl, tr)
    return added


added_skills = merge_names("skills", table("Skill"),
                           lambda r: "SkillString_" + str(r["SkillString_Key"]) + "_skill_name",
                           lambda d: dict(sorted(d.items())))
added_buffs = merge_names("abnormals", table("SkillAbnormal"),
                          lambda r: "SkillAbnormalString_" + str(r.get("SkillAbnormalString_Key") or "") + "_desc_name",
                          lambda d: dict(sorted(d.items())))

print(f"dungeons +{len(added_dungeons)} {added_dungeons}")
print(f"npcs +{added_npcs} ({linked} linked to a dungeon), finalBosses {len(bosses['finalBosses'])}")
print(f"skills +{added_skills}, buffs +{added_buffs}")


# --- Transcendence and Sanctuary tiers ----------------------------------------
# One rule each, from the dungeon the boss is in: Transcendence is "Stage" with
# the game's grade (DeusCenter_G_06 is Stage 6; the listed data had squeezed
# grades 1, 3, 6, 9 into Stages 1-4 for two dungeons and not for the others),
# Sanctuary the dungeon's difficulty. A dungeon the client no longer lists
# keeps what it had (Abyssal Forge Hard, 600083).
STAGE = {"de": "Stufe {n}", "en": "Stage {n}", "es": "Fase {n}", "fr": "Niveau {n}", "ja": "{n}段階",
         "ko": "{n}단계", "pt": "Etapa {n}", "ru": "Этап {n}", "zh-Hans": "第{n}阶段", "zh-Hant": "第{n}階段"}
LEVELS = {
    "Easy": {"de": "Leicht", "en": "Easy", "es": "Fácil", "fr": "Facile", "ja": "簡単", "ko": "쉬움", "pt": "Fácil",
             "ru": "Лёгкий", "zh-Hans": "简单", "zh-Hant": "簡單"},
    "Normal": {"de": "Mittel", "en": "Normal", "es": "Normal", "fr": "Normal", "ja": "普通", "ko": "보통", "pt": "Médio",
               "ru": "Средний", "zh-Hans": "普通", "zh-Hant": "普通"},
    "Hard": {"de": "Schwer", "en": "Hard", "es": "Difícil", "fr": "Difficile", "ja": "難しい", "ko": "어려움", "pt": "Difícil",
             "ru": "Сложный", "zh-Hans": "困难", "zh-Hant": "困難"},
}
LEVEL_OF = {"EDungeonDifficulty::Easy": "Easy", "EDungeonDifficulty::Normal": "Normal",
            "EDungeonDifficulty::Advanced": "Hard", "EDungeonDifficulty::Hard": "Hard"}
# Sanctuaries the TW client no longer lists: Abyssal Forge Hard (Ludra Hard, Global).
KNOWN_LEVELS = {600083: "Hard"}
dungeon_by_id = {r["ID"]["Value"]: r for r in dungeons}
npc_rows = {r["ID"]["Value"]: r for r in table("NpcData")}
npc_files = {lang: load(I18N / "npcs" / f"{lang}.json") for lang in LANGS}
retiered = 0
for nid, e in npc_files["en"][0].items():
    category, did = e.get("category"), e.get("dungeonId")
    if category not in ("Transcendence", "Sanctuary") or not did:
        continue
    row = dungeon_by_id.get(did)
    tier = None
    if category == "Transcendence":
        m = re.search(r"_G_(\d+)$", row["Name"]) if row else None
        m = m or re.search(r"_Grade(\d+)$", (npc_rows.get(int(nid)) or {}).get("Name", ""))
        if m:
            tier = ("stage", int(m.group(1)))
    elif row and row.get("DungeonDifficulty") in LEVEL_OF:
        tier = ("level", LEVEL_OF[row["DungeonDifficulty"]])
    elif did in KNOWN_LEVELS:
        tier = ("level", KNOWN_LEVELS[did])
    elif e.get("tier") in LEVELS:
        tier = ("level", e["tier"])
    if not tier:
        continue
    for lang in LANGS:
        entry = npc_files[lang][0].get(nid)
        if entry is None:
            continue
        word = STAGE[lang].format(n=tier[1]) if tier[0] == "stage" else LEVELS[tier[1]][lang]
        if entry.get("tier") != word:
            entry["tier"] = word
            retiered += lang == "en"
for lang, (d, nl, tr) in npc_files.items():
    save(I18N / "npcs" / f"{lang}.json", d, nl, tr)
print(f"Transcendence and Sanctuary tiers set on {retiered} bosses")
