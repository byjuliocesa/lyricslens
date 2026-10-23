//! The lyrics that sit above everything else.
//!
//! A plain `gtk::Window`, never libadwaita: its background is opaque and
//! fights the transparency the whole overlay depends on.
//!
//! The lines are one column that only ever scrolls. A line never appears at
//! the centre out of nothing: it is already on screen underneath, smaller and
//! fainter, and when its turn comes the whole column slides up by one row
//! while that same widget grows into place and the one above it leaves.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib::ControlFlow;
use gtk::pango;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow, CssProvider, Label};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::error::Error;
use crate::store::settings::{Position, Settings};

mod x11;

/// How long a row takes to climb into the one above's place.
///
/// Short enough to be over before the next line is due: a slide still running
/// when the song moves on reads as lag, not as motion.
const SLIDE: Duration = Duration::from_millis(320);

/// The gap between rows.
const SPACING: i32 = 6;

/// How small a line is while it waits its turn, and how faint.
const WAITING_SCALE: f64 = 0.7;
const WAITING_OPACITY: f64 = 0.45;

/// How faint the words that have not been sung yet are, out of 65535.
const UNSUNG_ALPHA: u16 = 0x6000;

/// The line being sung and three waiting. More is a wall of text.
const ROWS: usize = 4;

/// An input region bigger than any screen. Wayland clips it to the surface,
/// which is the point: the size stops being something to get right.
const EVERYTHING: i32 = 32_767;

/// Marks the window while it is being moved, so there is something to grab.
const POSITIONING: &str = "positioning";

/// Everything here is scoped to this class. Without it the same rules reach
/// the preferences window, which then has no background and enormous text.
const OVERLAY: &str = "lyricslens-overlay";

/// The overlay's look as CSS, scoped to `scope`.
///
/// The preferences window draws a preview with the same rules under its own
/// class, so what it shows cannot drift from what the overlay does.
pub(crate) fn style(settings: &Settings, scope: &str) -> String {
    let shadow = if settings.text_shadow {
        "text-shadow: 0 2px 6px rgba(0, 0, 0, 0.9);"
    } else {
        "text-shadow: none;"
    };
    let strip = if settings.background_opacity > 0.0 {
        format!(
            "background: rgba(0, 0, 0, {:.2}); border-radius: {}px;",
            settings.background_opacity.clamp(0.0, 1.0),
            settings.corner_radius
        )
    } else {
        "background: transparent;".to_owned()
    };
    // An empty name leaves the desktop's own font alone, which is what most
    // people want and nobody has to type.
    let family = if settings.font_family.trim().is_empty() {
        String::new()
    } else {
        format!("font-family: \"{}\";", settings.font_family.trim())
    };

    format!(
        "{scope} {{ background: transparent; }}
         {scope} .lines {{ {strip} padding: 10px 24px; }}
         {scope}.{POSITIONING} .lines {{
             background: rgba(0, 0, 0, 0.55);
             border: 2px dashed rgba(255, 255, 255, 0.75);
             border-radius: 14px;
         }}
         {scope} .pixel {{ background: rgba(0, 0, 0, 0.01); }}
         {scope} label.track {{
             font-size: {small}px;
             font-weight: 400;
             opacity: 0.6;
             margin-bottom: 4px;
         }}
         {scope} picture.art {{
             border-radius: 8px;
             margin-right: 14px;
         }}
         {scope} progressbar.elapsed {{ margin-top: 8px; }}
         {scope} progressbar.elapsed trough {{
             min-height: 3px;
             border-radius: 3px;
             background: rgba(255, 255, 255, 0.18);
         }}
         {scope} progressbar.elapsed progress {{
             min-height: 3px;
             border-radius: 3px;
             background: {color};
         }}
         {scope} label {{
             color: {color};
             font-size: {size}px;
             font-weight: {weight};
             {family}
             {shadow}
         }}",
        color = settings.text_color,
        size = settings.font_size,
        weight = settings.font_weight.clamp(100, 900),
        small = (settings.font_size * 6 / 10).max(10),
    )
}

