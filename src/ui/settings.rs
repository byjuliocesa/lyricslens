//! The preferences window.
//!
//! The one place libadwaita earns its keep: rows that already look right and
//! already behave, instead of a hand-built grid. It never touches the overlay,
//! whose transparency libadwaita's own background would fight.
//!
//! Four pages, the way the GNOME guidelines lay preferences out: how the words
//! look, which words and when, where they sit, and the rest.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib, pango};
use gtk4 as gtk;

use crate::app::Request;
use crate::i18n::{count, t};
use crate::lyrics::lrclib::Candidate;
use crate::lyrics::normalize::from_track;
use crate::media::Track;
use crate::store::autostart;
use crate::store::settings::Settings;

/// What the window needs to look lyrics up and hand a choice back.
///
/// Absent when the preferences were opened on their own, with no overlay
/// running: there is nothing playing to correct.
#[derive(Clone)]
pub struct Search {
    pub requests: async_channel::Sender<Request>,
    pub candidates: async_channel::Receiver<Vec<Candidate>>,
    pub playing: Rc<RefCell<Track>>,
    /// What became of the automatic search for the track playing now.
    pub report: Rc<RefCell<String>>,
    /// A release newer than this one, when there is one.
    pub newer: Rc<RefCell<Option<String>>>,
}

/// The weights offered, by the name a font gives them.
const WEIGHTS: [(u32, &str); 6] = [
    (300, "Light"),
    (400, "Regular"),
    (500, "Medium"),
    (600, "Semibold"),
    (700, "Bold"),
    (800, "Heavy"),
];

/// The height of one recording in the list of those found, title and subtitle.
const RESULT_ROW: i32 = 58;

/// How tall that list may grow before it scrolls: five rows and half of the
/// next, so the cut-off row shows there is more below.
const RESULTS_HEIGHT: i32 = RESULT_ROW * 11 / 2;

/// Opens the preferences window, saving each change as it is made.
pub fn open(app: &adw::Application) {
    show(app, None);
}

/// Opens the preferences, with the means to correct the lyrics when there is a
/// track playing to correct them for.
pub fn show(app: &adw::Application, search: Option<Search>) {
    let settings = Rc::new(RefCell::new(Settings::load()));
    let preview = Rc::new(Preview::new());
    preview.refresh(&settings.borrow());
    let ctx = Ctx {
        settings,
        app: app.clone(),
        preview: preview.clone(),
    };

    let window = adw::PreferencesWindow::builder()
        .application(app)
        .title("LyricsLens")
        .search_enabled(true)
        .default_width(640)
        .default_height(760)
        .build();

    window.add(&appearance_page(&ctx));
    window.add(&lyrics_page(&ctx, search.as_ref()));
    window.add(&position_page(&ctx));
    window.add(&general_page(&ctx, search.as_ref()));

    window.connect_close_request(move |_| {
        preview.detach();
        glib::Propagation::Proceed
    });
    window.present();
}

/// The settings being edited, and what has to follow every change.
#[derive(Clone)]
struct Ctx {
    settings: Rc<RefCell<Settings>>,
    app: adw::Application,
    preview: Rc<Preview>,
}

impl Ctx {
    fn change(&self, edit: impl FnOnce(&mut Settings)) {
        edit(&mut self.settings.borrow_mut());
        save(&self.settings.borrow(), &self.app);
        self.preview.refresh(&self.settings.borrow());
    }

    fn get<T>(&self, read: impl FnOnce(&Settings) -> T) -> T {
        read(&self.settings.borrow())
    }
}

fn page(title: &str, icon: &str) -> adw::PreferencesPage {
    adw::PreferencesPage::builder()
        .title(t(title))
        .icon_name(icon)
        .build()
}

fn group(title: &str, description: &str) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(t(title)).build();
    if !description.is_empty() {
        group.set_description(Some(&t(description)));
    }
    group
}

fn switch(
    ctx: &Ctx,
    title: &str,
    subtitle: &str,
    read: fn(&Settings) -> bool,
    write: fn(&mut Settings, bool),
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(t(title))
        .active(ctx.get(read))
        .build();
    if !subtitle.is_empty() {
        row.set_subtitle(&t(subtitle));
    }
    let ctx = ctx.clone();
    row.connect_active_notify(move |row| ctx.change(|s| write(s, row.is_active())));
    row
}

