//! Asking the compositor for a key combination.
//!
//! Wayland gives an ordinary client no way to claim one: the compositor owns
//! every key, and a program that wants one has to ask. Hyprland and Sway both
//! take the request over their own control socket.
//!
//! Nothing is written to the user's configuration. The compositor forgets the
//! binding when it restarts, and this asks again every time the program
//! starts — so there is nothing to clean up, and nothing left behind pointing
//! at a program that has been removed.
//!
//! Hyprland stopped taking `hyprctl keyword` in 0.56, where the configuration
//! moved to Lua. It answers `keyword can't work with non-legacy parsers. Use
//! eval.` — and it means it: the same request through `hyprctl eval` is taken.
//! Both are tried, newest first, so one build covers either version.

use std::process::Command;

/// The id the program runs under, on the bus and in the Flatpak.
pub const APP_ID: &str = "io.github.julio0cesar.lyricslens";

/// True inside the Flatpak sandbox.
///
/// Three things change there: `hyprctl` and `swaymsg` live on the host and
/// cannot be run, Flatpak does the updating, and a copy started detached would
/// die with the sandbox the moment the first one exits.
pub fn sandboxed() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// The compositors that can be asked, and everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compositor {
    Hyprland,
    Sway,
    /// Somewhere else. The key stays the user's to configure.
    Unknown,
}

pub fn compositor() -> Compositor {
    if sandboxed() {
        return Compositor::Unknown;
    }
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        return Compositor::Hyprland;
    }
    if std::env::var_os("SWAYSOCK").is_some() {
        return Compositor::Sway;
    }
    Compositor::Unknown
}

/// Asks for `combination` to run this program with `flag`.
///
/// The combination is written the way Hyprland writes it — `SUPER SHIFT, L` —
/// and translated for Sway. An empty one asks for nothing.
pub fn bind(combination: &str, flag: &str) -> Result<(), String> {
    let combination = combination.trim();
    if combination.is_empty() {
        return Ok(());
    }

    let program = std::env::current_exe()
        .map_err(|error| format!("could not find my own binary: {error}"))?
        .display()
        .to_string();
    let (modifiers, key) = split(combination)?;
    let run = format!("{} {flag}", shell_quote(&program));

    match compositor() {
        Compositor::Hyprland => hyprland(&modifiers, &key, &run),
        Compositor::Sway => {
            let said = talk(
                "swaymsg",
                &[
                    "bindsym".to_owned(),
                    sway_keys(&modifiers, &key),
                    "exec".to_owned(),
                    run,
                ],
            )?;
            // swaymsg answers with JSON and a zero exit code either way.
            if said.replace(' ', "").contains("\"success\":false") {
                return Err(format!("swaymsg refused: {said}"));
            }
            Ok(())
        }
        Compositor::Unknown => {
            Err("this desktop has no way to be asked; bind the key yourself".to_owned())
        }
    }
}

/// Asks Hyprland, through whichever of its two doors is open.
///
/// The binding is dropped before it is made: nothing is written down, so a
/// restart of this program would otherwise leave two bindings on one key and
/// the overlay would be shown and hidden again by a single press.
fn hyprland(modifiers: &str, key: &str, run: &str) -> Result<(), String> {
    let combination = lua_keys(modifiers, key);
    let dropped = format!(r#"hl.unbind("{}")"#, lua_quote(&combination));
    // Nothing was bound yet on the first run, and that is not a failure.
    let _ = hyprctl(&["eval".to_owned(), dropped]);

    let asked = format!(
        r#"hl.bind("{}", hl.dsp.exec_cmd("{}"))"#,
        lua_quote(&combination),
        lua_quote(run)
    );
    match hyprctl(&["eval".to_owned(), asked]) {
        Ok(()) => Ok(()),
        // Before 0.56 there was no evaluator, and `keyword` was the way.
        Err(error) => {
            tracing::debug!(%error, "the evaluator would not take it; trying the older way");
            hyprctl(&[
                "keyword".to_owned(),
                "bind".to_owned(),
                format!("{modifiers},{key},exec,{run}"),
            ])
        }
    }
}

/// `hyprctl` says `ok` when it did what was asked, and anything else when it
/// did not — including with a zero exit code, which is how a refused binding
/// used to pass for a working one.
fn hyprctl(arguments: &[String]) -> Result<(), String> {
    let said = talk("hyprctl", arguments)?;
    if said.trim().eq_ignore_ascii_case("ok") {
        return Ok(());
    }
    Err(format!("hyprctl refused: {}", said.trim()))
}

/// Runs the control program and hands back everything it said.
fn talk(program: &str, arguments: &[String]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("{program} could not be run: {error}"))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!("{program} refused: {}", said.trim()));
    }
    Ok(said)
}