/// How much of a line has been sung, as a byte offset into it.
///
/// The highlight lands on word boundaries rather than in the middle of a word.
/// Lyrics carry one timestamp per line, so where a word falls inside it is an
/// estimate: the line's time is shared out by how long each word is. That
/// reads as karaoke, which a smooth wipe never does.
fn sung_bytes(text: &str, progress: f64) -> usize {
    let words: Vec<(usize, usize)> =
        text.char_indices()
            .fold(Vec::new(), |mut words: Vec<(usize, usize)>, (at, c)| {
                if c.is_whitespace() {
                    return words;
                }
                match words.last_mut() {
                    Some(word) if word.1 == at => word.1 = at + c.len_utf8(),
                    _ => words.push((at, at + c.len_utf8())),
                }
                words
            });

    let total: usize = words.iter().map(|(from, to)| to - from).sum();
    if total == 0 {
        return text.len();
    }

    let mut sung = 0usize;
    let mut ends = 0usize;
    for (from, to) in &words {
        if sung as f64 / total as f64 >= progress {
            break;
        }
        sung += to - from;
        ends = *to;
    }
    ends
}

/// One line on screen.
///
/// The same widget carries a line from the bottom of the column to the centre.
/// That is the whole point: nothing is removed and drawn again somewhere else.
#[derive(Clone)]
struct Row {
    text: Label,
}

impl Row {
    fn new() -> Self {
        let text = Label::builder()
            .justify(gtk::Justification::Center)
            .wrap(true)
            .halign(gtk::Align::Center)
            .build();
        Self { text }
    }

    fn root(&self) -> &Label {
        &self.text
    }

    /// Which edge the words line up with, and where they wrap.
    fn align(&self, align: gtk::Align, justify: gtk::Justification, width: i32) {
        self.text.set_halign(align);
        self.text.set_justify(justify);
        // Wrapping is what `max_width` is really about: the surface is as wide
        // as its longest line until this stops it.
        self.text.set_size_request(-1, -1);
        self.text.set_max_width_chars(-1);
        self.text.set_width_request(-1);
        self.text.set_wrap(true);
        self.text.set_xalign(match align {
            gtk::Align::Start => 0.0,
            gtk::Align::End => 1.0,
            _ => 0.5,
        });
        let _ = width;
    }

    fn set_text(&self, text: &str) {
        self.text.set_text(text);
        self.text.set_visible(!text.is_empty());
    }

    fn text(&self) -> String {
        self.text.text().to_string()
    }

    /// How big and how bright, from 0 while waiting to 1 while being sung, and
    /// how far the singer has got through it.
    ///
    /// The words already sung are drawn at full strength and the rest faded,
    /// by colouring two ranges of the same text. A second label on top, cut to
    /// width, was the obvious way and the wrong one: a box grows to whatever
    /// its child asks for, so the copy always covered the whole line.
    fn paint(&self, weight: f64, sung: Option<usize>) {
        let weight = weight.clamp(0.0, 1.0);
        let attributes = pango::AttrList::new();
        attributes.insert(pango::AttrFloat::new_scale(
            WAITING_SCALE + (1.0 - WAITING_SCALE) * weight,
        ));

        if let Some(sung) = sung {
            let bytes = u32::try_from(sung).unwrap_or(u32::MAX);
            let mut faded = pango::AttrInt::new_foreground_alpha(UNSUNG_ALPHA);
            faded.set_start_index(bytes);
            attributes.insert(faded);
        }

        self.text.set_attributes(Some(&attributes));
        self.text
            .set_opacity(WAITING_OPACITY + (1.0 - WAITING_OPACITY) * weight);
    }

    fn set_weight(&self, weight: f64) {
        self.paint(weight, None);
    }

    /// Shrinking and fading as it leaves the top of the column.
    fn set_leaving(&self, progress: f64) {
        let attributes = pango::AttrList::new();
        attributes.insert(pango::AttrFloat::new_scale(1.0 - 0.15 * progress));
        self.text.set_attributes(Some(&attributes));
        self.text.set_opacity(1.0 - progress);
    }
}