fn spin(
    ctx: &Ctx,
    title: &str,
    subtitle: &str,
    (low, high, step): (f64, f64, f64),
    value: f64,
    write: fn(&mut Settings, f64),
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(low, high, step);
    row.set_title(&t(title));
    if !subtitle.is_empty() {
        row.set_subtitle(&t(subtitle));
    }
    row.set_value(value);
    let ctx = ctx.clone();
    row.connect_value_notify(move |row| ctx.change(|s| write(s, row.value())));
    row
}

fn appearance_page(ctx: &Ctx) -> adw::PreferencesPage {
    let page = page("Appearance", "color-select-symbolic");

    let shown = adw::PreferencesGroup::new();
    shown.add(&ctx.preview.root);
    page.add(&shown);

    let text = group("Text", "");

    let font = adw::ActionRow::builder().title(t("Font")).build();
    let picker = gtk::FontDialogButton::builder()
        .dialog(&gtk::FontDialog::new())
        .level(gtk::FontLevel::Family)
        .use_font(false)
        .use_size(false)
        .valign(gtk::Align::Center)
        .build();
    // Empty means the desktop's font, so that is the one the button names.
    let family = ctx.get(|s| s.font_family.trim().to_owned());
    let shown = if family.is_empty() {
        desktop_font()
    } else {
        family
    };
    picker.set_font_desc(&pango::FontDescription::from_string(&shown));
    let reset = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text(t("Use the desktop font"))
        .valign(gtk::Align::Center)
        .build();
    reset.add_css_class("flat");
    // Unset follows the desktop, and the row says so; there is nothing to
    // reset until a font has been picked.
    let follows = {
        let font = font.clone();
        let reset = reset.clone();
        move |unset: bool| {
            font.set_subtitle(&if unset {
                t("Follows the desktop")
            } else {
                String::new()
            });
            reset.set_sensitive(!unset);
        }
    };
    follows(ctx.get(|s| s.font_family.trim().is_empty()));
    // Set while the button is pointed back at the desktop's font, which is
    // not a choice of font and must not be stored as one.
    let resetting = Rc::new(std::cell::Cell::new(false));
    picker.connect_font_desc_notify({
        let ctx = ctx.clone();
        let follows = follows.clone();
        let resetting = resetting.clone();
        move |picker| {
            if resetting.get() {
                return;
            }
            let family = picker
                .font_desc()
                .and_then(|desc| desc.family())
                .map(|family| family.to_string())
                .unwrap_or_default();
            ctx.change(|s| s.font_family = family);
            follows(false);
        }
    });
    reset.connect_clicked({
        let ctx = ctx.clone();
        let picker = picker.clone();
        move |_| {
            ctx.change(|s| s.font_family.clear());
            resetting.set(true);
            picker.set_font_desc(&pango::FontDescription::from_string(&desktop_font()));
            resetting.set(false);
            follows(true);
        }
    });
    font.add_suffix(&picker);
    font.add_suffix(&reset);
    text.add(&font);

    text.add(&spin(
        ctx,
        "Size",
        "",
        (12.0, 96.0, 1.0),
        f64::from(ctx.get(|s| s.font_size)),
        |s, v| s.font_size = v.max(1.0) as u32,
    ));

    let names: Vec<String> = WEIGHTS.iter().map(|(_, name)| t(name)).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let current = ctx.get(|s| s.font_weight);
    let weight = adw::ComboRow::builder()
        .title(t("Weight"))
        .model(&gtk::StringList::new(&names))
        .selected(nearest_weight(current))
        .build();
    weight.connect_selected_notify({
        let ctx = ctx.clone();
        move |row| {
            let (value, _) = WEIGHTS[(row.selected() as usize).min(WEIGHTS.len() - 1)];
            ctx.change(|s| s.font_weight = value);
        }
    });
    text.add(&weight);

    let colour = adw::ActionRow::builder().title(t("Color")).build();
    let swatch = gtk::ColorDialogButton::builder()
        .dialog(&gtk::ColorDialog::builder().with_alpha(false).build())
        .valign(gtk::Align::Center)
        .build();
    if let Ok(rgba) = gdk::RGBA::parse(ctx.get(|s| s.text_color.clone())) {
        swatch.set_rgba(&rgba);
    }
    swatch.connect_rgba_notify({
        let ctx = ctx.clone();
        move |swatch| ctx.change(|s| s.text_color = hex(&swatch.rgba()))
    });
    colour.add_suffix(&swatch);
    colour.set_activatable_widget(Some(&swatch));
    text.add(&colour);

    text.add(&switch(
        ctx,
        "Shadow",
        "Keeps the words readable over bright windows",
        |s| s.text_shadow,
        |s, on| s.text_shadow = on,
    ));
    page.add(&text);

    let behind = group("Background", "");
    let darkness = adw::ActionRow::builder()
        .title(t("Darkness"))
        .subtitle(t("How dark the strip behind the words is"))
        .build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 5.0);
    scale.set_value(ctx.get(|s| s.background_opacity) * 100.0);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Left);
    scale.set_digits(0);
    scale.set_hexpand(true);
    scale.set_width_request(180);
    scale.set_valign(gtk::Align::Center);
    scale.set_format_value_func(|_, value| format!("{value:.0}%"));
    scale.connect_value_changed({
        let ctx = ctx.clone();
        move |scale| ctx.change(|s| s.background_opacity = scale.value() / 100.0)
    });
    darkness.add_suffix(&scale);
    behind.add(&darkness);
    behind.add(&spin(
        ctx,
        "Rounded Corners",
        "In pixels",
        (0.0, 40.0, 2.0),
        f64::from(ctx.get(|s| s.corner_radius)),
        |s, v| s.corner_radius = v.max(0.0) as u32,
    ));
    page.add(&behind);

    let layout = group("Layout", "");
    let alignments = [t("Center"), t("Left"), t("Right")];
    let alignments: Vec<&str> = alignments.iter().map(String::as_str).collect();
    let align = adw::ComboRow::builder()
        .title(t("Alignment"))
        .model(&gtk::StringList::new(&alignments))
        .selected(match ctx.get(|s| s.alignment().to_owned()).as_str() {
            "start" => 1,
            "end" => 2,
            _ => 0,
        })
        .build();
    align.connect_selected_notify({
        let ctx = ctx.clone();
        move |row| {
            let value = match row.selected() {
                1 => "start",
                2 => "end",
                _ => "center",
            };
            ctx.change(|s| s.align = value.to_owned());
        }
    });
    layout.add(&align);
    layout.add(&spin(
        ctx,
        "Maximum Width",
        "Longer lines wrap. In pixels",
        (200.0, 3840.0, 20.0),
        f64::from(ctx.get(|s| s.max_width)),
        |s, v| s.max_width = v.max(0.0) as u32,
    ));
    page.add(&layout);

    page
}

