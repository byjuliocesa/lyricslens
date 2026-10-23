//! Everything that talks to a player over D-Bus: finding one, and following
//! what it plays.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use zbus::Connection;
use zbus::fdo::DBusProxy;
use zbus::fdo::PropertiesProxy;
use zbus::names::{InterfaceName, OwnedBusName};
use zbus::proxy;
use zbus::zvariant::OwnedValue;

use super::{Event, Track};
use crate::error::Error;
use crate::sync::Playback;

/// Every MPRIS player owns a bus name starting with this.
const PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Only the parts of the player interface this milestone needs.
#[proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
pub trait Player {
    #[zbus(property)]
    fn metadata(
        &self,
    ) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;

    /// Microseconds into the track. The specification gives this one no change
    /// signal, so it must not be cached and has to be read on a timer.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> zbus::Result<i64>;
}

/// The part of the root interface that names the player for people.
#[proxy(
    interface = "org.mpris.MediaPlayer2",
    default_path = "/org/mpris/MediaPlayer2"
)]
pub trait Root {
    #[zbus(property)]
    fn identity(&self) -> zbus::Result<String>;
}

/// A player that can be chosen to follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// What is stored in the settings and matched against the bus name: the
    /// short name, which a browser keeps across restarts while its instance
    /// number changes.
    pub key: String,
    /// What the player calls itself, such as `Spotify`.
    pub name: String,
    pub playing: bool,
}

/// Every player on the bus right now, one per application.
pub async fn choices(connection: &Connection) -> Result<Vec<Choice>, Error> {
    let mut found: Vec<Choice> = Vec::new();
    for bus in list(connection).await? {
        let key = short_name(&bus).to_owned();
        let name = match RootProxy::builder(connection)
            .destination(bus.clone())?
            .build()
            .await
        {
            Ok(root) => root.identity().await.unwrap_or_else(|_| key.clone()),
            Err(_) => key.clone(),
        };
        let playing = match PlayerProxy::builder(connection)
            .destination(bus.clone())?
            .build()
            .await
        {
            Ok(player) => player.playback_status().await.as_deref() == Ok("Playing"),
            Err(_) => false,
        };
        // Two windows of the same browser are one choice, since the setting
        // cannot tell them apart.
        match found.iter_mut().find(|choice| choice.key == key) {
            Some(same) => same.playing |= playing,
            None => found.push(Choice { key, name, playing }),
        }
    }
    Ok(found)
}

/// Bus names of every running MPRIS player, in the order the bus returns them.
pub async fn list(connection: &Connection) -> Result<Vec<OwnedBusName>, Error> {
    let bus = DBusProxy::new(connection).await?;
    let mut players: Vec<OwnedBusName> = bus
        .list_names()
        .await?
        .into_iter()
        .filter(|name| name.starts_with(PREFIX))
        .collect();
    // The bus order is arbitrary; sorting makes the pick reproducible when
    // nothing is playing.
    players.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(players)
}

/// Picks the player to follow.
///
/// `wanted` matches any part of a bus name, for when a browser and a music
/// player are both open. Without it, the one that is actually playing wins,
/// else the first.
pub async fn pick(
    connection: &Connection,
    wanted: Option<&str>,
) -> Result<Option<OwnedBusName>, Error> {
    let players = list(connection).await?;

    if let Some(wanted) = wanted.filter(|wanted| !wanted.is_empty()) {
        let picked = players
            .into_iter()
            .find(|name| name.as_str().contains(wanted));
        if picked.is_none() {
            tracing::warn!(wanted, "no player matches the one asked for");
        }
        return Ok(picked);
    }

    for name in &players {
        let proxy = PlayerProxy::builder(connection)
            .destination(name.clone())?
            .build()
            .await?;
        if proxy.playback_status().await.as_deref() == Ok("Playing") {
            return Ok(Some(name.clone()));
        }
    }
    Ok(players.into_iter().next())
}

/// A name like `org.mpris.MediaPlayer2.firefox.instance_1_1` shortened to `firefox`.
pub fn short_name(name: &OwnedBusName) -> &str {
    name.as_str()
        .strip_prefix(PREFIX)
        .unwrap_or(name.as_str())
        .split('.')
        .next()
        .unwrap_or(name.as_str())
}

const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

/// How often `Position` is read while the song plays.
///
/// The flip of the integer second is what anchors the clock, and it can only be
/// noticed one poll late — so this interval is the floor on how far the overlay
/// can be off. Twenty reads a second is what it costs.
const POLL: Duration = Duration::from_millis(50);

