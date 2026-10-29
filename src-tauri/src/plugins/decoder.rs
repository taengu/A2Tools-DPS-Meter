//! Plugin decoders: a plugin's manifest names a frame type and the fields it
//! wants, and this reads exactly those, as numbers, from frames of that type.
//! A plugin never sees the frame itself.
//!
//! ```json
//! { "name": "soul-gained", "opcode": "0D 90",
//!   "fields": [ { "type": "skip:1" },
//!               { "name": "pet", "type": "u32" },
//!               { "name": "amount", "type": "u32" } ] }
//! ```
//!
//! Field types: `u8`, `u16`, `u32`, `u64`, `i32` (little-endian), `varint`,
//! `skip:N`, and `list` (a `count` of `u8`, `u16`, `u32` or `varint`, then that
//! many entries of the list's own `fields`). Numeric fields are not an
//! anonymization boundary: a byte-valued list can reconstruct text. The host
//! therefore asks for approval of each plugin's complete decoder layout.
//!
//! `"self": "<field>"` (plugin API 2) names a top-level number field that holds
//! an entity id: the meter passes the event on only when that entity is the
//! player's own character, so a plugin can tell "I did this" without ever
//! seeing anyone else's id.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Most fields one level of a decoder may have.
const MAX_FIELDS: usize = 32;
/// Most entries a list may have.
const MAX_LIST: u64 = 4096;
/// How deep lists may nest.
const MAX_DEPTH: u8 = 2;
/// Total JSON values one decoded event may allocate, including list items.
/// Per-list limits alone allow nested lists to multiply into millions of
/// empty objects without consuming a corresponding amount of packet data.
const MAX_VALUES: usize = 8192;
const MAX_FIELD_NAME: usize = 64;
/// Frame types no decoder may read, for any plugin: another player coming
/// into view (`44 36`, `45 36`) and players' movement (`1e`-`20 37`,
/// `28`-`2a 37`), self/party identity and summon-owner records. This is an
/// additional restriction, not a guarantee that other numeric packet fields
/// cannot identify players: decoder access needs explicit host approval.
const REFUSED_OPCODES: [[u8; 2]; 12] = [
    [0x33, 0x36],
    [0x02, 0x97],
    [0x04, 0x8d],
    [0x40, 0x36],
    [0x44, 0x36],
    [0x45, 0x36],
    [0x1e, 0x37],
    [0x1f, 0x37],
    [0x20, 0x37],
    [0x28, 0x37],
    [0x29, 0x37],
    [0x2a, 0x37],
];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DecoderSpec {
    pub name: String,
    /// Two bytes in hex, as frames are logged: `"0D 90"`.
    pub opcode: String,
    pub fields: Vec<FieldSpec>,
    /// A field that must be the player's own entity id (see the module docs).
    #[serde(default, rename = "self")]
    pub self_field: Option<String>,
    /// Sent as soon as it is read, even to a plugin told the character,
    /// whose events otherwise wait for the login's character record (see
    /// `SETTLE` in the plugin host). For records that never come at login.
    #[serde(default)]
    pub immediate: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FieldSpec {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    /// For `list`: the count's type.
    #[serde(default)]
    pub count: Option<String>,
    /// For `list`: each entry's fields.
    #[serde(default)]
    pub fields: Option<Vec<FieldSpec>>,
}

/// A decoder checked and ready to run.
#[derive(Debug, Clone)]
pub struct Decoder {
    pub name: String,
    pub opcode: [u8; 2],
    fields: Vec<FieldSpec>,
    /// The field that must be the player's own entity id, if any.
    pub self_field: Option<String>,
    /// Never waits for the character record.
    pub immediate: bool,
}

impl Decoder {
    /// Check a manifest's decoder: a sane name, a two-byte opcode, and fields
    /// of known types within the limits.
    pub fn compile(spec: &DecoderSpec) -> Result<Self, String> {
        if spec.name.is_empty()
            || spec.name.len() > 40
            || !spec.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!("decoder name {:?}: lowercase letters, digits and '-' only", spec.name));
        }
        let bytes: Vec<u8> = spec
            .opcode
            .split_whitespace()
            .map(|b| u8::from_str_radix(b, 16))
            .collect::<Result<_, _>>()
            .map_err(|_| format!("decoder {}: opcode {:?} is not hex bytes", spec.name, spec.opcode))?;
        let opcode: [u8; 2] = bytes
            .try_into()
            .map_err(|_| format!("decoder {}: opcode must be two bytes", spec.name))?;
        if REFUSED_OPCODES.contains(&opcode) {
            return Err(format!("decoder {}: frames {} can't be read by plugins", spec.name, spec.opcode));
        }
        check_fields(&spec.fields, 0).map_err(|e| format!("decoder {}: {e}", spec.name))?;
        if let Some(field) = &spec.self_field {
            let ok = spec.fields.iter().any(|f| {
                f.name.as_deref() == Some(field.as_str()) && matches!(f.kind.as_str(), "u16" | "u32" | "u64" | "varint")
            });
            if !ok {
                return Err(format!("decoder {}: \"self\" must name one of its number fields", spec.name));
            }
        }
        Ok(Self { name: spec.name.clone(), opcode, fields: spec.fields.clone(), self_field: spec.self_field.clone(), immediate: spec.immediate })
    }

    /// Decode a frame's body (the bytes after its type) into the fields.
    pub fn decode(&self, body: &[u8]) -> Option<Value> {
        let mut pos = 0;
        let mut remaining = MAX_VALUES;
        decode_fields(&self.fields, body, &mut pos, &mut remaining).map(Value::Object)
    }
}