#[derive(Clone)]
pub struct Overlay {
    window: ApplicationWindow,
    /// The strip the lines sit on, background and all.
    lines: gtk::Box,
    /// Shows a fixed number of rows of the column, and scrolls between them.
    viewport: gtk::ScrolledWindow,
    column: gtk::Box,
    rows: Vec<Row>,
    track: Label,
    elapsed: gtk::ProgressBar,
    art: gtk::Picture,
    /// The cover already on screen, so the same file is not decoded on every
    /// tick.
    showing: Rc<RefCell<Option<PathBuf>>>,
    provider: CssProvider,
    motion: Rc<RefCell<Motion>>,
    /// Which row is the one being sung. One when the line before it is shown,
    /// zero when it is not.
    highlighted: Rc<Cell<usize>>,
    /// How far through the line being sung the song is, as of the last tick.
    /// The slide reads it so the line climbing into place arrives already lit
    /// as far as it should be.
    progress: Rc<Cell<f64>>,
    placement: Rc<RefCell<Placement>>,
    /// False where the compositor has no layer-shell and the window fell back
    /// to X11. Every placement decision differs between the two.
    layer_shell: bool,
}

/// What is on screen, and what is waiting to be.
struct Motion {
    /// Row 0 is the line being sung; the rest are waiting their turn.
    shown: Vec<String>,
    wanted: Vec<String>,
    running: bool,
}

/// Where the overlay sits, and whether it is being moved right now.
struct Placement {
    settings: Settings,
    /// The margins when the drag started, so the drag is relative to them.
    grabbed: Option<(i32, i32)>,
    positioning: bool,
}

impl Overlay {
    /// Builds the window and puts it on screen.
    pub fn build(app: &Application, settings: &Settings) -> Result<Self, Error> {
        let display = gtk::gdk::Display::default().ok_or(Error::NoDisplay)?;

        let provider = CssProvider::new();
        provider.load_from_string(&style(settings, &format!("window.{OVERLAY}")));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(SPACING)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .build();

        let rows: Vec<Row> = (0..ROWS).map(|_| Row::new()).collect();
        for (index, row) in rows.iter().enumerate() {
            row.root().set_visible(false);
            row.set_weight(if index == 0 { 1.0 } else { 0.0 });
            column.append(row.root());
        }

        // A window onto the column, scrolled by hand. Doing the clipping any
        // other way means a box that grows by exactly as much as the column
        // moves, and the movement cancels itself out on screen.
        let viewport = gtk::ScrolledWindow::builder()
            .child(&column)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .propagate_natural_height(false)
            .propagate_natural_width(true)
            .build();

        // What is playing, and how far in. Both off unless asked for: the
        // lyrics are what the overlay is for.
        let track = Label::builder()
            .justify(gtk::Justification::Center)
            .halign(gtk::Align::Center)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .visible(false)
            .build();
        track.add_css_class("track");

        let elapsed = gtk::ProgressBar::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        elapsed.add_css_class("elapsed");

        let words = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        words.append(&track);
        words.append(&viewport);
        words.append(&elapsed);

        // Square, cropped rather than squashed: covers are square, and the
        // ones that are not would otherwise stretch a face sideways.
        let art = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        art.add_css_class("art");
        // CSS rounds the corners; this is what makes the picture stop at them.
        art.set_overflow(gtk::Overflow::Hidden);

        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .visible(false)
            .build();
        lines.add_css_class("lines");
        lines.append(&art);
        lines.append(&words);

        // The strip goes away with the words, and something has to stay
        // behind it: a window with nothing left to draw sends no new frame,
        // and the compositor goes on showing the last one — the line stayed on
        // screen long after the program had stopped drawing it. One pixel,
        // almost but not quite invisible, is enough to keep the frames coming.
        let pixel = gtk::Box::builder()
            .width_request(1)
            .height_request(1)
            .halign(gtk::Align::Center)
            .build();
        pixel.add_css_class("pixel");

        let stack = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .build();
        stack.append(&lines);
        stack.append(&pixel);

        let window = ApplicationWindow::builder()
            .application(app)
            .default_width(900)
            .default_height(220)
            .child(&stack)
            .build();
        window.add_css_class(OVERLAY);

        // The branch is on the protocol, never on which desktop is running.
        // GNOME is the case that lands here, because Mutter does not implement
        // wlr-layer-shell and does not intend to — but so does any plain X11
        // session.
        let layer_shell = gtk4_layer_shell::is_supported();
        if layer_shell {
            window.init_layer_shell();
            // Top does not survive a fullscreen window; Overlay does, and that
            // is the whole point of the project.
            window.set_layer(Layer::Overlay);
            // Without this the compositor shrinks every other window, the way
            // it does for a panel.
            window.set_exclusive_zone(-1);
        } else {
            tracing::info!("no layer-shell here; falling back to an X11 surface");
            window.set_decorated(false);
            x11::keep_above(&window, settings.bottom_margin);
        }

        let overlay = Self {
            window,
            lines,
            viewport,
            column,
            rows,
            track,
            elapsed,
            art,
            showing: Rc::new(RefCell::new(None)),
            provider,
            highlighted: Rc::new(Cell::new(0)),
            progress: Rc::new(Cell::new(0.0)),
            motion: Rc::new(RefCell::new(Motion {
                shown: vec![String::new(); ROWS],
                wanted: vec![String::new(); ROWS],
                running: false,
            })),
            placement: Rc::new(RefCell::new(Placement {
                // Moving the overlay is something done now, not a way to
                // start: a switch left on from last time would claim the mode
                // is on while every click went straight through.
                settings: Settings {
                    movable: false,
                    ..settings.clone()
                },
                grabbed: None,
                positioning: false,
            })),
            layer_shell,
        };

        overlay.shape(settings);
        if settings.movable {
            let mut off = settings.clone();
            off.movable = false;
            if let Err(error) = off.save() {
                tracing::warn!(%error, "could not switch the positioning mode off");
            }
        }
        overlay.place();
        overlay.watch_drag();
        overlay.watch_keys();
        // Placed again once there is a surface: which screen this is comes
        // from the surface, and the position is kept per screen, so the first
        // placement has nothing to look the position up by.
        overlay.window.connect_map({
            let overlay = overlay.clone();
            move |_| overlay.place()
        });
        overlay.window.present();
        // Only once the surface exists is there an input region to empty.
        overlay.set_click_through(true);

        Ok(overlay)
    }

