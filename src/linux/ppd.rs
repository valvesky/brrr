//! power-profiles-daemon (or tuned-ppd) over the system D-Bus.
//!
//! Holds belong to our bus connection, so the daemon drops them whenever this
//! process exits, however it exits. Calls go to the daemon's unique name, so a
//! restarted daemon is a new owner and never answers for the old one.

use crate::core::engine::{AcquireError, Event, Events, Power, Profile};
use std::time::Duration;
use zbus::blocking::{Connection, MessageIterator, fdo::DBusProxy};
use zbus::message::Type;
use zbus::{MatchRule, names::BusName};

const DEST: &str = "net.hadess.PowerProfiles";
const PATH: &str = "/net/hadess/PowerProfiles";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

pub struct PowerProfiles {
    bus: Connection,
    events: Events,
}

/// Errors the daemon itself returns when it refuses a hold. Anything else
/// (timeouts, disconnects) leaves it unknown whether the hold was created.
fn definite_rejection(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::MethodError(name, _, _) if matches!(name.as_str(),
        "org.freedesktop.DBus.Error.InvalidArgs"
        | "org.freedesktop.DBus.Error.AccessDenied"
        | "org.freedesktop.DBus.Error.AuthFailed"))
}

fn message(error: &zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(name, Some(detail), _) => format!("{name}: {detail}"),
        other => other.to_string(),
    }
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().name(name.into()).spawn(work).expect("spawn a D-Bus thread");
}

impl PowerProfiles {
    pub fn new(events: Events) -> Result<PowerProfiles, String> {
        let bus = zbus::blocking::connection::Builder::system()
            .and_then(|builder| builder.method_timeout(Duration::from_secs(5)).build())
            .map_err(|error| format!("system bus: {error}"))?;

        // Subscribe before asking for the current owner, so no change is lost.
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender("org.freedesktop.DBus")
            .and_then(|rule| rule.interface("org.freedesktop.DBus"))
            .and_then(|rule| rule.member("NameOwnerChanged"))
            .and_then(|rule| rule.arg(0, DEST))
            .map(|rule| rule.build())
            .map_err(|error| error.to_string())?;
        let owners = MessageIterator::for_match_rule(rule, &bus, Some(64)).map_err(|error| error.to_string())?;
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(DEST)
            .and_then(|rule| rule.member("ProfileReleased"))
            .and_then(|rule| rule.path(PATH))
            .map(|rule| rule.build())
            .map_err(|error| error.to_string())?;
        let releases = MessageIterator::for_match_rule(rule, &bus, Some(64)).map_err(|error| error.to_string())?;
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(PROPERTIES)
            .and_then(|rule| rule.member("PropertiesChanged"))
            .and_then(|rule| rule.path(PATH))
            .map(|rule| rule.build())
            .map_err(|error| error.to_string())?;
        let properties = MessageIterator::for_match_rule(rule, &bus, Some(64)).map_err(|error| error.to_string())?;

        // Like GLib's AUTO_START name watching: ask the bus to start the daemon,
        // then report the current owner. Changes after this point are queued
        // in `owners` and delivered after it, so the last word is always right.
        let proxy = DBusProxy::new(&bus).map_err(|error| error.to_string())?;
        let _ = proxy.start_service_by_name(DEST.try_into().expect("valid name"), 0);
        let owner = BusName::try_from(DEST)
            .ok()
            .and_then(|name| proxy.get_name_owner(name).ok())
            .map(|owner| owner.to_string());
        if owner.is_some() {
            refresh_profile(&bus, &events);
        }
        let _ = events.send(Event::Owner(owner));

        let sender = events.clone();
        let connection = bus.clone();
        spawn("ppd-owner", move || {
            for message in owners {
                let Ok(message) = message else { continue };
                if let Ok((_, _, owner)) = message.body().deserialize::<(String, String, String)>() {
                    let owner = (!owner.is_empty()).then_some(owner);
                    if owner.is_some() {
                        refresh_profile(&connection, &sender);
                    }
                    let _ = sender.send(Event::Owner(owner));
                }
            }
        });
        let sender = events.clone();
        spawn("ppd-released", move || {
            for message in releases {
                let Ok(message) = message else { continue };
                let owner = message.header().sender().map(|sender| sender.to_string());
                if let (Some(owner), Ok(cookie)) = (owner, message.body().deserialize::<u32>()) {
                    let _ = sender.send(Event::ExternalRelease { owner, cookie });
                }
            }
        });
        let sender = events.clone();
        spawn("ppd-properties", move || {
            for message in properties {
                let Ok(message) = message else { continue };
                type Changed = (String, std::collections::HashMap<String, zbus::zvariant::OwnedValue>, Vec<String>);
                if let Ok((interface, changed, _)) = message.body().deserialize::<Changed>() {
                    let profile = changed.get("ActiveProfile").and_then(|value| String::try_from(value.clone()).ok());
                    if let (true, Some(profile)) = (interface == DEST, profile) {
                        let _ = sender.send(Event::Profile(Profile::from_name(&profile)));
                    }
                }
            }
        });
        let sender = events.clone();
        let connection = bus.clone();
        spawn("ppd-bus", move || {
            connection.closed();
            let _ = sender.send(Event::Fatal("system bus disconnected".into()));
        });

        Ok(PowerProfiles { bus, events })
    }
}

fn refresh_profile(bus: &Connection, events: &Events) {
    let reply = bus.call_method(Some(DEST), PATH, Some(PROPERTIES), "Get", &(DEST, "ActiveProfile"));
    let profile = reply
        .ok()
        .and_then(|reply| reply.body().deserialize::<zbus::zvariant::OwnedValue>().ok())
        .and_then(|value| String::try_from(value).ok());
    if let Some(profile) = profile {
        let _ = events.send(Event::Profile(Profile::from_name(&profile)));
    }
}

impl Power for PowerProfiles {
    fn name(&self) -> &'static str {
        "power-profiles"
    }

    fn acquire(&mut self, owner: &str, generation: u64) {
        let (bus, events, owner) = (self.bus.clone(), self.events.clone(), owner.to_string());
        spawn("ppd-hold", move || {
            let reply = bus.call_method(
                Some(owner.as_str()),
                PATH,
                Some(DEST),
                "HoldProfile",
                &("performance", "Whitelisted command running", "omapower"),
            );
            let result = match reply {
                Ok(reply) => reply
                    .body()
                    .deserialize::<u32>()
                    .map_err(|error| AcquireError::Ambiguous(error.to_string())),
                Err(error) if definite_rejection(&error) => Err(AcquireError::Rejected(message(&error))),
                Err(error) => Err(AcquireError::Ambiguous(message(&error))),
            };
            let _ = events.send(Event::Acquired { generation, result });
        });
    }

    fn release(&mut self, owner: &str, cookie: u32, generation: u64) {
        let (bus, events, owner) = (self.bus.clone(), self.events.clone(), owner.to_string());
        spawn("ppd-release", move || {
            let result = bus
                .call_method(Some(owner.as_str()), PATH, Some(DEST), "ReleaseProfile", &(cookie,))
                .map(|_| ())
                .map_err(|error| message(&error));
            let _ = events.send(Event::ReleaseDone { generation, result });
        });
    }
}
