//! The worker thread.
//!
//! zbus and reqwest run on tokio; GTK has its own main loop, and neither can
//! drive the other. So tokio lives on a thread of its own, follows the player,
//! looks the lyrics up, and the two sides meet on a channel both know how to
//! await.

use std::time::Duration;

use tokio::task::JoinHandle;
use zbus::Connection;

use crate::art::Art;
use crate::error::Error;
use crate::lyrics::lrclib::{Candidate, Client};
use crate::lyrics::normalize::{Query, from_track};
use crate::lyrics::{Lyrics, lrc};
use crate::media::{Event, Track, mpris};
use crate::store::{cache, settings::Settings};

/// How long to wait before looking for a player again.
const RETRY: Duration = Duration::from_secs(2);

/// Names the player to follow, over whatever the settings say.
const OVERRIDE: &str = "LYRICSLENS_PLAYER";

/// What the interface asks the worker to do.
///
/// Only the worker has a runtime to reach the network with, so a search
/// started from the preferences window is a message rather than a call.
#[derive(Debug, Clone)]
pub enum Request {
    /// Every recording under this name, for choosing by hand.
    Search { artist: String, title: String },
    /// Use these lyrics for the track playing now, and remember the choice.
    Choose(Box<Candidate>),
    /// The players open right now, to choose from.
    Players,
    /// Follow this player from now on, by the short name in its bus name, or
    /// whichever is playing when there is none.
    Follow(Option<String>),
}

/// Everything the interface needs to know.
#[derive(Debug, Clone)]
pub enum Update {
    Media(Event),
    /// Which player is being followed, by bus name. The manual offset is kept
    /// per player, so the interface needs to know whose it is.
    Player(String),
    /// The lyrics for the track playing now, or nothing found.
    ///
    /// Boxed because it dwarfs every other variant, and a whole song would
    /// otherwise set the size of the channel's every message.
    Lyrics(Box<Option<Lyrics>>),
    /// The cover for the track playing now, as a file on disk.
    Art(Option<std::path::PathBuf>),
    /// The answer to a search, in the order the service returned it.
    Candidates(Vec<Candidate>),
    /// A release newer than this build exists. Nothing is installed by it.
    NewVersion(String),
    /// The answer to [`Request::Players`].
    Players(Vec<mpris::Choice>),
}

/// Starts the worker and hands back the receiving end.
///
/// The channel is bounded: if the interface ever stops reading, the worker
/// waits instead of growing a queue of stale updates.
pub fn start(
    settings: Settings,
) -> (
    async_channel::Receiver<Update>,
    async_channel::Sender<Request>,
) {
    let (sender, receiver) = async_channel::bounded(64);
    let (requests, incoming) = async_channel::bounded(8);

    std::thread::Builder::new()
        .name("player".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(%error, "could not start the worker runtime");
                    return;
                }
            };
            runtime.block_on(run(sender, incoming, settings));
        })
        .expect("spawning a thread");

    (receiver, requests)
}

/// Follows whatever is playing, and keeps looking when nothing is.
async fn run(
    updates: async_channel::Sender<Update>,
    requests: async_channel::Receiver<Request>,
    settings: Settings,
) {
    if let Ok(client) = Client::new() {
        let updates = updates.clone();
        tokio::spawn(async move {
            if let Some(version) = client.check_for_a_newer_release().await {
                tracing::info!(version, "a newer release is out");
                let _ = updates.send(Update::NewVersion(version)).await;
            }
        });
    }

    // The variable wins over the file: it is how a single run is pointed at a
    // different player without editing anything.
    let mut wanted = std::env::var(OVERRIDE)
        .ok()
        .filter(|wanted| !wanted.is_empty())
        .or(settings.player);

    loop {
        if updates.is_closed() {
            return;
        }
        match once(&updates, &requests, wanted.as_deref()).await {
            // Another player was chosen: follow it now rather than after the
            // pause, which is for a bus with nothing on it.
            Ok(Some(chosen)) => {
                wanted = chosen;
                continue;
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "lost the player"),
        }

        // Nothing to follow yet, but the preferences window may still ask
        // which players there are, or pick one.
        let pause = tokio::time::sleep(RETRY);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                () = &mut pause => break,
                request = requests.recv() => match request {
                    Ok(Request::Follow(chosen)) => {
                        wanted = chosen;
                        break;
                    }
                    Ok(request) => {
                        if let Ok(client) = Client::new() {
                            answer(&client, request, None, &updates).await;
                        }
                    }
                    Err(_) => return,
                },
            }
        }
    }
}