    /// The line being sung and the ones waiting, as of now.
    ///
    /// Called on every tick. Nothing happens unless something changed; when
    /// the line does change, the column slides by one row rather than swapping
    /// its contents where they stand.
    pub fn show(&self, before: Option<&str>, current: Option<&str>, upcoming: &[String]) {
        // The line before comes first, when it is shown at all, and the one
        // being sung moves down a place to make room for it.
        let lead = usize::from(before.is_some() && current.is_some());
        let mut wanted: Vec<String> = Vec::with_capacity(ROWS);
        if lead == 1 {
            wanted.push(before.unwrap_or_default().to_owned());
        }
        wanted.push(current.unwrap_or_default().to_owned());
        wanted.extend(upcoming.iter().take(ROWS - 1 - lead).cloned());
        wanted.resize(ROWS, String::new());
        self.highlighted.set(lead);

        let (changed, moved, running) = {
            let mut motion = self.motion.borrow_mut();
            let changed = motion.wanted != wanted;
            motion.wanted = wanted.clone();
            // The line moved on only if what is being sung now is the line
            // that was waiting directly underneath.
            let moved = motion.shown.get(lead) != wanted.get(lead);
            (changed, moved, motion.running)
        };

        if !changed || running {
            return;
        }
        let arriving = self.rows.get(lead + 1).map(Row::text).unwrap_or_default();
        if moved && !arriving.is_empty() && arriving == wanted[lead] {
            self.motion.borrow_mut().running = true;
            self.slide();
            return;
        }
        // Nothing to slide from: a new song, a seek, or only the tail changed.
        self.settle(&wanted);
    }

    /// How wide the overlay may get, and which edge the words line up with.
    fn shape(&self, settings: &Settings) {
        let width = i32::try_from(settings.max_width.max(120)).unwrap_or(900);
        // The height stays generous: a surface asked for its natural height
        // before anything is on it comes out one pixel tall.
        self.window.set_default_size(width, 220);

        let (align, justify) = match settings.alignment() {
            "start" => (gtk::Align::Start, gtk::Justification::Left),
            "end" => (gtk::Align::End, gtk::Justification::Right),
            _ => (gtk::Align::Center, gtk::Justification::Center),
        };
        self.lines.set_halign(align);
        self.column.set_halign(align);
        // Tied to the text: a cover twice the height of a line sits level with
        // it whatever size the words are.
        let side = i32::try_from(settings.font_size * 2)
            .unwrap_or(60)
            .clamp(48, 160);
        self.art.set_size_request(side, side);
        for row in &self.rows {
            row.align(align, justify, width);
        }
    }

