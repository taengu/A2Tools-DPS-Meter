//! Asking the desktop to keep the meter above a fullscreen game. On KDE
//! Plasma that is a KWin window rule (see `platform::kwin_rules`), written with
//! KDE's own `kwriteconfig6`/`kwriteconfig5` and loaded with KWin's
//! `reconfigure`. Other desktops are left alone.

use std::process::Command;

use crate::platform::kwin_rules::{
    config_value, general_entries_for, icons_rule_entries, is_kde, no_focus_rule_entries, order_only_entries, parse_version,
    plugin_windows_rule_entries, reads_order_only, rule_entries, GROUP, ICONS_GROUP, NO_FOCUS_GROUP, PLUGIN_WINDOWS_GROUP,
};

fn tool(names: &[&'static str]) -> Option<&'static str> {
    names.iter().copied().find(|name| {
        Command::new(name).arg("--help").output().is_ok_and(|o| o.status.success() || !o.stdout.is_empty())
    })
}

fn read(reader: &str, group: &str, key: &str) -> String {
    Command::new(reader)
        .args(["--file", "kwinrulesrc", "--group", group, "--key", key])
        .output()
        .map(|o| config_value(&String::from_utf8_lossy(&o.stdout)).to_string())
        .unwrap_or_default()
}

fn delete(writer: &str, group: &str, key: &str) {
    let _ = Command::new(writer).args(["--file", "kwinrulesrc", "--group", group, "--key", key, "--delete"]).status();
}

/// Whether the running KWin lists rules in `Order` alone (KWin 6.8 and its
/// betas; see `kwin_rules::reads_order_only`). Asked once, of the KWin
/// binary; not known, the lists are kept both ways, which every KWin reads.
fn kwin_reads_order_only() -> bool {
    static ANSWER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ANSWER.get_or_init(|| {
        let version = ["kwin_wayland", "kwin_x11", "plasmashell"].iter().find_map(|program| {
            let out = Command::new(program).arg("--version").output().ok()?;
            parse_version(&String::from_utf8_lossy(&out.stdout))
        });
        tracing::info!("KWin {:?}: rules listed in {}", version,
            if version.is_some_and(reads_order_only) { "Order" } else { "rules and Order" });
        version.is_some_and(reads_order_only)
    })
}

fn write(writer: &str, group: &str, key: &str, value: &str) -> Result<(), String> {
    let status = Command::new(writer)
        .args(["--file", "kwinrulesrc", "--group", group, "--key", key, "--", value])
        .status()
        .map_err(|e| format!("{writer}: {e}"))?;
    status.success().then_some(()).ok_or_else(|| format!("{writer} {group}/{key} failed"))
}

/// Ask KWin to load its rules again. Several ways, as distributions ship
/// different D-Bus tools.
fn reconfigure() {
    let attempts: [(&str, &[&str]); 3] = [
        ("dbus-send", &["--session", "--type=method_call", "--dest=org.kde.KWin", "/KWin", "org.kde.KWin.reconfigure"]),
        ("qdbus6", &["org.kde.KWin", "/KWin", "reconfigure"]),
        ("qdbus", &["org.kde.KWin", "/KWin", "reconfigure"]),
    ];
    for (program, args) in attempts {
        if Command::new(program).args(args).status().is_ok_and(|s| s.success()) {
            return;
        }
    }
    tracing::info!("KWin rule written; KWin applies it after the next login");
}

/// Make sure the desktop keeps the meter above a fullscreen game. Called at
/// every start with `done` false, so a missing rule is put back (it is the only
/// thing that keeps the meter on top there); nothing is written when it is
/// there. Returns whether the job is done now (added, or already there).
pub fn keep_above_fullscreen(done: bool) -> Result<bool, String> {
    add_rule(GROUP, &rule_entries(), done, "the meter stays above fullscreen games")
}

/// Make sure a click on the meter overlay leaves the game focused (#71), as
/// `keep_above_fullscreen` does for its rule.
pub fn click_without_focus(done: bool) -> Result<bool, String> {
    add_rule(NO_FOCUS_GROUP, &no_focus_rule_entries(), done, "clicking the overlay keeps the game focused")
}

/// Make sure plugins' icons never take the focus from the game.
pub fn icons_without_focus(done: bool) -> Result<bool, String> {
    add_rule(ICONS_GROUP, &icons_rule_entries(), done, "plugin icons keep the game focused")
}

/// Make sure plugin windows stay above a fullscreen game, so an alert one
/// opens is seen.
pub fn plugin_windows_above(done: bool) -> Result<bool, String> {
    add_rule(PLUGIN_WINDOWS_GROUP, &plugin_windows_rule_entries(), done, "plugin windows stay above fullscreen games")
}

/// The meter's rules, each with its group, entries and what it does.
fn rules() -> [(&'static str, Vec<(&'static str, &'static str)>, &'static str); 4] {
    [
        (GROUP, rule_entries(), "the meter stays above fullscreen games"),
        (NO_FOCUS_GROUP, no_focus_rule_entries(), "clicking the overlay keeps the game focused"),
        (ICONS_GROUP, icons_rule_entries(), "plugin icons keep the game focused"),
        (PLUGIN_WINDOWS_GROUP, plugin_windows_rule_entries(), "plugin windows stay above fullscreen games"),
    ]
}

/// Keep the meter's rules in `kwinrulesrc` while it runs: KWin rewrites the
/// file from what it holds, and rules it did not write itself went missing
/// mid-session (KDE Plasma 6, 2026-10-19), taking the focus fixes with them.
/// When the file changes (its time, looked at every 15 s), any rule gone is
/// put back and KWin told; each time is logged, to tell what removes them.
pub fn keep_rules() {
    if !is_kde(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()) {
        return;
    }
    let file = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .map(|dir| dir.join("kwinrulesrc"));
    let Some(file) = file else { return };
    let changed = |f: &std::path::Path| std::fs::metadata(f).and_then(|m| m.modified()).ok();
    std::thread::spawn(move || {
        let mut seen = changed(&file);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(15));
            let now = changed(&file);
            if now == seen {
                continue;
            }
            // A rule gone is put back, and one dropped from the list listed
            // again; add_rule logs either.
            for (group, entries, what) in rules() {
                if let Err(e) = add_rule(group, &entries, false, what) {
                    tracing::warn!("Could not put the KWin rule back: {e}");
                }
            }
            seen = changed(&file);
        }
    });
}

