//! The icon in the status bar.
//!
//! A program that sits in the background with no window of its own is
//! invisible without one: there is nothing to click, and no way to close it.
//!
//! GTK4 has no tray of its own. This speaks StatusNotifierItem, the protocol
//! every modern bar implements, over D-Bus.

use crate::i18n::t;
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};

/// The icon is carried in the binary and handed to the bar as pixels.
///
/// Naming a theme icon would work only once the desktop has rescanned its
/// icon directories, which an install into `~/.local` does not make it do.
const ICON: &[u8] = include_bytes!("../../packaging/icons/128.png");

/// What the menu asks the interface to do. The tray lives on its own thread,
/// so it can only send; the GTK side does the work.
#[derive(Debug, Clone, Copy)]
pub enum Command {
    Toggle,
    Song,
    Position,
    Settings,
    Quit,
}

struct Tray {
    commands: async_channel::Sender<Command>,
    /// Decoded once: the bar asks for it again on every redraw.
    icon: Option<ksni::Icon>,
}

impl Tray {
    fn send(&self, command: Command) {
        // Full means the interface is busy and will catch up; closed means it
        // is already gone. Neither is worth a crash from a menu click.
        if let Err(error) = self.commands.try_send(command) {
            tracing::debug!(%error, ?command, "the interface did not take the command");
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "lyricslens".to_owned()
    }

    fn title(&self) -> String {
        "LyricsLens".to_owned()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone().into_iter().collect()
    }

    /// Named only when there are no pixels to give.
    ///
    /// A bar that is handed both prefers the name, and then draws whatever its
    /// icon theme happens to hold — which, for a program installed into
    /// `~/.local`, is often a stale copy or nothing at all.
    fn icon_name(&self) -> String {
        if self.icon.is_some() {
            String::new()
        } else {
            "lyricslens".to_owned()
        }
    }

    /// A left click is the thing people try first.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Command::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: t("Show / hide"),
                activate: Box::new(|tray: &mut Self| tray.send(Command::Toggle)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: t("Whole song…"),
                activate: Box::new(|tray: &mut Self| tray.send(Command::Song)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: t("Move the overlay"),
                activate: Box::new(|tray: &mut Self| tray.send(Command::Position)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: t("Preferences…"),
                activate: Box::new(|tray: &mut Self| tray.send(Command::Settings)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: t("Quit"),
                activate: Box::new(|tray: &mut Self| tray.send(Command::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Decodes the icon into what the protocol asks for: ARGB32, most significant
/// byte first.
fn icon() -> Option<ksni::Icon> {
    let decoder = png::Decoder::new(std::io::Cursor::new(ICON));
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;

    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        tracing::debug!("the icon is not 8-bit RGBA");
        return None;
    }

    let mut data = Vec::with_capacity(pixels.len());
    let rgba = &pixels[..info.buffer_size()];
    let mut at = 0;
    while at + 4 <= rgba.len() {
        let (r, g, b, a) = (rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]);
        data.extend_from_slice(&[a, r, g, b]);
        at += 4;
    }

    Some(ksni::Icon {
        width: i32::try_from(info.width).ok()?,
        height: i32::try_from(info.height).ok()?,
        data,
    })
}

/// Puts the icon in the bar and hands back what the menu asks for.
///
/// A desktop with no status bar simply has nowhere to put it; the overlay runs
/// exactly the same, so this never fails the program.
pub fn start() -> async_channel::Receiver<Command> {
    let (commands, receiver) = async_channel::bounded(8);

    std::thread::Builder::new()
        .name("tray".to_owned())
        .spawn(move || {
            let tray = Tray {
                commands,
                icon: icon(),
            };
            // A sandbox may only own the names it was granted, and the
            // icon is found by its connection anyway.
            match tray.disable_dbus_name(crate::desktop::sandboxed()).spawn() {
                // The handle has to outlive the icon, and the icon lives as long
                // as the program does.
                Ok(handle) => std::mem::forget(handle),
                Err(error) => tracing::info!(%error, "no status bar to put an icon in"),
            }
        })
        .expect("spawning a thread");

    receiver
}
