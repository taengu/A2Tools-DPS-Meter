"""Buffs the game hides, and names for buffs the game leaves unnamed.

    python tools/abnormal-extras.py <game Data/Table folder>

From the client's tables (SkillAbnormal, SkillAbnormalEffect, SkillEffect,
Skill) and the meter's own skill names (src/data/i18n/skills):

- `hidden` in src/data/abnormals.json: the abnormals the game never shows.
  No name string, no buff icon (self, party or detail view) and no effect
  but NoEffect. Each class's Dodge leaves one (3015, 3 s, with the MP-drop
  icon) that drew as an unnamed bar in the buff timeline.
- src/data/i18n/abnormals/<lang>.json: an abnormal with no name of its own
  that one named skill applies gets that skill's name, in each language.
  Names already there are kept.

Run again after a game update; it only adds.
"""

import collections
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
DATA = ROOT / "src" / "data"
LANGS = ("de", "en", "es", "fr", "ja", "ko", "pt", "ru")
# Skill names that are the developers' placeholders, not a player's word.
PLACEHOLDER = re.compile(r"^(test|for testing|softmove|monster skill)", re.I)


def rows(table, name):
    return json.loads((table / name).read_text(encoding="utf-8"))["Properties"]["Data"]


def value(x):
    return x.get("Value") if isinstance(x, dict) else x


def write(path, data, **dump):
    text = json.dumps(data, ensure_ascii=False, **dump) + "\n"
    path.write_bytes(text.replace("\n", "\r\n").encode("utf-8"))


def main(table):
    names = {lang: json.loads((DATA / "i18n" / "abnormals" / (lang + ".json")).read_text(encoding="utf-8"))
             for lang in LANGS}
    skill_names = {lang: json.loads((DATA / "i18n" / "skills" / (lang + ".json")).read_text(encoding="utf-8"))
                   for lang in LANGS}
    abnormals = {value(r["ID"]): r for r in rows(table, "SkillAbnormal.json")}
    effects = collections.defaultdict(set)
    for r in rows(table, "SkillAbnormalEffect.json"):
        effects[value(r["AbnormalId"])].add(r["AbnormalEffectType"])

    def unnamed(a):
        return (not (names["en"].get(str(a)) or "").strip()
                and abnormals[a].get("SkillAbnormalString_Key") in (None, "None"))

    hidden = sorted(a for a, r in abnormals.items() if unnamed(a)
                    and not (r.get("bAbnormalIconView") or r.get("bAbnormalPartyView")
                             or r.get("bAbnormalDetailIconView"))
                    and not effects[a] - {"EAbnormalEffectType::NoEffect"})

    # Which skills apply each abnormal: Skill -> its effect groups ->
    # SkillEffect rows of an Abnormal type, whose third value is the abnormal.
    by_group = collections.defaultdict(set)
    for r in rows(table, "SkillEffect.json"):
        vals = r.get("EffectValueList") or []
        if str(r.get("EffectType", "")).startswith("ESkillEffectType::Abnormal") and len(vals) > 2 and vals[2].isdigit():
            by_group[value(r["SkillEffectGroupId"])].add(int(vals[2]))
    appliers = collections.defaultdict(set)
    for r in rows(table, "Skill.json"):
        for timed in r.get("SkillEffectTimeDataList") or []:
            for a in by_group.get(value(timed.get("SkillEffectGroupId")), ()):
                appliers[a].add(str(value(r["ID"])))

    hide = set(hidden)
    added = collections.Counter()
    for a in sorted(appliers):
        if a in hide or a not in abnormals or not unnamed(a):
            continue
        english = {skill_names["en"].get(s) for s in appliers[a]} - {None, ""}
        if len(english) != 1 or PLACEHOLDER.match(next(iter(english))):
            continue
        for lang in LANGS:
            if (names[lang].get(str(a)) or "").strip():
                continue
            found = {skill_names[lang].get(s) for s in appliers[a]} - {None, ""}
            if len(found) == 1:
                names[lang][str(a)] = found.pop()
                added[lang] += 1

    table_path = DATA / "abnormals.json"
    shipped = json.loads(table_path.read_text(encoding="utf-8"))
    shipped["hidden"] = hidden
    write(table_path, shipped, separators=(",", ":"))
    for lang in LANGS:
        write(DATA / "i18n" / "abnormals" / (lang + ".json"), names[lang], indent=2, sort_keys=True)
    print("hidden: %d; named from their skill: %s" % (len(hidden), dict(added)))


if __name__ == "__main__":
    main(pathlib.Path(sys.argv[1]))