    /// Puts the lines where they belong, with no movement.
    ///
    /// The line being sung is the first row, and the window onto the column
    /// sits at the top of it. Nothing is hidden above: a row that has gone has
    /// already been written over.
    fn settle(&self, wanted: &[String]) {
        let highlighted = self.highlighted.get();
        let waiting = self.waiting(wanted[highlighted].is_empty());

        for (index, row) in self.rows.iter().enumerate() {
            let text = if index <= waiting + highlighted {
                wanted.get(index).cloned().unwrap_or_default()
            } else {
                String::new()
            };
            row.set_text(&text);
            row.set_weight(if index == highlighted { 1.0 } else { 0.0 });
        }

        // Shown first: a hidden widget measures as nothing, and `fit` would
        // size the window onto the column to a single pixel. An instrumental
        // keeps the strip up as long as a line is still coming.
        let anything = wanted[highlighted..=highlighted + waiting.min(1)]
            .iter()
            .any(|text| !text.is_empty());
        self.lines.set_visible(anything);
        self.fit();
        self.motion.borrow_mut().shown = wanted.to_vec();
    }

    /// How many lines waiting below the one being sung are on screen.
    ///
    /// The settings decide, except in an instrumental: with nothing being
    /// sung, the next line stays up even when no lines waiting were asked
    /// for, so the overlay reads as a pause in the song and not as its end.
    fn waiting(&self, nothing_sung: bool) -> usize {
        let asked = usize::from(self.placement.borrow().settings.upcoming_lines);
        if nothing_sung { asked.max(1) } else { asked }
    }

    /// Sizes the window onto the column, and scrolls it to the line being
    /// sung.
    fn fit(&self) {
        let highlighted = self.highlighted.get();
        let waiting = self.waiting(self.rows[highlighted].text().is_empty());
        let visible = 1 + waiting + highlighted;

        let mut height = 0;
        for row in self.rows.iter().take(visible) {
            if !row.root().is_visible() {
                continue;
            }
            let (_, natural, _, _) = row.root().measure(gtk::Orientation::Vertical, -1);
            height += natural + SPACING;
        }
        if height <= SPACING {
            return;
        }
        self.viewport.set_size_request(-1, height - SPACING);
        self.viewport.vadjustment().set_value(0.0);
    }

    /// Scrolls the column up by one row, over time.
    fn slide(&self) {
        let adjustment = self.viewport.vadjustment();
        // The height it has on screen, not the one it would like: a line that
        // wrapped is taller than its unconstrained measurement, and scrolling
        // by the smaller number leaves the row that left still showing.
        let leaving = self.highlighted.get();
        let climbing = self.rows[leaving + 1].text();
        // Coming out of an instrumental the row above is empty and takes no
        // room: the next line is already at the top, so it only grows into
        // place instead of climbing.
        let step = if self.rows[leaving].text().is_empty() {
            0.0
        } else {
            f64::from(self.rows[0].root().height() + SPACING)
        };
        if step <= f64::from(SPACING) && !self.rows[leaving].text().is_empty() {
            let wanted = self.motion.borrow().wanted.clone();
            self.motion.borrow_mut().running = false;
            self.settle(&wanted);
            return;
        }

        let started = Instant::now();
        let overlay = self.clone();
        let karaoke = self.placement.borrow().settings.karaoke;

        self.column.add_tick_callback(move |_, _| {
            let progress = (started.elapsed().as_secs_f64() / SLIDE.as_secs_f64()).min(1.0);
            // Slow at both ends: a row that starts and stops gently reads as
            // one movement rather than a jump that was slowed down.
            let eased = if progress < 0.5 {
                4.0 * progress.powi(3)
            } else {
                1.0 - (-2.0f64).mul_add(progress, 2.0).powi(3) / 2.0
            };

            adjustment.set_value(step * eased);
            overlay.rows[leaving].set_leaving(eased);
            overlay.rows[leaving + 1].paint(
                eased,
                karaoke.then(|| sung_bytes(&climbing, overlay.progress.get())),
            );

            if progress < 1.0 {
                return ControlFlow::Continue;
            }

            // The row that climbed is now the one being sung, and every text
            // moves down a place so the same thing can happen again.
            let wanted = overlay.motion.borrow().wanted.clone();
            overlay.motion.borrow_mut().running = false;
            overlay.settle(&wanted);
            ControlFlow::Break
        });
    }