fn lyrics_page(ctx: &Ctx, search: Option<&Search>) -> adw::PreferencesPage {
    let page = page("Lyrics", "audio-x-generic-symbolic");

    if let Some(search) = search {
        page.add(&wrong_lyrics(search));
    }

    let shown = group("On Screen", "");
    shown.add(&switch(
        ctx,
        "Previous Line",
        "The line just sung, dimmed above",
        |s| s.previous_line,
        |s, on| s.previous_line = on,
    ));
    shown.add(&spin(
        ctx,
        "Upcoming Lines",
        "Dimmed below the current one",
        (0.0, 3.0, 1.0),
        f64::from(ctx.get(|s| s.upcoming_lines)),
        |s, v| s.upcoming_lines = v.max(0.0) as u8,
    ));
    shown.add(&switch(
        ctx,
        "Karaoke",
        "Fills the line as it is sung",
        |s| s.karaoke,
        |s, on| s.karaoke = on,
    ));
    shown.add(&switch(
        ctx,
        "Song Title",
        "Artist and title above the lyrics",
        |s| s.show_track,
        |s, on| s.show_track = on,
    ));
    shown.add(&switch(
        ctx,
        "Progress Bar",
        "Needs a player that reports the song length",
        |s| s.show_progress,
        |s, on| s.show_progress = on,
    ));
    shown.add(&switch(
        ctx,
        "Album Cover",
        "Beside the lyrics. Looked up when the player has none",
        |s| s.show_art,
        |s, on| s.show_art = on,
    ));
    shown.add(&switch(
        ctx,
        "Hide While Paused",
        "Clears the lyrics when nothing is playing",
        |s| s.hide_when_paused,
        |s, on| s.hide_when_paused = on,
    ));
    page.add(&shown);

    page.add(&timing(ctx, search));
    page
}