fn add_rule(group: &str, entries: &[(&str, &str)], done: bool, what: &str) -> Result<bool, String> {
    if done || !is_kde(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()) {
        return Ok(done);
    }
    let (Some(writer), Some(reader)) =
        (tool(&["kwriteconfig6", "kwriteconfig5"]), tool(&["kreadconfig6", "kreadconfig5"]))
    else {
        return Err("kwriteconfig is not installed".into());
    };
    let added = read(reader, group, "wmclass").is_empty();
    // A rule an earlier version wrote, with settings that have changed since
    // (the overlay's rule once matched every window, #90): written again.
    let outdated = !added && entries.iter().any(|(key, value)| read(reader, group, key) != *value);
    if added || outdated {
        for (key, value) in entries {
            write(writer, group, key, value)?;
        }
    }
    if outdated {
        reconfigure();
        tracing::info!("KWin rule updated: {what}");
    }
    // KWin 6.8: Order alone; a rules list makes it delete the rules not in
    // it, and it drops the list when it saves (see reads_order_only).
    if kwin_reads_order_only() {
        let order = read(reader, "General", "Order");
        let want = order_only_entries(group, &order);
        let old_list = !read(reader, "General", "rules").is_empty();
        if !added && want == order && !old_list {
            return Ok(true);
        }
        if old_list {
            delete(writer, "General", "rules");
            delete(writer, "General", "count");
        }
        write(writer, "General", "Order", &want)?;
        reconfigure();
        tracing::info!("KWin rule {}: {what}", if added { "added" } else { "list mended" });
        return Ok(true);
    }
    // Earlier KWin: the rule must be named in `rules` too, or KWin neither
    // loads it nor keeps it (see general_entries); 2.0.73 took that list
    // out, so a rule already there is checked as well and its list mended.
    let now = [read(reader, "General", "Order"), read(reader, "General", "rules"), read(reader, "General", "count")];
    let general = general_entries_for(group, &now[0], &now[1]);
    if !added && general.iter().zip(&now).all(|((_, want), have)| want == have) {
        return Ok(true);
    }
    for (key, value) in general {
        write(writer, "General", key, &value)?;
    }
    if !added {
        reconfigure();
        tracing::info!("KWin rule list mended: {what}");
        return Ok(true);
    }
    reconfigure();
    tracing::info!("KWin rule added: {what}");
    Ok(true)
}
