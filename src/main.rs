use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use lyricslens::app::{self, Update};
use lyricslens::i18n::t;
use lyricslens::lyrics::Lyrics;
use lyricslens::media::{Event, Track};
use lyricslens::store::settings::Settings;
use lyricslens::sync::Playback;
use lyricslens::sync::clock::Clock;
use lyricslens::ui::overlay::Overlay;
use lyricslens::ui::settings as preferences;
use lyricslens::ui::song;
use lyricslens::ui::tray;

/// How often the overlay asks the clock which line is being sung.
///
/// The screen redraws at its own rate; this only decides how late a line can
/// be, and a tenth of a second is below what anyone sees.
const TICK: Duration = Duration::from_millis(100);

const ID: &str = "io.github.julio0cesar.lyricslens";

/// How long the overlay is allowed to explain itself before going quiet.
///
/// A message about the search is worth a moment and no more: past that, an
/// overlay saying it is still looking is just words on the screen with no
/// song under them. The preferences window keeps the answer.
const GRACE: Duration = Duration::from_secs(4);

/// What the running overlay can be told to do from outside.
///
/// Wayland gives an ordinary client no way to grab a key combination, so the
/// hotkey belongs to the compositor. All this program offers is the command
/// for the compositor to run.
const COMMANDS: [(&str, &str); 5] = [
    ("--toggle", "toggle"),
    ("--song", "song"),
    ("--position", "position"),
    ("--settings", "settings"),
    ("--quit", "quit"),
];

fn main() -> glib::ExitCode {
    // The copy that holds the terminal is the one that prints to it; the one
    // that was let go of writes to a file nobody has to be watching.
    lyricslens::log::start(lyricslens::cli::holds_terminal());

    if let Some(code) = lyricslens::cli::handle() {
        return glib::ExitCode::from(code);
    }

    // Anything the running overlay can do, it should do itself rather than a
    // second copy doing it beside it.
    if let Some(action) = asked_command()
        && let Some(code) = send(action)
    {
        return code;
    }

    let application = adw::Application::builder().application_id(ID).build();

    if preferences::requested() {
        // Nothing is running, so the preferences window is the whole program.
        application.connect_activate(preferences::open);
        // GTK would try to make sense of our own flags otherwise.
        return application.run_with_args::<&str>(&[]);
    }

    // Launching again while a copy runs is a request to see it, not to start
    // another. Saying so is the difference between "it did nothing" and "it is
    // already there".
    if running_elsewhere() {
        println!("LyricsLens is already running; bringing the overlay to the front.");
        return send("present").unwrap_or(glib::ExitCode::SUCCESS);
    }

    // The overlay is meant to sit there all day, so the terminal comes back
    // straight away. `--foreground` is for watching the log.
    if !lyricslens::cli::wants_foreground() {
        return glib::ExitCode::from(lyricslens::cli::detach());
    }

    let settings = Settings::load();
    let (updates, requests) = app::start(settings.clone());

    // GTK calls this again every time the program is launched while one copy
    // is already running. Building a second overlay there would stack another
    // surface on the screen, so the running one is raised instead.
    let running: Rc<RefCell<Option<Rc<Overlay>>>> = Rc::new(RefCell::new(None));

    application.connect_activate(move |application| {
        if let Some(overlay) = running.borrow().as_ref() {
            overlay.present();
            return;
        }

        let overlay = match Overlay::build(application.upcast_ref(), &settings) {
            Ok(overlay) => overlay,
            Err(error) => {
                tracing::error!(%error, "could not open the overlay");
                return;
            }
        };

        let overlay = Rc::new(overlay);
        *running.borrow_mut() = Some(Rc::clone(&overlay));

        // What the window that lists the whole song reads. Kept here because
        // this is where the player is followed; the window only looks.
        let song = song::Feed {
            lyrics: Rc::new(RefCell::new(None)),
            current: Rc::new(std::cell::Cell::new(None)),
            track: Rc::new(RefCell::new(String::new())),
            generation: Rc::new(std::cell::Cell::new(0)),
        };

        let report = Rc::new(RefCell::new(String::new()));
        let newer = Rc::new(RefCell::new(None));
        let state = Rc::new(RefCell::new(State::new(
            settings.clone(),
            Rc::clone(&report),
            Rc::clone(&newer),
            song.clone(),
        )));

        // What the preferences window needs to search for the track playing
        // now, and where the answers land while it is open.
        let playing: Rc<RefCell<Track>> = Rc::new(RefCell::new(Track::default()));
        let (found, candidates) = async_channel::bounded(4);
        let (open_players, players) = async_channel::bounded(4);
        let search = preferences::Search {
            requests: requests.clone(),
            candidates,
            players,
            playing: Rc::clone(&playing),
            report: Rc::clone(&report),
            newer: Rc::clone(&newer),
        };

        add_commands(application, &overlay, &search, &song);
        ask_for_the_keys(&settings);

        // The icon in the status bar is the only handle a program with no
        // window of its own gives the person running it.
        let commands = tray::start();
        glib::spawn_future_local({
            let application = application.clone();
            let overlay = Rc::clone(&overlay);
            let search = search.clone();
            let song = song.clone();
            async move {
                while let Ok(command) = commands.recv().await {
                    match command {
                        tray::Command::Toggle => overlay.toggle(),
                        tray::Command::Song => song::show(&application, song.clone()),
                        tray::Command::Position => overlay.toggle_positioning(),
                        tray::Command::Settings => {
                            preferences::show(&application, Some(search.clone()));
                        }
                        tray::Command::Quit => application.quit(),
                    }
                }
            }
        });

        // The preferences window writes the file and asks for this: both the
        // window and what decides its contents have to read it again.
        let reload = gio::SimpleAction::new("reload", None);
        reload.connect_activate({
            let overlay = Rc::clone(&overlay);
            let state = Rc::clone(&state);
            move |_, _| {
                let settings = Settings::load();
                overlay.reload(&settings);
                let mut state = state.borrow_mut();
                state.settings = settings;
                // The window may have just changed a correction, and the clock
                // is the only thing that acts on one.
                state.apply_offset();
            }
        });
        application.add_action(&reload);

        let updates = updates.clone();
        let reader = Rc::clone(&state);
        let seen = Rc::clone(&playing);
        glib::spawn_future_local(async move {
            while let Ok(update) = updates.recv().await {
                match update {
                    // The window that asked for these is the only one that
                    // wants them; the state has no use for a search.
                    app::Update::Candidates(results) => {
                        let _ = found.send(results).await;
                    }
                    app::Update::Players(open) => {
                        let _ = open_players.send(open).await;
                    }
                    update => {
                        if let app::Update::Media(Event::TrackChanged(track)) = &update {
                            *seen.borrow_mut() = track.clone();
                        }
                        reader.borrow_mut().apply(update);
                    }
                }
            }
        });

        glib::timeout_add_local(TICK, move || {
            let state = state.borrow();
            overlay.show(
                state.before().as_deref(),
                state.line().as_deref(),
                &state.upcoming(),
            );
            overlay.show_progress(state.progress());
            let (name, elapsed) = state.now_playing();
            overlay.show_track(name.as_deref(), elapsed);
            overlay.show_art(state.art());
            state.song.current.set(state.line_index());
            glib::ControlFlow::Continue
        });
    });

    application.run_with_args::<&str>(&[])
}