/// The offsets, for the player being followed and for the song playing now.
fn timing(ctx: &Ctx, search: Option<&Search>) -> adw::PreferencesGroup {
    let timing = group(
        "Timing",
        "Positive delays the lyrics and negative brings them earlier. In milliseconds",
    );

    // The offset is kept per player, and which one is being followed is only
    // known while the overlay runs. Editing the one that is configured is what
    // this window can honestly offer.
    let named = ctx.get(|s| s.player.clone());
    let followed = named.clone().unwrap_or_else(|| "default".to_owned());
    let player = adw::SpinRow::with_range(-5000.0, 5000.0, 50.0);
    player.set_title(&t("This Player"));
    player.set_subtitle(&named.unwrap_or_else(|| t("Any player")));
    player.set_value(ctx.get(|s| s.offset_ms(&followed)) as f64);
    player.connect_value_notify({
        let ctx = ctx.clone();
        move |row| ctx.change(|s| s.set_offset_ms(&followed, row.value() as i64))
    });
    timing.add(&player);

    // The recording playing now, which only the running overlay knows. With
    // nothing playing there is nothing to correct, and the row says so instead
    // of writing a correction against an empty name.
    let playing = search.map(|search| search.playing.borrow().clone());
    let key = playing
        .as_ref()
        .filter(|track| !track.is_empty())
        .map(|track| {
            Settings::track_key(
                &track.artists.join(", "),
                track.title.as_deref().unwrap_or_default(),
                track.length,
            )
        });

    let song = adw::SpinRow::with_range(-5000.0, 5000.0, 50.0);
    song.set_title(&t("This Song"));
    match (&key, &playing) {
        (Some(key), Some(track)) => {
            song.set_subtitle(track.title.as_deref().unwrap_or_default());
            song.set_value(ctx.get(|s| s.track_offset_ms(key)) as f64);
        }
        _ => {
            song.set_subtitle(&t("Nothing playing"));
            song.set_sensitive(false);
        }
    }
    song.connect_value_notify({
        let ctx = ctx.clone();
        move |row| {
            if let Some(key) = key.as_deref() {
                ctx.change(|s| s.set_track_offset_ms(key, row.value() as i64));
            }
        }
    });
    timing.add(&song);
    timing
}

fn position_page(ctx: &Ctx) -> adw::PreferencesPage {
    let page = page("Position", "view-fullscreen-symbolic");

    let place = group("Screen", "");

    // A layer surface belongs to one screen, so this is a choice rather than a
    // drag across the edge.
    let screens = connectors();
    let automatic = t("Automatic");
    let mut names: Vec<&str> = vec![&automatic];
    names.extend(screens.iter().map(String::as_str));
    let screen = adw::ComboRow::builder()
        .title(t("Monitor"))
        .model(&gtk::StringList::new(&names))
        .selected(
            ctx.get(|s| s.monitor.clone())
                .and_then(|wanted| screens.iter().position(|name| *name == wanted))
                .map_or(0, |index| index as u32 + 1),
        )
        .build();
    screen.connect_selected_notify({
        let ctx = ctx.clone();
        move |row| {
            let chosen = (row.selected() as usize)
                .checked_sub(1)
                .and_then(|index| screens.get(index).cloned());
            ctx.change(|s| s.monitor = chosen);
        }
    });
    place.add(&screen);

    place.add(&spin(
        ctx,
        "Distance From Bottom",
        "In pixels",
        (0.0, 800.0, 10.0),
        f64::from(ctx.get(|s| s.bottom_margin)),
        |s, v| s.set_bottom_everywhere(v as i32),
    ));
    place.add(&switch(
        ctx,
        "Drag to Move",
        "The overlay takes clicks while this is on, so it can be dragged",
        |s| s.movable,
        |s, on| s.movable = on,
    ));
    page.add(&place);

    page.add(&shortcuts(ctx));
    page
}

