//! What the program does before it ever opens a window.
//!
//! No argument parser: there are a handful of flags, none of them take a
//! value, and a dependency to match a string against a list would be a
//! dependency for nothing.

use std::process::Command;

const INSTALLER: &str = "https://raw.githubusercontent.com/byjuliocesa/lyricslens/main/install.sh";

const HELP: &str = "\
LyricsLens — synced lyrics over any window

Usage:
  lyricslens [option]

Options:
  -h, --help        print this and exit
  -V, --version     print the version and exit
      --foreground  keep the terminal, instead of letting go of it
      --settings    open the preferences window
      --toggle      hide the overlay, or bring it back
      --song        open the whole song in a window
      --position    drag the overlay somewhere else, then press again
      --sync        the line being sung starts now: fixes this song's timing
      --quit        close the overlay that is running
      --paths       print where the settings, the cache and the log live
      --upgrade     install the newest release over this one
      --uninstall   remove the program from ~/.local

With no option, the overlay runs in the background and gives the terminal back.
`lls` is the same program under a shorter name.

Hiding and moving belong to a key of your own: Wayland gives a program no way
to claim one, so bind your compositor to `lyricslens --toggle`.

Environment:
  LYRICSLENS_PLAYER  part of a player's bus name, when more than one is open
  RUST_LOG           how much to log, for example lyricslens=debug

Home: https://github.com/byjuliocesa/lyricslens
";

/// Handles the arguments that never reach the interface.
///
/// Returns the exit code when the program is done, and `None` when it should
/// carry on and open a window.
pub fn handle() -> Option<u8> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let has = |flags: &[&str]| arguments.iter().any(|a| flags.contains(&a.as_str()));

    if has(&["-h", "--help"]) {
        print!("{HELP}");
        return Some(0);
    }
    if has(&["-V", "--version"]) {
        println!("lyricslens {}", env!("CARGO_PKG_VERSION"));
        return Some(0);
    }
    if has(&["--paths"]) {
        paths();
        return Some(0);
    }
    if has(&["--upgrade"]) {
        return Some(installer(&[]));
    }
    if has(&["--uninstall"]) {
        return Some(installer(&["--remove"]));
    }

    // An argument nobody recognises is a typo, and silently running the
    // overlay instead would hide it.
    if let Some(unknown) = arguments.iter().find(|a| a.starts_with('-') && !known(a)) {
        eprintln!("lyricslens: unknown option {unknown}");
        eprintln!("try: lyricslens --help");
        return Some(2);
    }

    None
}

/// Whether this run should stay put rather than starting a copy of itself in
/// the background.
///
/// The child started by [`detach`] carries a marker, or it would fork forever.
pub fn wants_foreground() -> bool {
    holds_terminal() || std::env::var_os(CHILD).is_some()
}

/// Whether there is a terminal to print to.
///
/// The child in the background has none — its output goes nowhere — so its log
/// belongs in a file. Only `--foreground` means someone is watching.
pub fn holds_terminal() -> bool {
    std::env::args().any(|argument| argument == "--foreground")
}

/// Marks the copy that was started in the background, so it does not try to
/// start another.
const CHILD: &str = "LYRICSLENS_DETACHED";

/// Starts a copy of the program detached from the terminal and returns.
///
/// GTK applications normally hold the terminal they were started from. This
/// one is meant to sit there all day, so it lets go by default.
pub fn detach() -> u8 {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let Ok(program) = std::env::current_exe() else {
        eprintln!("lyricslens: could not find my own binary");
        return 1;
    };

    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let mut command = Command::new(program);
    command
        .args(arguments)
        .env(CHILD, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A group of its own, so closing the terminal does not take it along.
    command.process_group(0);

    match command.spawn() {
        Ok(child) => {
            println!("LyricsLens is running (pid {}).", child.id());
            println!("Close it from the icon in the status bar, or with: lyricslens --quit");
            0
        }
        Err(error) => {
            eprintln!("lyricslens: could not start in the background: {error}");
            1
        }
    }
}

/// Every option the program takes. The completions in `packaging/completions`
/// are written by hand from this list.
const FLAGS: &[&str] = &[
    "-h",
    "--help",
    "-V",
    "--version",
    "--paths",
    "--upgrade",
    "--uninstall",
    "--foreground",
    "--settings",
    "--toggle",
    "--song",
    "--position",
    "--sync",
    "--quit",
];

fn known(argument: &str) -> bool {
    FLAGS.contains(&argument)
}

fn paths() {
    let show = |name: &str, path: Option<std::path::PathBuf>| match path {
        Some(path) => println!("{name:10} {}", path.display()),
        None => println!("{name:10} (no home directory)"),
    };
    show(
        "settings",
        crate::store::config_dir().map(|dir| dir.join("settings.toml")),
    );
    show("lyrics", crate::store::cache_dir());
    show("log", crate::log::path());
    println!(
        "{:10} {}",
        "binary",
        std::env::current_exe().unwrap_or_default().display()
    );
}

/// Hands the work to the same script that installed the program.
///
/// Upgrading and removing are the installer's job; doing it twice, in two
/// languages, is how the two drift apart.
fn installer(arguments: &[&str]) -> u8 {
    let mut script = format!("curl -fsSL {INSTALLER} | sh");
    if !arguments.is_empty() {
        script.push_str(" -s --");
        for argument in arguments {
            script.push(' ');
            script.push_str(argument);
        }
    }

    match Command::new("sh").arg("-c").arg(&script).status() {
        Ok(status) => u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
        Err(error) => {
            eprintln!("lyricslens: could not run the installer: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FLAGS;

    // A flag added to the program and forgotten in a completion is offered by
    // no shell, and nothing else would notice.
    #[test]
    fn every_flag_is_completed_in_every_shell() {
        let shells = [
            (
                "bash",
                include_str!("../packaging/completions/lyricslens.bash"),
            ),
            ("zsh", include_str!("../packaging/completions/_lyricslens")),
            (
                "fish",
                include_str!("../packaging/completions/lyricslens.fish"),
            ),
        ];
        for flag in FLAGS.iter().filter(|flag| flag.starts_with("--")) {
            for (shell, script) in shells {
                let written = match shell {
                    "fish" => format!("-l {}", &flag[2..]),
                    _ => flag.to_string(),
                };
                assert!(
                    script.contains(&written),
                    "{shell} does not complete {flag}"
                );
            }
        }
    }
}
