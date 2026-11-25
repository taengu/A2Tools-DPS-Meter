//! The KWin window rule that keeps the meter above a fullscreen game on KDE
//! Plasma, as data: what the rule says and how it joins the rules already in
//! `kwinrulesrc`. OS-neutral so it can be tested anywhere; `linux/window_rules`
//! writes it.
//!
//! KWin treats a borderless game that covers the screen as fullscreen, and a
//! fullscreen window sits above "keep above" windows, so the meter went behind
//! the game (#33, and a CachyOS player on Discord, 2026-10-08). Forcing the
//! meter's layer to Overlay puts it above. Keys and values are KWin's own
//! (src/rulesettings.kcfg): `layerrule`/`aboverule` 2 is Force, `wmclassmatch`
//! 1 is an exact match and 3 a regular expression, `titlematch` the same.

/// The rule's group in `kwinrulesrc`. Fixed, so the meter can tell its own
/// rule from the player's and never adds it twice.
pub const GROUP: &str = "a2tools-dps-meter-overlay";

/// The window class the meter's windows carry.
pub const WINDOW_CLASS: &str = "a2tools-dps-meter";

/// The rules' class, as a regular expression (`wmclassmatch` 3). KWin
/// compares the class as written: a native Wayland window's is
/// `a2tools-dps-meter`, but under XWayland (`GDK_BACKEND=x11`) GTK capitalises
/// it, `A2tools-dps-meter`, and an exact `a2tools-dps-meter` missed it. Since
/// the rule also needs the title (#90) nothing else put the overlay above, so
/// it went behind a fullscreen game.
pub const WINDOW_CLASS_MATCH: &str = "^[aA]2tools-dps-meter$";

/// The rule, as (key, value) pairs in its group: the overlay only (class and
/// exact title). It matched every window of the meter's by class, and a
/// forced Keep Above can't be unticked, so Settings, Battle History and
/// plugin windows were stuck above everything (#90). Plugins' icons and
/// windows get the same through their own rules (`icons_rule_entries`,
/// `plugin_windows_rule_entries`).
pub fn rule_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Description", "A2Tools DPS Meter above fullscreen games (added by the meter)"),
        ("wmclass", WINDOW_CLASS_MATCH),
        ("wmclassmatch", "3"),
        ("wmclasscomplete", "false"),
        ("title", OVERLAY_TITLE),
        ("titlematch", "1"),
        ("layer", "overlay"),
        ("layerrule", "2"),
        // Plasma 5 has no layer rule; keep-above is what it can do.
        ("above", "true"),
        ("aboverule", "2"),
    ]
}

/// The second rule's group: clicking the meter overlay does not take focus
/// from the game (#71). Its own group, so installs that already have the
/// first rule get it too.
pub const NO_FOCUS_GROUP: &str = "a2tools-dps-meter-no-focus";

/// The overlay's title. The meter's other windows (Settings, Details,
/// Battle History, plugins) have their own titles and keep focus as usual:
/// a window that takes no focus takes no typing either.
pub const OVERLAY_TITLE: &str = "A2Tools DPS Meter";

/// The no-focus rule: the overlay window only (class and exact title), and
/// `acceptfocus` false, forced (`acceptfocusrule` 2). Clicks still reach it.
pub fn no_focus_rule_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Description", "A2Tools DPS Meter overlay keeps focus on the game (added by the meter)"),
        ("wmclass", WINDOW_CLASS_MATCH),
        ("wmclassmatch", "3"),
        ("wmclasscomplete", "false"),
        ("title", OVERLAY_TITLE),
        ("titlematch", "1"),
        ("acceptfocus", "false"),
        ("acceptfocusrule", "2"),
        // Out of Alt+Tab: it takes no focus, so switching to it did nothing.
        ("skipswitcher", "true"),
        ("skipswitcherrule", "2"),
    ]
}

/// The third rule's group: plugins' icons (widgets) never take focus. Each
/// one an alert opened took it from the game until it closed, and handing it
/// back after still left the game a moment without it (KDE Plasma 6 on
/// Wayland, 2026-10-19). Clicks still reach them.
pub const ICONS_GROUP: &str = "a2tools-dps-meter-icons";

/// What plugins' icons are titled with: `"<plugin> · <name>"`
/// (plugins::widgets). No other window of the meter's has it.
pub const ICON_TITLE_MARK: &str = " · ";