/// A program with no window of its own needs a way out that is not the tray,
/// for the desktops that have none.
fn shortcuts(ctx: &Ctx) -> adw::PreferencesGroup {
    let keys = group(
        "Shortcuts",
        match crate::desktop::compositor() {
            crate::desktop::Compositor::Unknown => {
                "This desktop does not take shortcuts from apps. Add the line below to its configuration"
            }
            _ => {
                "Written like SUPER SHIFT, L. Taken as soon as it is typed, and forgotten when the desktop restarts"
            }
        },
    );

    let toggle = adw::EntryRow::builder()
        .title(t("Show and Hide"))
        .text(ctx.get(|s| s.hotkey_toggle.clone()))
        .build();
    let movement = adw::EntryRow::builder()
        .title(t("Move"))
        .text(ctx.get(|s| s.hotkey_position.clone()))
        .build();

    // Offered whatever the desktop, for the ones that cannot be asked and for
    // anyone who would rather have the binding survive a restart.
    let line = adw::ActionRow::builder()
        .title(t("Line for Your Configuration"))
        .subtitle(crate::desktop::snippet(
            &ctx.get(|s| s.hotkey_toggle.clone()),
            "--toggle",
        ))
        .subtitle_selectable(true)
        .build();
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(t("Copy"))
        .build();
    copy.add_css_class("flat");
    copy.connect_clicked({
        let ctx = ctx.clone();
        move |button| {
            let text = crate::desktop::snippet(&ctx.get(|s| s.hotkey_toggle.clone()), "--toggle");
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&text);
            }
            button.set_tooltip_text(Some(&t("Copied")));
        }
    });
    line.add_suffix(&copy);

    toggle.connect_changed({
        let ctx = ctx.clone();
        let line = line.clone();
        move |row| {
            ctx.change(|s| s.hotkey_toggle = row.text().trim().to_owned());
            let combination = ctx.get(|s| s.hotkey_toggle.clone());
            ask_for(&combination, "--toggle");
            // The line is only useful if it says the combination in the box.
            line.set_subtitle(&crate::desktop::snippet(&combination, "--toggle"));
        }
    });
    movement.connect_changed({
        let ctx = ctx.clone();
        move |row| {
            ctx.change(|s| s.hotkey_position = row.text().trim().to_owned());
            ask_for(&ctx.get(|s| s.hotkey_position.clone()), "--position");
        }
    });

    keys.add(&toggle);
    keys.add(&movement);
    keys.add(&line);
    keys
}

fn general_page(ctx: &Ctx, search: Option<&Search>) -> adw::PreferencesPage {
    let page = page("General", "preferences-system-symbolic");

    let player = group(
        "Player",
        "When several players are open, follow the one whose name contains this, such as spotify",
    );
    let name = adw::EntryRow::builder()
        .title(t("Player Name"))
        .text(ctx.get(|s| s.player.clone()).unwrap_or_default())
        .build();
    name.connect_changed({
        let ctx = ctx.clone();
        move |row| {
            let text = row.text().trim().to_owned();
            ctx.change(|s| s.player = (!text.is_empty()).then_some(text));
        }
    });
    player.add(&name);
    page.add(&player);

    let startup = group("Startup", "");
    let login = adw::SwitchRow::builder()
        .title(t("Start on Login"))
        .subtitle(t("Opens when you log in, with the overlay ready"))
        .active(autostart::enabled())
        .build();
    login.connect_active_notify(|row| {
        if let Err(error) = autostart::set(row.is_active()) {
            tracing::warn!(%error, "could not change the autostart entry");
            // Says what is true, not what was asked for.
            row.set_active(autostart::enabled());
        }
    });
    startup.add(&login);
    page.add(&startup);

    if let Some(version) = search.and_then(|search| search.newer.borrow().clone()) {
        let news = group("Update Available", "");
        let row = adw::ActionRow::builder()
            .title(format!("LyricsLens {version}"))
            .subtitle(format!("{} lyricslens --upgrade", t("Install it with")))
            .subtitle_selectable(true)
            .build();
        news.add(&row);
        page.add(&news);
    }

    let close = adw::PreferencesGroup::new();
    let quit = gtk::Button::builder()
        .label(t("Quit LyricsLens"))
        .halign(gtk::Align::Center)
        .tooltip_text(t("Closes the overlay and the status bar icon"))
        .build();
    quit.add_css_class("pill");
    quit.connect_clicked({
        let app = ctx.app.clone();
        move |_| app.quit()
    });
    close.add(&quit);
    page.add(&close);

    page
}