fn check_fields(fields: &[FieldSpec], depth: u8) -> Result<(), String> {
    if fields.len() > MAX_FIELDS {
        return Err(format!("more than {MAX_FIELDS} fields"));
    }
    let mut names = std::collections::HashSet::new();
    for field in fields {
        let kind = field.kind.as_str();
        if !kind.starts_with("skip:") {
            let name = field.name.as_deref().unwrap_or_default();
            if name.is_empty() || name.len() > MAX_FIELD_NAME || !names.insert(name) {
                return Err(format!("field names must be unique, nonempty and at most {MAX_FIELD_NAME} bytes"));
            }
        }
        match kind {
            "u8" | "u16" | "u32" | "u64" | "i32" | "varint" => {
                if field.name.as_deref().is_none_or(str::is_empty) {
                    return Err(format!("a {kind} field needs a name"));
                }
            }
            "list" => {
                if depth >= MAX_DEPTH {
                    return Err("lists nest too deep".into());
                }
                if field.name.as_deref().is_none_or(str::is_empty) {
                    return Err("a list needs a name".into());
                }
                match field.count.as_deref() {
                    Some("u8" | "u16" | "u32" | "varint") => {}
                    _ => return Err("a list's count must be u8, u16, u32 or varint".into()),
                }
                check_fields(field.fields.as_deref().unwrap_or(&[]), depth + 1)?;
            }
            skip if skip.starts_with("skip:") => {
                let n: usize = skip[5..].parse().map_err(|_| format!("bad {skip}"))?;
                if n == 0 || n > 4096 {
                    return Err(format!("bad {skip}"));
                }
            }
            other => return Err(format!("unknown field type {other:?}")),
        }
    }
    Ok(())
}

fn take<'a>(body: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(n)?;
    let bytes = body.get(*pos..end)?;
    *pos = end;
    Some(bytes)
}