/// The icons' rule: the meter's windows whose title contains
/// `ICON_TITLE_MARK` (`titlematch` 2), `acceptfocus` false, forced, and
/// above a fullscreen game like the overlay (they are overlays too).
pub fn icons_rule_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Description", "A2Tools DPS Meter plugin icons keep focus on the game (added by the meter)"),
        ("wmclass", WINDOW_CLASS_MATCH),
        ("wmclassmatch", "3"),
        ("wmclasscomplete", "false"),
        ("title", ICON_TITLE_MARK),
        ("titlematch", "2"),
        ("acceptfocus", "false"),
        ("acceptfocusrule", "2"),
        ("layer", "overlay"),
        ("layerrule", "2"),
        ("above", "true"),
        ("aboverule", "2"),
        // Out of Alt+Tab and the taskbar: one entry per icon cluttered both
        // (Hirai, 2026-10-22), and GTK's skip-taskbar does nothing on
        // native Wayland.
        ("skipswitcher", "true"),
        ("skipswitcherrule", "2"),
        ("skiptaskbar", "true"),
        ("skiptaskbarrule", "2"),
    ]
}

/// The fourth rule's group: plugin windows above a fullscreen game. Since
/// #90 they were Keep Above only, which a focused fullscreen game covers, so
/// a plugin's alert window (a party member's low HP, a mantra off) opened
/// behind the game on its monitor. Each plugin window has its own hide or
/// close button; Settings, Battle History and Details stay normal windows.
pub const PLUGIN_WINDOWS_GROUP: &str = "a2tools-dps-meter-plugins";

/// The plugin windows' rule: the meter's windows whose title ends with
/// " (plugin)" (`titlematch` 3, a regular expression), as plugin windows are
/// titled `"<plugin> (plugin)"` (plugins::open_window) and no other window of
/// the meter's is. Forced to the overlay layer and Keep Above like the
/// overlay. They keep taking focus, so a plugin's search box can be typed in.
pub fn plugin_windows_rule_entries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Description", "A2Tools DPS Meter plugin windows above fullscreen games (added by the meter)"),
        ("wmclass", WINDOW_CLASS_MATCH),
        ("wmclassmatch", "3"),
        ("wmclasscomplete", "false"),
        ("title", " \\(plugin\\)$"),
        ("titlematch", "3"),
        ("layer", "overlay"),
        ("layerrule", "2"),
        ("above", "true"),
        ("aboverule", "2"),
        // Out of Alt+Tab, which they cluttered (Hirai, 2026-10-22); still
        // in the taskbar, the way back to one hidden off screen.
        ("skipswitcher", "true"),
        ("skipswitcherrule", "2"),
    ]
}

/// A value as kreadconfig prints it, without the line end it adds. Only
/// that: the icons' and plugin windows' titles start with a space (" · ",
/// " \(plugin\)$"), and trimming it made both rules look changed at every
/// start, so the meter wrote them again and had KWin reload every rule, twice,
/// while the game ran fullscreen (Hirai, 2026-11-25).
pub fn config_value(printed: &str) -> &str {
    let value = printed.strip_suffix('\n').unwrap_or(printed);
    value.strip_suffix('\r').unwrap_or(value)
}

/// The `[General]` entries that list the rules, with the meter's added.
///
/// KWin loads the rules named in `rules` (with `count`), and deletes every
/// rule group not named there as a stray (rulebooksettings.cpp, KDE Plasma
/// 6.7.5 included). `Order` is not read: with `rules` gone, KWin loaded no
/// rule at all, the player's own included, and the meter went behind the
/// game (2026-10-18). So both lists are written as one: every rule either
/// names, in their order, and the meter's on the end. Nothing is ever taken
/// out of them.
pub fn general_entries(order: &str, legacy_rules: &str) -> Vec<(&'static str, String)> {
    general_entries_for(GROUP, order, legacy_rules)
}

/// `general_entries` for any of the meter's rules.
pub fn general_entries_for(group: &str, order: &str, legacy_rules: &str) -> Vec<(&'static str, String)> {
    let mut list: Vec<&str> = Vec::new();
    for name in order.split(',').chain(legacy_rules.split(',')).chain([group]).map(str::trim) {
        if !name.is_empty() && !list.contains(&name) {
            list.push(name);
        }
    }
    let list = list.join(",");
    let count = list.split(',').count().to_string();
    vec![("Order", list.clone()), ("rules", list), ("count", count)]
}