/// Writes the settings down and tells the overlay to read them again.
fn save(settings: &Settings, app: &adw::Application) {
    if let Err(error) = settings.save() {
        tracing::warn!(%error, "could not save the settings");
        return;
    }
    app.activate_action("reload", None);
}

/// Asks the compositor for a combination as soon as it is typed, so it works
/// without restarting anything.
///
/// A half-typed combination is refused, and that is the usual case here: every
/// keystroke in the box comes through. Nothing is said about it, because the
/// next keystroke may well finish it.
fn ask_for(combination: &str, flag: &str) {
    if let Err(error) = crate::desktop::bind(combination, flag) {
        tracing::debug!(%error, combination, flag, "the key was not taken");
    }
}

/// Which of the offered weights is closest to the one in the settings, which
/// older versions let anyone set to any hundred.
fn nearest_weight(weight: u32) -> u32 {
    WEIGHTS
        .iter()
        .enumerate()
        .min_by_key(|(_, (value, _))| value.abs_diff(weight))
        .map_or(3, |(index, _)| index as u32)
}

/// The family of the desktop's interface font, which is what an empty setting
/// leaves the overlay with.
fn desktop_font() -> String {
    gtk::Settings::default()
        .and_then(|settings| settings.gtk_font_name())
        .map(|name| pango::FontDescription::from_string(&name))
        .and_then(|desc| desc.family().map(|family| family.to_string()))
        .unwrap_or_else(|| "Sans".to_owned())
}

fn hex(rgba: &gdk::RGBA) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(rgba.red()),
        channel(rgba.green()),
        channel(rgba.blue())
    )
}

/// A few lines drawn with the overlay's own rules, over a scene with bright and
/// dark parts, so the effect of each change is seen without looking away.
struct Preview {
    root: gtk::Box,
    lines: gtk::Box,
    previous: gtk::Label,
    sung: gtk::Label,
    next: gtk::Label,
    provider: gtk::CssProvider,
}

/// The class the preview's rules are scoped to.
const PREVIEW: &str = "lyricslens-preview";

/// Shenandoah is traditional, so its words can be shown anywhere.
const SAMPLE: [&str; 3] = [
    "’Cross the wide Missouri",
    "Oh Shenandoah, I long to see you",
    "Away, you rolling river",
];

impl Preview {
    fn new() -> Self {
        let label = |text: &str| {
            gtk::Label::builder()
                .label(text)
                .wrap(true)
                .justify(gtk::Justification::Center)
                .build()
        };
        let previous = label(SAMPLE[0]);
        let sung = label(SAMPLE[1]);
        let next = label(SAMPLE[2]);
        previous.add_css_class("waiting");
        next.add_css_class("waiting");

        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .valign(gtk::Align::Center)
            .build();
        lines.add_css_class("lines");
        lines.append(&previous);
        lines.append(&sung);
        lines.append(&next);

        let scope = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .valign(gtk::Align::Center)
            .build();
        scope.add_css_class(PREVIEW);
        scope.append(&lines);

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .height_request(200)
            .overflow(gtk::Overflow::Hidden)
            .build();
        root.add_css_class("lyricslens-preview-scene");
        root.update_property(&[gtk::accessible::Property::Label(&t(
            "Preview of the lyrics with these settings",
        ))]);
        root.append(&scope);

        let provider = gtk::CssProvider::new();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        Self {
            root,
            lines,
            previous,
            sung,
            next,
            provider,
        }
    }