    /// Who is playing what, above the lyrics, and how far into it, below.
    pub fn show_track(&self, name: Option<&str>, elapsed: Option<f64>) {
        match name {
            Some(name) => {
                self.track.set_text(name);
                self.track.set_visible(true);
            }
            None => self.track.set_visible(false),
        }

        match elapsed {
            Some(fraction) => {
                self.elapsed.set_fraction(fraction.clamp(0.0, 1.0));
                self.elapsed.set_visible(true);
            }
            None => self.elapsed.set_visible(false),
        }
    }

    /// The cover beside the lyrics, or none.
    ///
    /// The file is decoded only when it is a different file: this runs on
    /// every tick, and decoding a picture thirty times a second is the kind
    /// of thing that makes an overlay stutter.
    pub fn show_art(&self, path: Option<&Path>) {
        if self.showing.borrow().as_deref() == path {
            return;
        }
        *self.showing.borrow_mut() = path.map(Path::to_path_buf);

        let Some(path) = path else {
            self.art.set_visible(false);
            return;
        };

        match gtk::gdk::Texture::from_filename(path) {
            Ok(texture) => {
                self.art.set_paintable(Some(&texture));
                self.art.set_visible(true);
            }
            Err(error) => {
                tracing::debug!(%error, path = %path.display(), "the cover would not decode");
                self.art.set_visible(false);
            }
        }
    }

    /// How far through the line the song is, from 0 to 1.
    ///
    /// Does nothing unless karaoke is on, and nothing while the column is
    /// moving: a half-drawn line under a sliding one reads as a glitch.
    pub fn show_progress(&self, progress: Option<f64>) {
        self.progress.set(progress.unwrap_or(0.0));

        // The slide paints the row that is climbing, with this same value. Two
        // hands on the same line is what made it flash: lit on the way up,
        // then dark again the moment it arrived.
        if self.motion.borrow().running {
            return;
        }

        let karaoke = self.placement.borrow().settings.karaoke;
        let row = &self.rows[0];
        let Some(progress) = progress.filter(|_| karaoke) else {
            row.paint(1.0, None);
            return;
        };
        row.paint(1.0, Some(sung_bytes(&row.text(), progress)));
    }

    /// Takes the settings again, after the preferences window changed them.
    pub fn reload(&self, settings: &Settings) {
        self.provider
            .load_from_string(&style(settings, &format!("window.{OVERLAY}")));
        self.shape(settings);
        // While the lines are being dragged the overlay is the one that knows
        // where they are; the window that switched the mode off only knows
        // where they were when it opened.
        let dragging = self.placement.borrow().positioning;
        let mut next = settings.clone();
        if dragging {
            let placement = self.placement.borrow();
            next.positions = placement.settings.positions.clone();
            next.left_margin = placement.settings.left_margin;
            next.bottom_margin = placement.settings.bottom_margin;
        }
        self.placement.borrow_mut().settings = next;
        self.set_positioning(settings.movable);
        if dragging && !settings.movable {
            self.save_position();
        }
    }

    /// Brings the overlay back to the screen, which is what a second launch
    /// of the program should do instead of drawing another one.
    pub fn present(&self) {
        self.window.set_visible(true);
        self.window.present();
        self.set_click_through(!self.placement.borrow().positioning);
    }

    /// Hides the overlay, or brings it back.
    pub fn toggle(&self) {
        let visible = self.window.is_visible();
        self.window.set_visible(!visible);
        if !visible {
            self.set_click_through(!self.placement.borrow().positioning);
        }
    }

    /// Enters or leaves the mode where the overlay can be dragged.
    ///
    /// A layer surface cannot be moved by the pointer: its position comes from
    /// an anchor and a margin, and moving it under the cursor would drag the
    /// cursor's own frame of reference along. So the mode grows the surface to
    /// cover the screen and moves the lines inside it, which stays still.
    pub fn toggle_positioning(&self) {
        let positioning = !self.placement.borrow().positioning;
        self.set_positioning(positioning);
        if !positioning {
            self.save_position();
        }
    }