/// KWin 6.8 (from its 6.7.90 betas) lists rules in `Order` alone. A
/// `rules` list is an old file to it: it deletes every rule group not named
/// there, takes `rules` as the order, and drops the key when it saves
/// (rulebooksettings.cpp, `usrRead`). Earlier KWin reads `rules`/`count`
/// only (KDE Plasma 6.7.5, PR #88). So which list the meter keeps depends on
/// the KWin that reads it: writing `rules` for 6.8 had KWin drop it and the
/// meter write it back, over and over.
pub fn reads_order_only(version: (u32, u32, u32)) -> bool {
    version >= (6, 7, 90)
}

/// "kwin_wayland 6.7.5" (any word before it) -> (6, 7, 5).
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let word = text.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut parts = word.split('.').map(|p| p.trim_matches(|c: char| !c.is_ascii_digit()).parse::<u32>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

/// For KWin 6.8: `Order` with every rule it names and the meter's on the
/// end, and no `rules`/`count` (see `reads_order_only`).
pub fn order_only_entries(group: &str, order: &str) -> String {
    let mut list: Vec<&str> = order.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if !list.contains(&group) {
        list.push(group);
    }
    list.join(",")
}

/// Whether the session is KDE Plasma (`XDG_CURRENT_DESKTOP` is `KDE`).
pub fn is_kde(current_desktop: &str) -> bool {
    current_desktop.split(':').any(|name| name.eq_ignore_ascii_case("kde"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(entries: &'a [(&str, String)], key: &str) -> &'a str {
        entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str()).unwrap()
    }

    #[test]
    fn a_first_rule_starts_both_lists() {
        let e = general_entries("", "");
        assert_eq!(get(&e, "Order"), GROUP);
        assert_eq!(get(&e, "rules"), GROUP);
        assert_eq!(get(&e, "count"), "1");
    }

    #[test]
    fn the_players_own_rules_stay_and_come_first() {
        // Legacy list only: Order must carry the player's rules too, or KWin
        // 6 would read Order and lose them.
        let e = general_entries("", "abc-1,def-2");
        assert_eq!(get(&e, "Order"), format!("abc-1,def-2,{GROUP}"));
        assert_eq!(get(&e, "count"), "3");
        // Both kept: every rule either names, ours on the end.
        let e = general_entries("x,y", "abc-1");
        assert_eq!(get(&e, "Order"), format!("x,y,abc-1,{GROUP}"));
        assert_eq!(get(&e, "rules"), format!("x,y,abc-1,{GROUP}"));
    }

    #[test]
    fn the_rule_is_never_listed_twice() {
        let listed = format!("abc-1,{GROUP}");
        let e = general_entries(&listed, &listed);
        assert_eq!(get(&e, "Order"), listed);
        assert_eq!(get(&e, "count"), "2");
    }

    #[test]
    fn the_rule_forces_the_overlay_layer_for_the_meter_only() {
        let rule = rule_entries();
        let value = |k: &str| rule.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        assert_eq!(value("layer"), Some("overlay"));
        assert_eq!(value("layerrule"), Some("2"));
        assert_eq!(value("wmclass"), Some(WINDOW_CLASS_MATCH));
        assert_eq!(value("wmclassmatch"), Some("3"));
    }

    /// #71: the overlay takes no focus, so a click on it leaves the game
    /// focused; only the overlay, by its exact title, as the other windows
    /// need focus to be typed in.
    #[test]
    fn the_no_focus_rule_is_the_overlays_alone_and_joins_the_first() {
        let rule = no_focus_rule_entries();
        let value = |k: &str| rule.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        assert_eq!(value("title"), Some("A2Tools DPS Meter"));
        assert_eq!(value("titlematch"), Some("1"));
        assert_eq!((value("acceptfocus"), value("acceptfocusrule")), (Some("false"), Some("2")));
        let listed = format!("abc-1,{GROUP}");
        let e = general_entries_for(NO_FOCUS_GROUP, &listed, &listed);
        assert_eq!(get(&e, "Order"), format!("{listed},{NO_FOCUS_GROUP}"));
        assert_eq!(get(&e, "count"), "3");
    }

    /// Icons only, by the mark in their title, and never focused.
    #[test]
    fn the_icons_rule_matches_titles_with_the_mark() {
        let rule = icons_rule_entries();
        let value = |k: &str| rule.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        assert_eq!(value("title"), Some(" · "));
        assert_eq!(value("titlematch"), Some("2"));
        assert_eq!((value("acceptfocus"), value("acceptfocusrule")), (Some("false"), Some("2")));
        assert_eq!(value("wmclass"), Some(WINDOW_CLASS_MATCH));
    }

    #[test]
    fn the_plugin_windows_rule_matches_titles_ending_in_plugin() {
        let rule = plugin_windows_rule_entries();
        let value = |k: &str| rule.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        assert_eq!(value("title"), Some(" \\(plugin\\)$"));
        assert_eq!(value("titlematch"), Some("3"));
        assert_eq!((value("layer"), value("layerrule")), (Some("overlay"), Some("2")));
        assert_eq!(value("acceptfocus"), None);
        assert_eq!(value("wmclass"), Some(WINDOW_CLASS_MATCH));
        assert_eq!((value("skipswitcher"), value("skipswitcherrule")), (Some("true"), Some("2")));
        assert_eq!(value("skiptaskbar"), None, "the taskbar still finds a plugin window");
    }

    /// The overlay and plugins' icons stay out of Alt+Tab; icons out of the
    /// taskbar too.
    #[test]
    fn the_overlay_and_icons_stay_out_of_alt_tab() {
        let value = |rule: &[(&str, &'static str)], k: &str| rule.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        let (overlay, icons) = (no_focus_rule_entries(), icons_rule_entries());
        assert_eq!((value(&overlay, "skipswitcher"), value(&overlay, "skipswitcherrule")), (Some("true"), Some("2")));
        assert_eq!((value(&icons, "skipswitcher"), value(&icons, "skiptaskbar")), (Some("true"), Some("true")));
        assert_eq!(value(&icons, "skiptaskbarrule"), Some("2"));
    }

    /// KWin reads `rules`: a list left only in `Order` (as 2.0.73 left it)
    /// is put back there, and no rule either list names is lost.
    #[test]
    fn a_rule_named_in_either_list_stays_in_both() {
        let e = general_entries_for(ICONS_GROUP, &format!("steam,{GROUP}"), "");
        let both = format!("steam,{GROUP},{ICONS_GROUP}");
        assert_eq!(get(&e, "rules"), both);
        assert_eq!(get(&e, "Order"), both);
        assert_eq!(get(&e, "count"), "3");
        let e = general_entries_for(GROUP, "steam", "mine,steam");
        assert_eq!(get(&e, "rules"), format!("steam,mine,{GROUP}"));
    }

    /// 6.8 and its betas list rules in Order alone; 6.7 and before read rules.
    #[test]
    fn the_kwin_version_picks_the_list() {
        assert_eq!(parse_version("kwin_wayland 6.7.5"), Some((6, 7, 5)));
        assert_eq!(parse_version("kwin_wayland 6.7.91\n"), Some((6, 7, 91)));
        assert_eq!(parse_version("kwin 6.8"), Some((6, 8, 0)));
        assert_eq!(parse_version("no version here"), None);
        assert!(!reads_order_only((6, 7, 5)));
        assert!(reads_order_only((6, 7, 91)));
        assert!(reads_order_only((6, 8, 0)));
        assert!(!reads_order_only((5, 27, 11)));
        assert_eq!(order_only_entries(ICONS_GROUP, "steam, mine"), format!("steam,mine,{ICONS_GROUP}"));
        let listed = format!("steam,{ICONS_GROUP}");
        assert_eq!(order_only_entries(ICONS_GROUP, &listed), listed);
    }

    #[test]
    fn a_value_read_back_keeps_its_spaces() {
        // As kreadconfig6 prints the two titles with spaces at their ends:
        // only the line end goes, so the rules match and aren't rewritten.
        assert_eq!(config_value(" · \n"), " · ");
        let plugin = r" \(plugin\)$";
        assert_eq!(config_value(&format!("{plugin}\n")), plugin);
        assert_eq!(config_value("overlay\r\n"), "overlay");
        assert_eq!(config_value(""), "");
        for (_, value) in icons_rule_entries().into_iter().chain(plugin_windows_rule_entries()) {
            assert_eq!(config_value(&format!("{value}\n")), value);
        }
    }

    #[test]
    fn kde_is_told_from_its_desktop_name() {
        assert!(is_kde("KDE"));
        assert!(is_kde("kde"));
        assert!(!is_kde("GNOME"));
        assert!(!is_kde("ubuntu:GNOME"));
        assert!(!is_kde(""));
    }
}