/// Follows one player until it leaves, or until another is chosen, which is
/// what comes back: `Some` with the new choice.
async fn once(
    updates: &async_channel::Sender<Update>,
    requests: &async_channel::Receiver<Request>,
    wanted: Option<&str>,
) -> Result<Option<Option<String>>, Error> {
    let connection = Connection::session().await?;
    let Some(name) = mpris::pick(&connection, wanted).await? else {
        tracing::debug!("no MPRIS player on the bus");
        // Whatever was on screen belongs to a player that is gone.
        let _ = updates
            .send(Update::Media(Event::TrackChanged(Track::default())))
            .await;
        return Ok(None);
    };
    if updates
        .send(Update::Player(name.as_str().to_owned()))
        .await
        .is_err()
    {
        return Ok(None);
    }

    let (events, incoming) = async_channel::bounded(64);
    let follower = {
        let connection = connection.clone();
        let name = name.clone();
        tokio::spawn(async move { mpris::follow(&connection, &name, events).await })
    };

    let client = Client::new()?;
    let art = Art::new().ok();
    let mut lookup: Option<JoinHandle<()>> = None;
    let mut cover: Option<JoinHandle<()>> = None;
    let mut playing: Option<Track> = None;
    let mut chosen = None;

    loop {
        let event = tokio::select! {
            event = incoming.recv() => match event {
                Ok(event) => event,
                Err(_) => break,
            },
            request = requests.recv() => {
                match request {
                    Ok(Request::Follow(other)) => {
                        chosen = Some(other);
                        break;
                    }
                    Ok(request) => {
                        answer(&client, request, playing.clone(), updates).await;
                        continue;
                    }
                    Err(_) => break,
                }
            }
        };

        if let Event::TrackChanged(track) = &event {
            playing = Some(track.clone());
            // A lookup for the previous song is worthless now, and its answer
            // arriving late would put the wrong lyrics on screen.
            if let Some(lookup) = lookup.take() {
                lookup.abort();
            }
            lookup = Some(tokio::spawn(fetch(
                client.clone(),
                track.clone(),
                updates.clone(),
            )));

            if let Some(cover) = cover.take() {
                cover.abort();
            }
            cover = art
                .clone()
                .map(|art| tokio::spawn(find_art(art, track.clone(), updates.clone())));
        }

        if updates.send(Update::Media(event)).await.is_err() {
            break;
        }
    }

    follower.abort();
    if let Some(lookup) = lookup {
        lookup.abort();
    }
    if let Some(cover) = cover {
        cover.abort();
    }
    // The player left, or stopped talking. Either way there is nothing to sing
    // along to, and the last line must not sit there as if there were.
    let _ = updates
        .send(Update::Media(Event::TrackChanged(Track::default())))
        .await;
    Ok(chosen)
}

/// Handles what the preferences window asked for.
async fn answer(
    client: &Client,
    request: Request,
    playing: Option<Track>,
    updates: &async_channel::Sender<Update>,
) {
    match request {
        Request::Search { artist, title } => {
            let found = match client.search(&artist, &title).await {
                Ok(found) => found,
                Err(error) => {
                    tracing::warn!(%error, "the search failed");
                    Vec::new()
                }
            };
            tracing::info!(results = found.len(), artist, title, "searched by hand");
            let _ = updates.send(Update::Candidates(found)).await;
        }
        Request::Choose(chosen) => {
            let lyrics = lrc::parse(&chosen.lrc);
            // Cached under what is playing, not under what was searched for:
            // the point is that the same track comes back right next time.
            if let Some(track) = playing {
                let query = from_track(&track);
                cache::put(
                    query.artist.as_deref().unwrap_or_default(),
                    &query.title,
                    track.length,
                    &chosen.lrc,
                );
            }
            tracing::info!(lines = lyrics.lines.len(), "lyrics chosen by hand");
            let _ = updates.send(Update::Lyrics(Box::new(Some(lyrics)))).await;
        }
        Request::Players => {
            let found = match Connection::session().await {
                Ok(connection) => mpris::choices(&connection).await.unwrap_or_else(|error| {
                    tracing::warn!(%error, "could not list the players");
                    Vec::new()
                }),
                Err(error) => {
                    tracing::warn!(%error, "no session bus to list the players on");
                    Vec::new()
                }
            };
            let _ = updates.send(Update::Players(found)).await;
        }
        // Taken by the loops that follow a player, which are the ones that
        // can act on it.
        Request::Follow(_) => {}
    }
}