    /// Enters or leaves the positioning mode, without saving anything: the
    /// preferences window saves for itself, the shortcut through
    /// `toggle_positioning`.
    fn set_positioning(&self, positioning: bool) {
        {
            let mut placement = self.placement.borrow_mut();
            placement.positioning = positioning;
            // The switch in the preferences window reads this, so the two
            // cannot be left disagreeing.
            placement.settings.movable = positioning;
            if !positioning {
                placement.grabbed = None;
            }
        }

        if positioning {
            self.window.add_css_class(POSITIONING);
            self.window.set_visible(true);
            self.lines.set_visible(true);
            let row = &self.rows[self.highlighted.get()];
            if row.text().is_empty() {
                row.set_text(&crate::i18n::t("drag me"));
                row.set_weight(1.0);
            }
        } else {
            self.window.remove_css_class(POSITIONING);
        }
        // Escape ends the mode, which needs the keyboard; the rest of the time
        // the overlay must never take it from the window being typed in.
        if self.layer_shell {
            self.window.set_keyboard_mode(if positioning {
                KeyboardMode::OnDemand
            } else {
                KeyboardMode::None
            });
        }
        self.place();
        self.set_click_through(!positioning);
    }

    fn save_position(&self) {
        let placement = self.placement.borrow();
        if let Err(error) = placement.settings.save() {
            tracing::warn!(%error, "could not save where the overlay was left");
        } else {
            tracing::info!(
                bottom = placement.settings.bottom_margin,
                left = ?placement.settings.left_margin,
                "overlay position saved"
            );
        }
    }