/// The line to put in a configuration file, for a desktop that will not be
/// asked.
pub fn snippet(combination: &str, flag: &str) -> String {
    // Inside the sandbox the binary's path means nothing to the compositor,
    // which runs the line on the host.
    let program = if sandboxed() {
        format!("flatpak run {APP_ID}")
    } else {
        std::env::current_exe()
            .map(|path| shell_quote(&path.display().to_string()))
            .unwrap_or_else(|_| "lyricslens".to_owned())
    };
    let combination = if combination.trim().is_empty() {
        "SUPER, L"
    } else {
        combination.trim()
    };

    let (modifiers, key) = split(combination).unwrap_or_default();
    let run = format!("{program} {flag}");

    match written_for() {
        Compositor::Sway => format!("bindsym {} exec {run}", sway_keys(&modifiers, &key)),
        // Two configuration languages, and a line in the wrong one does
        // nothing at all. Which file is there says which one this is.
        Compositor::Hyprland if lua_config() => format!(
            r#"hl.bind("{}", hl.dsp.exec_cmd("{}"))"#,
            lua_quote(&lua_keys(&modifiers, &key)),
            lua_quote(&run)
        ),
        _ => format!("bind = {modifiers}, {key}, exec, {run}"),
    }
}

/// The compositor the line is written for: the one running, even where it
/// cannot be asked directly.
fn written_for() -> Compositor {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        return Compositor::Hyprland;
    }
    if std::env::var_os("SWAYSOCK").is_some() {
        return Compositor::Sway;
    }
    Compositor::Unknown
}

/// True where Hyprland is configured in Lua, which it has been since 0.56.
fn lua_config() -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    std::path::Path::new(&home)
        .join(".config/hypr/hyprland.lua")
        .is_file()
}

/// `SUPER SHIFT, L` into its two halves.
fn split(combination: &str) -> Result<(String, String), String> {
    match combination.rsplit_once(',') {
        Some((modifiers, key)) => Ok((
            modifiers.split_whitespace().collect::<Vec<_>>().join(" "),
            key.trim().to_owned(),
        )),
        // A key on its own is allowed, with no modifier.
        None => Ok((String::new(), combination.to_owned())),
    }
}

/// Hyprland's Lua spells a combination out with spaces around the plus.
fn lua_keys(modifiers: &str, key: &str) -> String {
    let mut parts: Vec<&str> = modifiers.split_whitespace().collect();
    if !key.is_empty() {
        parts.push(key);
    }
    parts.join(" + ")
}

/// A string safe to drop inside Lua double quotes.
fn lua_quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A path safe to hand to the shell the compositor runs the command with.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// Sway spells the modifiers differently and joins them with a plus.
fn sway_keys(modifiers: &str, key: &str) -> String {
    let mut parts: Vec<String> = modifiers
        .split_whitespace()
        .map(|modifier| {
            match modifier.to_ascii_uppercase().as_str() {
                "SUPER" | "MOD" | "WIN" => "Mod4",
                "ALT" => "Mod1",
                "CTRL" | "CONTROL" => "Control",
                "SHIFT" => "Shift",
                other => other,
            }
            .to_owned()
        })
        .collect();
    parts.push(key.to_owned());
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_combination_splits_into_modifiers_and_key() {
        let (modifiers, key) = split("SUPER SHIFT, L").expect("a valid combination");
        assert_eq!(modifiers, "SUPER SHIFT");
        assert_eq!(key, "L");
    }

    #[test]
    fn a_key_on_its_own_has_no_modifiers() {
        let (modifiers, key) = split("F9").expect("a valid combination");
        assert!(modifiers.is_empty());
        assert_eq!(key, "F9");
    }

    #[test]
    fn hyprlands_lua_spells_a_combination_with_spaces() {
        assert_eq!(lua_keys("SUPER SHIFT", "L"), "SUPER + SHIFT + L");
        assert_eq!(lua_keys("", "F9"), "F9");
    }

    #[test]
    fn quotes_and_backslashes_survive_the_trip_into_lua() {
        assert_eq!(lua_quote(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        assert_eq!(shell_quote("/my apps/lyricslens"), "'/my apps/lyricslens'");
    }

    #[test]
    fn sway_spells_the_modifiers_its_own_way() {
        assert_eq!(sway_keys("SUPER SHIFT", "L"), "Mod4+Shift+L");
        assert_eq!(sway_keys("CTRL ALT", "F9"), "Control+Mod1+F9");
        assert_eq!(sway_keys("", "F9"), "F9");
    }
}