    fn refresh(&self, settings: &Settings) {
        let waiting = (settings.font_size * 8 / 10).max(8);
        let css = format!(
            "{rules}
             .lyricslens-preview-scene {{
                 border-radius: 12px;
                 padding: 28px 20px;
                 background: linear-gradient(120deg, #24313f 0%, #4f6a82 45%, #c9d3dc 75%, #f1e4c8 100%);
             }}
             .{PREVIEW} label.waiting {{ opacity: 0.5; font-size: {waiting}px; }}",
            rules = super::overlay::style(settings, &format!(".{PREVIEW}")),
        );
        self.provider.load_from_string(&css);

        let (align, justify) = match settings.alignment() {
            "start" => (gtk::Align::Start, gtk::Justification::Left),
            "end" => (gtk::Align::End, gtk::Justification::Right),
            _ => (gtk::Align::Center, gtk::Justification::Center),
        };
        self.lines.set_halign(align);
        for label in [&self.previous, &self.sung, &self.next] {
            label.set_justify(justify);
            label.set_halign(align);
        }
        self.previous.set_visible(settings.previous_line);
        self.next.set_visible(settings.upcoming_lines > 0);

        // Karaoke shows as the second half still waiting to be sung.
        let attributes = pango::AttrList::new();
        if settings.karaoke {
            let from = SAMPLE[1].find("I long").unwrap_or(0);
            let mut faded = pango::AttrInt::new_foreground_alpha(0x6000);
            faded.set_start_index(u32::try_from(from).unwrap_or(0));
            attributes.insert(faded);
        }
        self.sung.set_attributes(Some(&attributes));
    }

    fn detach(&self) {
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_remove_provider_for_display(&display, &self.provider);
        }
    }
}

/// The rescue for when the automatic match lands on the wrong recording.
///
/// Type a name, see everything the service has under it, pick one. The choice
/// is kept for the track playing now, so the same song comes back right.
fn wrong_lyrics(search: &Search) -> adw::PreferencesGroup {
    let group = group(
        "Wrong Lyrics",
        "Search by hand and pick the right recording",
    );

    let query = from_track(&search.playing.borrow());
    let artist = adw::EntryRow::builder()
        .title(t("Artist"))
        .text(query.artist.unwrap_or_default())
        .build();
    let title = adw::EntryRow::builder()
        .title(t("Title"))
        .text(query.title)
        .build();

    // What the automatic search made of the song, above the boxes for a
    // search by hand.
    let found_state = adw::ActionRow::builder()
        .title(t("Found Automatically"))
        .build();

    let results = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    results.add_css_class("boxed-list");
    // The service can answer with dozens of recordings; the list scrolls
    // instead of pushing everything else off the page.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(RESULTS_HEIGHT)
        .propagate_natural_height(true)
        .margin_top(12)
        .visible(false)
        .child(&results)
        .build();

    let status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .margin_top(8)
        .visible(false)
        .build();
    status.add_css_class("dim-label");

    let button = gtk::Button::builder()
        .label(t("Search"))
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("suggested-action");
    button.connect_clicked({
        let search = search.clone();
        let artist = artist.clone();
        let title = title.clone();
        let results = results.clone();
        let scroller = scroller.clone();
        let status = status.clone();
        move |button| {
            let title_text = title.text().trim().to_owned();
            if title_text.is_empty() {
                status.set_label(&t("Type at least a title"));
                status.set_visible(true);
                return;
            }

            button.set_sensitive(false);
            status.set_label(&t("Searching…"));
            status.set_visible(true);
            scroller.set_visible(false);

            let request = Request::Search {
                artist: artist.text().trim().to_owned(),
                title: title_text,
            };
            let search = search.clone();
            let results = results.clone();
            let scroller = scroller.clone();
            let status = status.clone();
            let button = button.clone();
            glib::spawn_future_local(async move {
                if search.requests.send(request).await.is_err() {
                    status.set_label(&t("The overlay is not running"));
                    button.set_sensitive(true);
                    return;
                }
                let Ok(found) = search.candidates.recv().await else {
                    button.set_sensitive(true);
                    return;
                };
                fill(&results, &scroller, &status, &search, found);
                button.set_sensitive(true);
            });
        }
    });
    group.set_header_suffix(Some(&button));

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    let entries = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    entries.add_css_class("boxed-list");
    entries.append(&found_state);
    entries.append(&artist);
    entries.append(&title);
    column.append(&entries);
    column.append(&status);
    column.append(&scroller);
    group.add(&column);

    // The window outlives the song. When the track changes under it, the boxes
    // hold the name of something that is no longer playing and the list below
    // answers a question nobody is asking any more.
    let mut showing = search.playing.borrow().clone();
    glib::timeout_add_local(Duration::from_millis(500), {
        let search = search.clone();
        move || {
            // The window was closed; nothing left to keep in step.
            if artist.root().is_none() {
                return glib::ControlFlow::Break;
            }

            let report = search.report.borrow().clone();
            found_state.set_subtitle(&report);
            found_state.set_visible(!report.is_empty());

            let playing = search.playing.borrow().clone();
            if playing == showing {
                return glib::ControlFlow::Continue;
            }
            showing = playing.clone();

            let query = from_track(&playing);
            artist.set_text(&query.artist.unwrap_or_default());
            title.set_text(&query.title);
            while let Some(row) = results.first_child() {
                results.remove(&row);
            }
            scroller.set_visible(false);
            status.set_visible(false);
            glib::ControlFlow::Continue
        }
    });

    group
}