    /// Escape or Enter puts the overlay down where it is, for when the
    /// preferences window is out of reach.
    fn watch_keys(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let overlay = self.clone();
            move |_, key, _, _| {
                let done = matches!(
                    key,
                    gtk::gdk::Key::Escape | gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter
                );
                if done && overlay.placement.borrow().positioning {
                    overlay.toggle_positioning();
                    return gtk::glib::Propagation::Stop;
                }
                gtk::glib::Propagation::Proceed
            }
        });
        self.window.add_controller(keys);
    }

    /// Lets the pointer through to whatever is underneath, or takes it.
    fn set_click_through(&self, through: bool) {
        let Some(surface) = self.window.surface() else {
            return;
        };
        if through {
            // An empty region means the surface wants no pointer events at all.
            surface.set_input_region(Some(&gtk::cairo::Region::create()));
            return;
        }
        let region = match self.grab_area() {
            Some(area) => area,
            // Not the window's own size, which is the trap here: this runs
            // while the surface may still be the small one it was before the
            // positioning mode asked the compositor to grow it to the whole
            // screen. A region that size covers the top-left corner and
            // nothing else, and the lines, at the bottom, cannot be grabbed.
            None => gtk::cairo::RectangleInt::new(0, 0, EVERYTHING, EVERYTHING),
        };
        surface.set_input_region(Some(&gtk::cairo::Region::create_rectangle(&region)));
    }

    /// The lines and a margin around them, in the coordinates of the surface
    /// that covers the screen while they are being moved.
    ///
    /// Only that much takes the pointer. A surface over the whole screen that
    /// took every click left the preferences window, the one place to switch
    /// the mode off, impossible to reach.
    fn grab_area(&self) -> Option<gtk::cairo::RectangleInt> {
        const AROUND: i32 = 24;
        if !self.layer_shell {
            return None;
        }
        let surface = self.window.surface()?;
        let monitor = surface.display().monitor_at_surface(&surface)?;
        let height = monitor.geometry().height();
        let (width, tall) = match (self.lines.width(), self.lines.height()) {
            (0, _) | (_, 0) => {
                let (_, w, _, _) = self.lines.measure(gtk::Orientation::Horizontal, -1);
                let (_, h, _, _) = self.lines.measure(gtk::Orientation::Vertical, w);
                (w, h)
            }
            size => size,
        };
        let left = self.lines.margin_start();
        let bottom = self.lines.margin_bottom();
        Some(gtk::cairo::RectangleInt::new(
            (left - AROUND).max(0),
            (height - bottom - tall - AROUND).max(0),
            width + 2 * AROUND,
            tall + 2 * AROUND,
        ))
    }

    /// Turns a drag into margins, live, while the positioning mode is on.
    fn watch_drag(&self) {
        let drag = gtk::GestureDrag::new();

        drag.connect_drag_begin({
            let overlay = self.clone();
            move |_, _, _| {
                let screen = overlay.screen();
                let mut placement = overlay.placement.borrow_mut();
                if !placement.positioning {
                    return;
                }
                let at = placement.settings.position_on(screen.as_deref());
                let left = at.left.unwrap_or_else(|| overlay.centred_left());
                placement.grabbed = Some((left, at.bottom));
            }
        });

        drag.connect_drag_update({
            let overlay = self.clone();
            move |_, x, y| {
                let screen = overlay.screen();
                {
                    let mut placement = overlay.placement.borrow_mut();
                    let Some((left, bottom)) = placement.grabbed else {
                        return;
                    };
                    // Dragging down moves the lines down, which is a smaller
                    // distance from the bottom.
                    let at = Position {
                        left: Some((left + x as i32).max(0)),
                        bottom: (bottom - y as i32).max(0),
                    };
                    placement.settings.set_position_on(screen.as_deref(), at);
                }
                overlay.place();
                overlay.set_click_through(false);
            }
        });

        drag.connect_drag_end({
            let overlay = self.clone();
            move |_, _, _| {
                overlay.placement.borrow_mut().grabbed = None;
            }
        });

        self.window.add_controller(drag);
    }

    /// Puts the lines where the settings say.
    fn place(&self) {
        let screen = self.screen();
        let placement = self.placement.borrow();
        let positioning = placement.positioning;
        let at = placement.settings.position_on(screen.as_deref());
        let (left, bottom) = (at.left, at.bottom);
        drop(placement);

        if !self.layer_shell {
            x11::place(&self.window, left, bottom);
            return;
        }

        self.choose_monitor();

        if positioning {
            // The surface covers the screen and stops moving; the lines move
            // inside it, so the pointer and the thing it drags stay in the
            // same frame of reference.
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                self.window.set_anchor(edge, true);
                self.window.set_margin(edge, 0);
            }
            self.lines.set_halign(gtk::Align::Start);
            self.lines
                .set_margin_start(left.unwrap_or_else(|| self.centred_left()));
            self.lines.set_margin_bottom(bottom);
            return;
        }

        self.lines.set_halign(gtk::Align::Center);
        self.lines.set_margin_start(0);
        self.lines.set_margin_bottom(0);
        self.window.set_anchor(Edge::Top, false);
        self.window.set_anchor(Edge::Right, false);
        self.window.set_anchor(Edge::Bottom, true);
        self.window.set_margin(Edge::Bottom, bottom);
        // Anchoring left is what makes the left margin mean anything; with no
        // anchor the compositor centres the surface.
        self.window.set_anchor(Edge::Left, left.is_some());
        self.window.set_margin(Edge::Left, left.unwrap_or(0));
    }

    /// Binds the surface to the screen the settings name.
    ///
    /// A layer surface belongs to one output. Moving between screens is not a
    /// drag but a different surface, so the screen is a setting.
    fn choose_monitor(&self) {
        let wanted = self.placement.borrow().settings.monitor.clone();
        let Some(wanted) = wanted else {
            return;
        };
        let Some(display) = gtk::gdk::Display::default() else {
            return;
        };

        let monitors = display.monitors();
        for index in 0..monitors.n_items() {
            let Some(monitor) = monitors.item(index).and_downcast::<gtk::gdk::Monitor>() else {
                continue;
            };
            if monitor.connector().is_some_and(|name| name == wanted) {
                self.window.set_monitor(Some(&monitor));
                return;
            }
        }
        tracing::warn!(%wanted, "no screen by that name; leaving it to the compositor");
    }

    /// The screen the overlay is on, by connector name.
    ///
    /// The chosen screen wins over the one the surface is on. They disagree
    /// for a moment every time the program starts: the surface is mapped
    /// wherever the compositor puts it and only then moved, and reading the
    /// surface in between gives the position of the wrong screen.
    fn screen(&self) -> Option<String> {
        if let Some(wanted) = self.placement.borrow().settings.monitor.clone() {
            return Some(wanted);
        }
        let surface = self.window.surface()?;
        let monitor = surface.display().monitor_at_surface(&surface)?;
        monitor.connector().map(|name| name.to_string())
    }

    /// Where the lines sit when centred, so a drag can start from there.
    fn centred_left(&self) -> i32 {
        let Some(surface) = self.window.surface() else {
            return 0;
        };
        let Some(monitor) = surface.display().monitor_at_surface(&surface) else {
            return 0;
        };
        (monitor.geometry().width() - self.lines.width().max(1)) / 2
    }
}
