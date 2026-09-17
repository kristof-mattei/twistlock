use std::fmt;

use hashbrown::HashMap;
use serde::Deserialize;
use thiserror::Error;

use crate::models::id::{ContainerId, NetworkId};

/// A line of the event stream that did not decode into an [`Event`].
#[derive(Error)]
#[error("Failed to decode event: {}", String::from_utf8_lossy(.line))]
pub struct EventDecodeError {
    pub source: serde_json::Error,
    /// The line as received, without its newline.
    pub line: Box<[u8]>,
}

impl fmt::Debug for EventDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventDecodeError")
            .field("source", &self.source)
            .field("line", &String::from_utf8_lossy(&self.line))
            .finish()
    }
}

#[derive(Deserialize, Debug)]
#[serde(from = "RawEvent")]
pub enum Event {
    Builder(EventBody<Box<str>>),
    Config(EventBody<Box<str>>),
    Container(EventBody<ContainerId>),
    Daemon(EventBody<Box<str>>),
    Image(EventBody<Box<str>>),
    Network(EventBody<NetworkId>),
    Node(EventBody<Box<str>>),
    Plugin(EventBody<Box<str>>),
    Secret(EventBody<Box<str>>),
    Service(EventBody<Box<str>>),
    Volume(EventBody<Box<str>>),
    Unknown {
        kind: Box<str>,
        body: EventBody<Box<str>>,
    },
}

#[derive(Deserialize, Debug)]
pub struct EventBody<Id> {
    #[serde(rename(deserialize = "Action"))]
    pub action: Box<str>,
    #[serde(rename(deserialize = "Actor"))]
    pub actor: EventActor<Id>,
    pub time: u64,
    #[serde(rename(deserialize = "timeNano"))]
    pub time_nano: u64,
}

#[derive(Deserialize, Debug)]
pub struct EventActor<Id> {
    #[serde(rename(deserialize = "ID"))]
    pub id: Id,
    #[serde(rename(deserialize = "Attributes"))]
    pub attributes: HashMap<Box<str>, Box<str>>,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename(deserialize = "Type"))]
    kind: Box<str>,
    #[serde(flatten)]
    body: EventBody<Box<str>>,
}

impl EventBody<Box<str>> {
    fn with_id<Id>(self, wrap: fn(Box<str>) -> Id) -> EventBody<Id> {
        EventBody {
            action: self.action,
            actor: EventActor {
                id: wrap(self.actor.id),
                attributes: self.actor.attributes,
            },
            time: self.time,
            time_nano: self.time_nano,
        }
    }
}

impl From<RawEvent> for Event {
    fn from(RawEvent { kind, body }: RawEvent) -> Self {
        match &*kind {
            "builder" => Event::Builder(body),
            "config" => Event::Config(body),
            "container" => Event::Container(body.with_id(ContainerId::new)),
            "daemon" => Event::Daemon(body),
            "image" => Event::Image(body),
            "network" => Event::Network(body.with_id(NetworkId::new)),
            "node" => Event::Node(body),
            "plugin" => Event::Plugin(body),
            "secret" => Event::Secret(body),
            "service" => Event::Service(body),
            "volume" => Event::Volume(body),
            _ => Event::Unknown { kind, body },
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::models::events::{Event, EventDecodeError};

    fn parse(json: &str) -> Result<Event, serde_json::Error> {
        serde_json::from_str(json)
    }

    fn decode_error(line: &[u8]) -> EventDecodeError {
        EventDecodeError {
            source: serde_json::from_slice::<Event>(line).unwrap_err(),
            line: line.into(),
        }
    }

    fn event(kind: &str, action: &str) -> String {
        format!(
            r#"{{"Type":"{}","Action":"{}","Actor":{{"ID":"0f9fc026ac74","Attributes":{{"name":"ubuntu"}}}},"scope":"local","time":1,"timeNano":2}}"#,
            kind, action
        )
    }

    #[test]
    fn documented_type_selects_its_variant() {
        let Event::Volume(body) = parse(&event("volume", "create")).unwrap() else {
            panic!("not a volume event");
        };

        assert_eq!(&*body.action, "create");
    }

    #[test]
    fn undocumented_type_keeps_its_kind() {
        let Event::Unknown { kind, body } = parse(&event("sandwich", "toast")).unwrap() else {
            panic!("not an unknown event");
        };

        assert_eq!(&*kind, "sandwich");
        assert_eq!(&*body.action, "toast");
    }

    #[test]
    fn container_id_reaches_the_container_variant() {
        let Event::Container(body) = parse(&event("container", "start")).unwrap() else {
            panic!("not a container event");
        };

        assert_eq!(body.actor.id.as_str(), "0f9fc026ac74");
    }

    #[test]
    fn decode_error_displays_invalid_utf8_as_replacement_character() {
        assert_eq!(
            decode_error(b"{\xff").to_string(),
            format!("Failed to decode event: {{{}", char::REPLACEMENT_CHARACTER)
        );
    }

    #[test]
    fn decode_error_debug_prints_the_line_as_text() {
        let debug = format!("{:?}", decode_error(b"not json"));

        assert!(debug.contains(r#"line: "not json""#), "{}", debug);
    }
}