/// Draws the answer, one row per recording.
fn fill(
    results: &gtk::ListBox,
    scroller: &gtk::ScrolledWindow,
    status: &gtk::Label,
    search: &Search,
    found: Vec<Candidate>,
) {
    while let Some(row) = results.first_child() {
        results.remove(&row);
    }

    if found.is_empty() {
        status.set_label(&t("Nothing found under that name"));
        status.set_visible(true);
        scroller.set_visible(false);
        return;
    }

    status.set_label(&count(
        found.len(),
        "{n} recording with synced lyrics",
        "{n} recordings with synced lyrics",
    ));
    status.set_visible(true);
    // Inside a preferences page a scrolled window asks for the height of one
    // row and no more, so it is told how much of the list to show.
    let rows = i32::try_from(found.len()).unwrap_or(i32::MAX);
    scroller.set_min_content_height(rows.saturating_mul(RESULT_ROW).min(RESULTS_HEIGHT));

    // The recording now on screen carries the check; the others lose theirs.
    let checks: Rc<RefCell<Vec<gtk::Image>>> = Rc::default();
    for candidate in found {
        let mut details = vec![candidate.artist.clone()];
        if !candidate.album.is_empty() {
            details.push(candidate.album.clone());
        }
        details.push(length(candidate.length));
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&candidate.title))
            .subtitle(glib::markup_escape_text(&details.join(", ")))
            .activatable(true)
            .build();
        let check = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .tooltip_text(t("Showing these lyrics now"))
            .visible(false)
            .build();
        row.add_suffix(&check);
        checks.borrow_mut().push(check.clone());
        row.connect_activated({
            let search = search.clone();
            let checks = checks.clone();
            move |_| {
                for other in checks.borrow().iter() {
                    other.set_visible(false);
                }
                check.set_visible(true);
                let search = search.clone();
                let candidate = candidate.clone();
                glib::spawn_future_local(async move {
                    let _ = search
                        .requests
                        .send(Request::Choose(Box::new(candidate)))
                        .await;
                });
            }
        });
        results.append(&row);
    }
    scroller.set_visible(true);
}

fn length(length: Option<Duration>) -> String {
    match length {
        Some(length) => {
            let seconds = length.as_secs();
            format!("{}:{:02}", seconds / 60, seconds % 60)
        }
        None => "--:--".to_owned(),
    }
}

/// The names of the screens attached right now.
fn connectors() -> Vec<String> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|index| monitors.item(index).and_downcast::<gdk::Monitor>())
        .filter_map(|monitor| monitor.connector().map(|name| name.to_string()))
        .collect()
}

/// True when the program was asked for the preferences window rather than the
/// overlay.
pub fn requested() -> bool {
    std::env::args().any(|argument| argument == "--settings")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_weight_lands_on_the_nearest_name() {
        assert_eq!(WEIGHTS[nearest_weight(600) as usize].0, 600);
        assert_eq!(WEIGHTS[nearest_weight(900) as usize].0, 800);
        assert_eq!(WEIGHTS[nearest_weight(100) as usize].0, 300);
        assert_eq!(WEIGHTS[nearest_weight(450) as usize].0, 400);
    }
}