/// How long an unchanging `Position` is given before the player is taken at its
/// word: it does not report one.
///
/// Firefox is the case that matters. It answers `Position` with zero forever
/// while `CanSeek` says true, and without that reading there is nothing to
/// synchronise against — better to say so than to let the overlay guess.
const STALL: Duration = Duration::from_secs(3);

/// Reads the current track, then forwards every later change until the channel
/// closes or the player leaves the bus.
///
/// There is no polling loop: zbus keeps a property cache fed by the signal, so
/// reading `Metadata` after a change costs nothing on the bus.
pub async fn follow(
    connection: &Connection,
    name: &OwnedBusName,
    events: async_channel::Sender<Event>,
) -> Result<(), Error> {
    // Subscribing before the first read, not after: a song that changes in
    // between would be missed, and every later change would be compared against
    // a track that was already stale.
    let properties = PropertiesProxy::builder(connection)
        .destination(name.clone())?
        .path("/org/mpris/MediaPlayer2")?
        .build()
        .await?;
    let interface = InterfaceName::try_from(PLAYER_INTERFACE)?;
    let mut changes = properties.receive_properties_changed().await?;

    let player = PlayerProxy::builder(connection)
        .destination(name.clone())?
        .build()
        .await?;

    let mut last = Track::from_metadata(&player.metadata().await?);
    tracing::info!(player = short_name(name), track = ?last, "following");
    if events
        .send(Event::TrackChanged(last.clone()))
        .await
        .is_err()
    {
        return Ok(());
    }

    let mut state = Playback::from_mpris(&player.playback_status().await?);
    if events.send(Event::Playback(state)).await.is_err() {
        return Ok(());
    }

    let mut seen: Option<(Duration, Instant)> = None;
    let mut stalled = false;
    /// A player that has gone stops answering long before its name leaves the
    /// bus, and the signal stream it owned simply goes quiet. Without this the
    /// clock keeps counting and the overlay sings on alone.
    const GONE_AFTER: u32 = 6;
    let mut unanswered = 0u32;

    let mut ticker = tokio::time::interval(POLL);
    // A tick that arrives late is a tick that is no longer true; skip it rather
    // than firing a burst to catch up.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            // While the song plays, the position is the only thing that moves.
            _ = ticker.tick(), if state == Playback::Playing && !stalled => {
                let at = Instant::now();
                let micros = match player.position().await {
                    Ok(micros) => {
                        unanswered = 0;
                        micros
                    }
                    Err(error) => {
                        unanswered += 1;
                        tracing::debug!(%error, unanswered, "Position is not readable");
                        if unanswered >= GONE_AFTER {
                            break;
                        }
                        continue;
                    }
                };
                let Ok(micros) = u64::try_from(micros) else {
                    continue;
                };
                let reading = Duration::from_micros(micros);

                match seen {
                    Some((value, since)) if value == reading => {
                        if at.duration_since(since) >= STALL {
                            stalled = true;
                            tracing::warn!(
                                player = short_name(name),
                                "the player does not report its position"
                            );
                            if events.send(Event::PositionStalled).await.is_err() {
                                break;
                            }
                            continue;
                        }
                    }
                    _ => seen = Some((reading, at)),
                }

                if events.send(Event::Position { reading, at }).await.is_err() {
                    break;
                }
            }

            change = changes.next() => {
                let Some(change) = change else { break };
                let args = change.args()?;
                if args.interface_name != interface {
                    continue;
                }

                if let Some(status) = args.changed_properties.get("PlaybackStatus")
                    && let Ok(status) = status.downcast_ref::<&str>()
                {
                    let next = Playback::from_mpris(status);
                    if next != state {
                        state = next;
                        seen = None;
                        stalled = false;
                        tracing::debug!(?state, "playback changed");
                        if events.send(Event::Playback(state)).await.is_err() {
                            break;
                        }
                    }
                }

                let Some(metadata) = args.changed_properties.get("Metadata") else {
                    continue;
                };
                let metadata: HashMap<String, OwnedValue> =
                    match metadata.try_clone().and_then(TryInto::try_into) {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            tracing::warn!(%error, "Metadata arrived with an unexpected type");
                            continue;
                        }
                    };

                let track = Track::from_metadata(&metadata);
                if track == last {
                    continue;
                }
                last = track.clone();
                seen = None;
                stalled = false;
                tracing::info!(track = ?track, "track changed");
                if events.send(Event::TrackChanged(track)).await.is_err() {
                    break;
                }
            }
        }
    }

    tracing::info!(player = short_name(name), "the signal stream ended");
    Ok(())
}