async fn fetch(client: Client, track: Track, updates: async_channel::Sender<Update>) {
    let query = from_track(&track);
    if query.title.is_empty() {
        let _ = updates.send(Update::Lyrics(Box::new(None))).await;
        return;
    }

    // Before anything else: a local library often already has the words on
    // disk, written by whoever tagged the collection, and that file is the one
    // its owner chose.
    if let Some(lyrics) = beside_the_track(&track) {
        tracing::info!(
            lines = lyrics.lines.len(),
            ?query,
            "lyrics from a file beside the track"
        );
        let _ = updates.send(Update::Lyrics(Box::new(Some(lyrics)))).await;
        return;
    }

    let artist = query.artist.as_deref().unwrap_or_default();
    if let Some(lyrics) = cache::get(artist, &query.title, track.length) {
        tracing::info!(lines = lyrics.lines.len(), ?query, "lyrics from the cache");
        let _ = updates.send(Update::Lyrics(Box::new(Some(lyrics)))).await;
        return;
    }

    let found = match client.lyrics(&query, track.length).await {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, ?query, "could not fetch lyrics");
            let _ = updates.send(Update::Lyrics(Box::new(None))).await;
            return;
        }
    };

    let lyrics = found.map(|found| {
        cache::put(artist, &query.title, track.length, &found.lrc);
        found.lyrics
    });
    report(&query, lyrics.as_ref());
    let _ = updates.send(Update::Lyrics(Box::new(lyrics))).await;
}

/// Looks the cover up on its own, so a slow picture never holds the words up.
async fn find_art(art: Art, track: Track, updates: async_channel::Sender<Update>) {
    let found = art.find(&track).await;
    match &found {
        Some(path) => tracing::debug!(path = %path.display(), "cover found"),
        None => tracing::debug!("no cover for this one"),
    }
    let _ = updates.send(Update::Art(found)).await;
}

/// The `.lrc` next to the audio file, when the track is a file on this machine
/// and that file holds synced lines.
///
/// Only the sibling with the same name: a `lyrics` folder next to the album is
/// the other convention in the wild, and reading one of the two is a
/// reasonable place to stop.
fn beside_the_track(track: &Track) -> Option<Lyrics> {
    let path = track.local_file()?.with_extension("lrc");
    let text = std::fs::read_to_string(&path).ok()?;
    let lyrics = lrc::parse(&text);
    if lyrics.is_empty() {
        tracing::debug!(path = %path.display(), "the file beside the track has no synced lines");
        return None;
    }
    Some(lyrics)
}

fn report(query: &Query, lyrics: Option<&Lyrics>) {
    match lyrics {
        Some(lyrics) => tracing::info!(lines = lyrics.lines.len(), ?query, "lyrics found"),
        None => tracing::info!(?query, "no synced lyrics for this one"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own per test: they run at the same time, and a file
    /// left behind would decide another one's answer.
    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lyricslens-beside-{name}"));
        std::fs::create_dir_all(&dir).expect("a temp folder");
        dir
    }

    fn playing(audio: &std::path::Path) -> Track {
        Track {
            title: Some("Airbag".to_owned()),
            url: Some(format!("file://{}", audio.display())),
            ..Track::default()
        }
    }

    #[test]
    fn the_lrc_next_to_the_audio_is_read() {
        let dir = folder("read");
        let audio = dir.join("airbag.flac");
        std::fs::write(&audio, b"not really audio").expect("the audio file");
        std::fs::write(dir.join("airbag.lrc"), "[00:01.00]In the next world war")
            .expect("the lyrics file");

        let lyrics = beside_the_track(&playing(&audio)).expect("lines from the file");
        assert_eq!(lyrics.lines[0].sung(), Some("In the next world war"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_with_no_timestamps_is_not_used() {
        let dir = folder("plain");
        let audio = dir.join("airbag.flac");
        std::fs::write(&audio, b"not really audio").expect("the audio file");
        std::fs::write(dir.join("airbag.lrc"), "In the next world war").expect("the lyrics file");

        assert!(beside_the_track(&playing(&audio)).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_track_with_no_file_beside_it_falls_through() {
        let dir = folder("alone");
        let audio = dir.join("airbag.flac");
        std::fs::write(&audio, b"not really audio").expect("the audio file");

        assert!(beside_the_track(&playing(&audio)).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_track_that_is_not_a_file_falls_through() {
        assert!(beside_the_track(&Track::default()).is_none());
    }
}
