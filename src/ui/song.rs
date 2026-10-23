//! The whole song in a window.
//!
//! The overlay shows the line being sung and a couple around it. This is for
//! reading ahead: every line of what is playing, with the ones already sung
//! dimmed, scrolling on its own so the current one stays in view.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::AdwWindowExt;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::i18n::t;
use crate::lyrics::Lyrics;

/// How often the window looks at what is playing.
///
/// The overlay runs far faster because it animates; here nothing moves between
/// one line and the next.
const TICK: Duration = Duration::from_millis(200);

/// What is playing, kept by whoever follows the player.
///
/// Cloned into the window, which only reads it. `generation` counts the songs:
/// the list is rebuilt when it changes, and comparing whole songs on every
/// tick would be the alternative.
#[derive(Clone)]
pub struct Feed {
    pub lyrics: Rc<RefCell<Option<Lyrics>>>,
    pub current: Rc<Cell<Option<usize>>>,
    pub track: Rc<RefCell<String>>,
    pub generation: Rc<Cell<u64>>,
    /// A request to correct the timing, left here for whoever follows the
    /// player to pick up on its next tick.
    pub sync: Rc<Cell<Option<SyncTo>>>,
}

/// Which line is being sung at the moment of asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncTo {
    /// The one nearest to where the lyrics think the song is.
    Nearest,
    /// This one, by its place in the song.
    Line(usize),
}

const STYLE: &str = "window.lyricslens-song .line {
         padding: 6px 2px;
         font-size: 15px;
     }
     window.lyricslens-song .line.sung { opacity: 0.35; }
     window.lyricslens-song .line.now {
         font-weight: 700;
         font-size: 17px;
     }
     window.lyricslens-song .gap { opacity: 0.25; }
     window.lyricslens-song .line:hover { text-decoration: underline; }";

pub fn show(app: &adw::Application, feed: Feed) {
    let window = adw::Window::builder()
        .application(app)
        .title(t("Lyrics"))
        .default_width(460)
        .default_height(620)
        .build();
    window.add_css_class("lyricslens-song");

    if let Some(display) = gtk::gdk::Display::default() {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(STYLE);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let heading = gtk::Label::builder()
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    heading.add_css_class("heading");

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_start(18)
        .margin_end(18)
        .margin_top(8)
        .margin_bottom(24)
        .build();

    let empty = gtk::Label::builder()
        .label(t("Nothing playing with lyrics to show."))
        .wrap(true)
        .build();
    empty.add_css_class("dim-label");

    let hint = gtk::Label::builder()
        .label(t("Out of time? Click the line being sung right now"))
        .wrap(true)
        .margin_top(6)
        .margin_start(18)
        .margin_end(18)
        .xalign(0.0)
        .build();
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&column)
        .build();

    let header = adw::HeaderBar::builder().title_widget(&heading).build();
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    page.append(&header);
    page.append(&hint);
    page.append(&empty);
    page.append(&scrolled);
    window.set_content(Some(&page));

    let rows: Rc<RefCell<Vec<gtk::Label>>> = Rc::new(RefCell::new(Vec::new()));
    let shown: Rc<Cell<u64>> = Rc::new(Cell::new(u64::MAX));
    let lit: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

    let tick = glib::timeout_add_local(TICK, {
        let window = window.clone();
        move || {
            if !window.is_visible() {
                return glib::ControlFlow::Break;
            }

            if shown.get() != feed.generation.get() {
                shown.set(feed.generation.get());
                heading.set_text(&feed.track.borrow());
                lit.set(None);
                fill(&column, &rows, feed.lyrics.borrow().as_ref(), &feed.sync);
                let any = !rows.borrow().is_empty();
                scrolled.set_visible(any);
                hint.set_visible(any);
                empty.set_visible(!any);
            }

            let current = feed.current.get();
            if current != lit.get() {
                lit.set(current);
                paint(&rows.borrow(), current);
                if let Some(index) = current {
                    centre(&scrolled, &rows.borrow(), index);
                }
            }
            glib::ControlFlow::Continue
        }
    });

    // The tick holds a handle on everything the window shows; letting it run
    // after the window is gone keeps the whole song alive for nothing.
    let tick = RefCell::new(Some(tick));
    window.connect_close_request(move |_| {
        if let Some(tick) = tick.borrow_mut().take() {
            tick.remove();
        }
        glib::Propagation::Proceed
    });

    window.present();
}

/// Builds one row per line, instrumental gaps included, so a row's place in
/// the list is the line's place in the song.
///
/// A click on a line says it is the one being sung right now, and the timing
/// is corrected to match.
fn fill(
    column: &gtk::Box,
    rows: &Rc<RefCell<Vec<gtk::Label>>>,
    lyrics: Option<&Lyrics>,
    sync: &Rc<Cell<Option<SyncTo>>>,
) {
    while let Some(child) = column.first_child() {
        column.remove(&child);
    }
    rows.borrow_mut().clear();

    let Some(lyrics) = lyrics else { return };
    for (index, line) in lyrics.lines.iter().enumerate() {
        let label = gtk::Label::builder()
            .label(line.sung().unwrap_or("♪"))
            .wrap(true)
            .xalign(0.0)
            .build();
        label.add_css_class("line");
        if line.sung().is_none() {
            label.add_css_class("gap");
        } else {
            label.set_tooltip_text(Some(&t("This line is being sung now")));
            let click = gtk::GestureClick::new();
            click.connect_released({
                let sync = sync.clone();
                move |_, _, _, _| sync.set(Some(SyncTo::Line(index)))
            });
            label.add_controller(click);
        }
        column.append(&label);
        rows.borrow_mut().push(label);
    }
}

/// Dims what has been sung and marks what is being sung now.
fn paint(rows: &[gtk::Label], current: Option<usize>) {
    for (index, row) in rows.iter().enumerate() {
        let sung = current.is_some_and(|current| index < current);
        let now = current == Some(index);
        set_class(row, "sung", sung);
        set_class(row, "now", now);
    }
}

fn set_class(row: &gtk::Label, class: &str, on: bool) {
    if on {
        row.add_css_class(class);
    } else {
        row.remove_css_class(class);
    }
}

/// Scrolls so the line being sung sits in the middle, rather than at whichever
/// edge it happened to reach.
fn centre(scrolled: &gtk::ScrolledWindow, rows: &[gtk::Label], index: usize) {
    let Some(row) = rows.get(index) else { return };
    let adjustment = scrolled.vadjustment();
    let top = row
        .compute_point(scrolled, &gtk::graphene::Point::new(0.0, 0.0))
        .map_or(0.0, |point| f64::from(point.y()));
    let middle = adjustment.value() + top + f64::from(row.height()) / 2.0;
    let wanted = middle - adjustment.page_size() / 2.0;
    adjustment.set_value(wanted.clamp(0.0, (adjustment.upper() - adjustment.page_size()).max(0.0)));
}