/// Asks the compositor for the key combinations the settings name.
///
/// Every failure is survivable: the overlay runs, and the tray icon and the
/// preferences window are still there to reach it by.
fn ask_for_the_keys(settings: &Settings) {
    for (combination, flag) in [
        (&settings.hotkey_toggle, "--toggle"),
        (&settings.hotkey_position, "--position"),
    ] {
        if let Err(error) = lyricslens::desktop::bind(combination, flag) {
            tracing::warn!(%error, combination, flag, "the key was not taken");
        }
    }
}

/// The action a command-line flag asks the running overlay for.
fn asked_command() -> Option<&'static str> {
    let arguments: Vec<String> = std::env::args().collect();
    COMMANDS
        .iter()
        .find(|(flag, _)| arguments.iter().any(|argument| argument == flag))
        .map(|(_, action)| *action)
}

/// Hands the action to the instance already running.
///
/// `None` means there is none, and the caller decides what to do about it: for
/// the preferences window that is to open one, for hiding the overlay there is
/// nothing to hide.
fn send(action: &str) -> Option<glib::ExitCode> {
    let remote = gio::Application::new(Some(ID), gio::ApplicationFlags::empty());
    if let Err(error) = remote.register(gio::Cancellable::NONE) {
        eprintln!("lyricslens: could not reach the session bus: {error}");
        return Some(glib::ExitCode::FAILURE);
    }
    if !remote.is_remote() {
        return None;
    }
    remote.activate_action(action, None);
    Some(glib::ExitCode::SUCCESS)
}

/// Whether another copy already holds the application's name on the bus.
fn running_elsewhere() -> bool {
    let probe = gio::Application::new(Some(ID), gio::ApplicationFlags::empty());
    probe.register(gio::Cancellable::NONE).is_ok() && probe.is_remote()
}