fn varint(body: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *take(body, pos, 1)?.first()?;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn number(kind: &str, body: &[u8], pos: &mut usize) -> Option<u64> {
    Some(match kind {
        "u8" => u64::from(take(body, pos, 1)?[0]),
        "u16" => u64::from(u16::from_le_bytes(take(body, pos, 2)?.try_into().ok()?)),
        "u32" => u64::from(u32::from_le_bytes(take(body, pos, 4)?.try_into().ok()?)),
        "u64" => u64::from_le_bytes(take(body, pos, 8)?.try_into().ok()?),
        "varint" => varint(body, pos)?,
        _ => return None,
    })
}

fn decode_fields(fields: &[FieldSpec], body: &[u8], pos: &mut usize, remaining: &mut usize) -> Option<Map<String, Value>> {
    let mut out = Map::new();
    for field in fields {
        let kind = field.kind.as_str();
        if let Some(n) = kind.strip_prefix("skip:") {
            take(body, pos, n.parse().ok()?)?;
            continue;
        }
        *remaining = remaining.checked_sub(1)?;
        let name = field.name.clone().unwrap_or_default();
        match kind {
            "i32" => {
                let v = i32::from_le_bytes(take(body, pos, 4)?.try_into().ok()?);
                out.insert(name, Value::from(v));
            }
            "list" => {
                let count = number(field.count.as_deref()?, body, pos)?;
                if count > MAX_LIST {
                    return None;
                }
                // Charge every item before reserving its array, including
                // empty objects (whose fields consume no packet bytes).
                *remaining = remaining.checked_sub(count as usize)?;
                let inner = field.fields.as_deref().unwrap_or(&[]);
                let mut items = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    items.push(Value::Object(decode_fields(inner, body, pos, remaining)?));
                }
                out.insert(name, Value::Array(items));
            }
            _ => {
                out.insert(name, Value::from(number(kind, body, pos)?));
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> Decoder {
        Decoder::compile(&serde_json::from_str(json).unwrap()).unwrap()
    }

    /// A player with more than 127 pets: the game sends the list's count as a
    /// varint, two bytes from 128 on (`87 01` = 135, from a player's log,
    /// 2026-10-13). Read as a u8 the whole list came out one byte off.
    #[test]
    fn a_varint_count_past_127() {
        let d = spec(r#"{"name":"pet-list","opcode":"00 90","fields":[
            {"type":"skip:4"},
            {"name":"levels","type":"list","count":"varint","fields":[
                {"name":"pet","type":"u32"},{"type":"skip:4"},{"name":"level","type":"u32"}]}]}"#);
        let mut body = vec![0x13, 0x05, 0, 0, 0x87, 0x01];
        for i in 0..135u32 {
            body.extend((1001 + i).to_le_bytes());
            body.extend((1001 + i).to_le_bytes());
            body.extend((1 + i % 3).to_le_bytes());
        }
        let v = d.decode(&body).unwrap();
        let levels = v["levels"].as_array().unwrap();
        assert_eq!(levels.len(), 135);
        assert_eq!(levels[0]["pet"], 1001);
        assert_eq!(levels[134]["pet"], 1135);
        assert_eq!(levels[134]["level"], 3);
    }

    #[test]
    fn other_players_records_are_refused() {
        for opcode in ["45 36", "44 36", "29 37", "2A 37", "33 36", "02 97", "04 8D", "40 36"] {
            let json = format!(r#"{{"name":"x","opcode":"{opcode}","fields":[{{"name":"id","type":"varint"}}]}}"#);
            assert!(Decoder::compile(&serde_json::from_str(&json).unwrap()).is_err(), "{opcode}");
        }
        // Monsters' spawns are still read (Pet Soul Tracker).
        spec(r#"{"name":"x","opcode":"41 36","fields":[{"name":"id","type":"varint"}]}"#);
    }

    #[test]
    fn reads_a_soul_gain() {
        let d = spec(r#"{"name":"soul-gained","opcode":"0D 90","fields":[
            {"type":"skip:1"},{"name":"pet","type":"u32"},{"name":"amount","type":"u32"}]}"#);
        assert_eq!(d.opcode, [0x0D, 0x90]);
        let body = [0x01, 0xF8, 0x03, 0, 0, 0x01, 0, 0, 0];
        assert_eq!(d.decode(&body).unwrap(), serde_json::json!({"pet": 1016, "amount": 1}));
    }

    /// A soul pickup lists every pet it gave souls to (a capture from a
    /// player, 2026-10-19: pets 1015 and 1120 from one kill), read as a list.
    #[test]
    fn reads_a_soul_pickup_for_two_pets() {
        let d = spec(r#"{"name":"soul-gained","opcode":"0D 90","fields":[
            {"name":"souls","type":"list","count":"u8","fields":[
                {"name":"pet","type":"u32"},{"name":"amount","type":"u32"}]}]}"#);
        let body = [0x02, 0xF7, 0x03, 0, 0, 1, 0, 0, 0, 0x60, 0x04, 0, 0, 1, 0, 0, 0];
        assert_eq!(
            d.decode(&body).unwrap(),
            serde_json::json!({"souls": [{"pet": 1015, "amount": 1}, {"pet": 1120, "amount": 1}]})
        );
    }

    #[test]
    fn reads_the_pet_list_with_its_two_lists() {
        let d = spec(r#"{"name":"pet-list","opcode":"00 90","fields":[
            {"type":"skip:4"},
            {"name":"levels","type":"list","count":"u8","fields":[
                {"name":"pet","type":"u32"},{"type":"skip:4"},{"name":"level","type":"u32"}]},
            {"name":"points","type":"list","count":"u8","fields":[
                {"name":"pet","type":"u32"},{"name":"points","type":"u32"}]}]}"#);
        let mut body = vec![0xFE, 0x04, 0, 0, 1];
        for v in [1097u32, 1097, 2] {
            body.extend(v.to_le_bytes());
        }
        body.push(2);
        for v in [1034u32, 28, 1016, 1] {
            body.extend(v.to_le_bytes());
        }
        body.extend([9, 9, 9]); // trailing data the decoder does not ask for
        assert_eq!(
            d.decode(&body).unwrap(),
            serde_json::json!({
                "levels": [{"pet": 1097, "level": 2}],
                "points": [{"pet": 1034, "points": 28}, {"pet": 1016, "points": 1}]
            })
        );
    }

    #[test]
    fn a_short_frame_decodes_to_nothing() {
        let d = spec(r#"{"name":"x","opcode":"0D 90","fields":[{"name":"a","type":"u32"}]}"#);
        assert!(d.decode(&[1, 2]).is_none());
    }

    #[test]
    fn bad_decoders_are_refused() {
        let bad = [
            r#"{"name":"x","opcode":"0D","fields":[]}"#,
            r#"{"name":"X!","opcode":"0D 90","fields":[]}"#,
            r#"{"name":"x","opcode":"0D 90","fields":[{"name":"s","type":"string"}]}"#,
            r#"{"name":"x","opcode":"0D 90","fields":[{"type":"u32"}]}"#,
            r#"{"name":"x","opcode":"0D 90","fields":[{"name":"l","type":"list","count":"u8","fields":[
                {"name":"m","type":"list","count":"u8","fields":[
                    {"name":"n","type":"list","count":"u8","fields":[]}]}]}]}"#,
        ];
        for json in bad {
            assert!(Decoder::compile(&serde_json::from_str(json).unwrap()).is_err(), "{json}");
        }
    }

    /// `immediate` is read when set and off by default; a meter without it
    /// ignores the key.
    #[test]
    fn immediate_is_off_unless_set() {
        assert!(!spec(r#"{"name":"a","opcode":"0D 90","fields":[]}"#).immediate);
        assert!(spec(r#"{"name":"a","opcode":"0D 90","immediate":true,"fields":[]}"#).immediate);
    }

    #[test]
    fn nested_empty_lists_cannot_amplify_a_small_packet() {
        let d = spec(r#"{"name":"x","opcode":"0D 90","fields":[
            {"name":"outer","type":"list","count":"u16","fields":[
                {"name":"inner","type":"list","count":"u16","fields":[]}]}]}"#);
        let mut body = 4096u16.to_le_bytes().to_vec();
        for _ in 0..4096 {
            body.extend(4096u16.to_le_bytes());
        }
        assert!(d.decode(&body).is_none(), "16 million empty objects must be refused");
        let small = [2, 0, 3, 0, 4, 0];
        let value = d.decode(&small).expect("ordinary nested lists still work");
        assert_eq!(value["outer"][0]["inner"].as_array().unwrap().len(), 3);
        assert_eq!(value["outer"][1]["inner"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn value_budget_is_shared_by_sibling_fields() {
        let d = spec(r#"{"name":"x","opcode":"0D 90","fields":[
            {"name":"a","type":"list","count":"u16","fields":[]},
            {"name":"b","type":"list","count":"u16","fields":[]}]}"#);
        assert!(d.decode(&[0, 16, 0, 16]).is_none());
        assert!(d.decode(&[0, 16, 0, 0]).is_some());
    }

    #[test]
    fn overflowing_varints_are_refused() {
        let d = spec(r#"{"name":"x","opcode":"0D 90","fields":[{"name":"n","type":"varint"}]}"#);
        let mut max = vec![0xff; 9];
        max.push(1);
        assert_eq!(d.decode(&max).unwrap()["n"].as_u64(), Some(u64::MAX));
        max[9] = 2;
        assert!(d.decode(&max).is_none());
        max[9] = 0x81;
        assert!(d.decode(&max).is_none());
    }

    #[test]
    fn field_names_cannot_overwrite_a_self_filter_or_amplify_keys() {
        let duplicate = r#"{"name":"x","opcode":"0D 90","self":"actor","fields":[
            {"name":"actor","type":"varint"},{"name":"actor","type":"u32"}]}"#;
        assert!(Decoder::compile(&serde_json::from_str(duplicate).unwrap()).is_err());
        let mut oversized: DecoderSpec = serde_json::from_str(r#"{"name":"x","opcode":"0D 90","fields":[{"name":"n","type":"u8"}]}"#).unwrap();
        oversized.fields[0].name = Some("n".repeat(MAX_FIELD_NAME + 1));
        assert!(Decoder::compile(&oversized).is_err());
    }
}