fn add_commands(
    application: &adw::Application,
    overlay: &Rc<Overlay>,
    search: &preferences::Search,
    song: &song::Feed,
) {
    let present = gio::SimpleAction::new("present", None);
    present.connect_activate({
        let overlay = Rc::clone(overlay);
        move |_, _| overlay.present()
    });

    let settings = gio::SimpleAction::new("settings", None);
    settings.connect_activate({
        let application = application.clone();
        let search = search.clone();
        move |_, _| preferences::show(&application, Some(search.clone()))
    });

    let whole_song = gio::SimpleAction::new("song", None);
    whole_song.connect_activate({
        let application = application.clone();
        let song = song.clone();
        move |_, _| song::show(&application, song.clone())
    });

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let application = application.clone();
        move |_, _| application.quit()
    });

    let toggle = gio::SimpleAction::new("toggle", None);
    toggle.connect_activate({
        let overlay = Rc::clone(overlay);
        move |_, _| overlay.toggle()
    });

    let position = gio::SimpleAction::new("position", None);
    position.connect_activate({
        let overlay = Rc::clone(overlay);
        move |_, _| overlay.toggle_positioning()
    });

    application.add_action(&quit);
    application.add_action(&present);
    application.add_action(&settings);
    application.add_action(&toggle);
    application.add_action(&position);
    application.add_action(&whole_song);
}

/// What the overlay is showing, and everything it takes to decide that.
struct State {
    settings: Settings,
    track: Track,
    lyrics: Option<Lyrics>,
    clock: Clock,
    /// When the track changed, which is when a message about it stops being
    /// news.
    since: Instant,
    /// What the preferences window says about the lyrics for this track.
    report: Rc<RefCell<String>>,
    /// A release newer than this one, once it is known.
    newer: Rc<RefCell<Option<String>>>,
    playing: bool,
    stalled: bool,
    /// True between a track change and the answer about its lyrics, so the
    /// overlay can tell "still looking" from "there are none".
    searching: bool,
    /// The cover for what is playing, once it has been found.
    art: Option<std::path::PathBuf>,
    /// What the window that lists the whole song reads.
    song: song::Feed,
    /// The player being followed, by bus name. Kept because the correction to
    /// apply is worked out again on every track change, not only when the
    /// player is picked.
    player: String,
}

impl State {
    fn new(
        settings: Settings,
        report: Rc<RefCell<String>>,
        newer: Rc<RefCell<Option<String>>>,
        song: song::Feed,
    ) -> Self {
        Self {
            settings,
            since: Instant::now(),
            report,
            newer,
            track: Track::default(),
            lyrics: None,
            clock: Clock::new(0),
            playing: false,
            stalled: false,
            searching: false,
            art: None,
            song,
            player: String::new(),
        }
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Lyrics(lyrics) => {
                self.lyrics = *lyrics;
                self.searching = false;
                self.publish_song();
                *self.report.borrow_mut() = match &self.lyrics {
                    Some(lyrics) => lyricslens::i18n::count(
                        lyrics.lines.len(),
                        "{n} line being followed",
                        "{n} lines being followed",
                    ),
                    None => t("No synced lyrics found for this one"),
                };
            }
            Update::Player(name) => {
                self.player = name;
                self.apply_offset();
            }
            Update::Media(Event::TrackChanged(track)) => {
                // An empty track means no player at all, so there is nothing
                // to look up and nothing to wait for.
                if track.is_empty() {
                    self.playing = false;
                }
                self.searching = !track.is_empty();
                self.since = Instant::now();
                *self.report.borrow_mut() = if self.searching {
                    t("Looking…")
                } else {
                    String::new()
                };
                self.track = track;
                self.lyrics = None;
                self.art = None;
                self.publish_song();
                self.apply_offset();
                self.stalled = false;
                self.clock.reset();
            }
            Update::Media(Event::Playback(state)) => {
                self.playing = state == Playback::Playing;
                self.stalled = false;
                self.clock.playback(state, Instant::now());
            }
            Update::Media(Event::Position { reading, at }) => {
                self.clock.sample(reading, at);
            }
            Update::Media(Event::PositionStalled) => self.stalled = true,
            // Answered straight to the window that asked; nothing here wants
            // a list of recordings.
            Update::Candidates(_) | Update::Players(_) => {}
            Update::Art(path) => self.art = path,
            Update::NewVersion(version) => {
                *self.newer.borrow_mut() = Some(version);
            }
        }
    }

    /// What is playing and how far into it, for the two things the overlay
    /// can show around the lyrics.
    fn now_playing(&self) -> (Option<String>, Option<f64>) {
        if self.track.is_empty() {
            return (None, None);
        }

        let name = self.settings.show_track.then(|| {
            let title = self.title();
            if self.track.artists.is_empty() {
                title
            } else {
                format!("{}  —  {title}", self.track.artists.join(", "))
            }
        });

        // Needs a length to be a fraction of, which a browser often does not
        // give.
        let elapsed = self
            .settings
            .show_progress
            .then(|| self.elapsed())
            .flatten();

        (name, elapsed)
    }

    /// Names the recording playing now, for the correction kept against it.
    fn track_key(&self) -> Option<String> {
        if self.track.is_empty() {
            return None;
        }
        Some(Settings::track_key(
            &self.track.artists.join(", "),
            &self.title(),
            self.track.length,
        ))
    }

    /// Points the clock at whichever correction applies: this recording's, or
    /// the player's.
    fn apply_offset(&mut self) {
        let key = self.track_key();
        let offset_ms = self.settings.offset_for(&self.player, key.as_deref());
        tracing::debug!(
            offset_ms,
            player = self.player,
            track = key.as_deref().unwrap_or("-"),
            "correction in force"
        );
        self.clock.set_offset_ms(offset_ms);
    }

    /// Hands the whole song to the window that lists it, if it is open.
    ///
    /// Counted rather than compared: the window rebuilds its list when the
    /// count moves, and comparing two whole songs on every tick is the
    /// alternative.
    fn publish_song(&self) {
        *self.song.lyrics.borrow_mut() = self.lyrics.clone();
        *self.song.track.borrow_mut() = if self.track.is_empty() {
            String::new()
        } else if self.track.artists.is_empty() {
            self.title()
        } else {
            format!("{}  —  {}", self.track.artists.join(", "), self.title())
        };
        self.song
            .generation
            .set(self.song.generation.get().wrapping_add(1));
    }

    /// Where in the song the clock is, as a place in the list of lines.
    fn line_index(&self) -> Option<usize> {
        let position = self.clock.position(Instant::now())?;
        self.lyrics.as_ref()?.index_at(position)
    }

    /// The cover to draw beside the lyrics, when there is one and it is wanted.
    fn art(&self) -> Option<&std::path::Path> {
        if self.track.is_empty() || !self.settings.show_art {
            return None;
        }
        self.art.as_deref()
    }

    fn elapsed(&self) -> Option<f64> {
        let position = self.clock.position(Instant::now())?;
        let length = self.track.length?.as_secs_f64();
        (length > 0.0).then(|| position.as_secs_f64() / length)
    }

    /// How far through the current line the song is.
    fn progress(&self) -> Option<f64> {
        let position = self.clock.position(Instant::now())?;
        self.lyrics.as_ref()?.progress_at(position)
    }

    /// The line just sung, when the settings ask for it.
    fn before(&self) -> Option<String> {
        if !self.settings.previous_line {
            return None;
        }
        let position = self.clock.position(Instant::now())?;
        self.lyrics.as_ref()?.before(position).map(str::to_owned)
    }

    /// The lines still to come.
    ///
    /// One more than the settings show: the overlay needs the line after the
    /// last visible one ready to climb into its place.
    fn upcoming(&self) -> Vec<String> {
        // Whatever hides the line being sung hides what comes after it too:
        // in an instrumental the next line is all that is on screen.
        if self.track.is_empty() || (self.settings.hide_when_paused && !self.playing) {
            return Vec::new();
        }
        let wanted = usize::from(self.settings.upcoming_lines) + 1;
        let (Some(lyrics), Some(position)) =
            (self.lyrics.as_ref(), self.clock.position(Instant::now()))
        else {
            return Vec::new();
        };
        lyrics.after(position, wanted)
    }

    /// The words on screen right now.
    /// The words on screen right now.
    ///
    /// When there are no lyrics to follow, the overlay says why instead of
    /// staying blank — a blank overlay and a broken one look identical.
    fn line(&self) -> Option<String> {
        if self.track.is_empty() {
            return None;
        }
        if self.settings.hide_when_paused && !self.playing {
            return None;
        }

        let position = self.clock.position(Instant::now());
        if let (Some(lyrics), Some(position)) = (self.lyrics.as_ref(), position) {
            // None here is an instrumental gap, which is meant to be silent.
            return lyrics
                .line_at(position)
                .and_then(|line| line.sung())
                .map(str::to_owned);
        }

        // A player that reports no position is a standing fact, not news: it
        // will not change while this song plays, and nothing will ever appear.
        if self.stalled {
            let title = self.title();
            let reason = t("this player does not report its position");
            return Some(format!("{title}  ·  {reason}"));
        }

        // Everything else is news, and news goes quiet. The preferences window
        // is where the answer lives from then on.
        if self.since.elapsed() >= GRACE {
            return None;
        }

        let title = self.title();
        let reason = if self.searching {
            t("looking for the lyrics…")
        } else if self.lyrics.is_none() {
            t("no synced lyrics for this one")
        } else {
            t("waiting for the player")
        };
        Some(format!("{title}  ·  {reason}"))
    }

    fn title(&self) -> String {
        self.track
            .title
            .clone()
            .unwrap_or_else(|| t("unknown track"))
    }
}
