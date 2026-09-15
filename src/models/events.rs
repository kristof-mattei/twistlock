use hashbrown::HashMap;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value as JsonValue};

#[derive(Debug)]
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

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut fields = Map::deserialize(deserializer)?;

        let kind = match fields.remove("Type") {
            Some(JsonValue::String(kind)) => kind,
            Some(_) => return Err(D::Error::custom("`Type` is not a string")),
            None => return Err(D::Error::missing_field("Type")),
        };

        let fields = JsonValue::Object(fields);

        let event = match kind.as_str() {
            "container" => {
                Event::Container(EventBody::deserialize(fields).map_err(D::Error::custom)?)
            },
            "network" => Event::Network(EventBody::deserialize(fields).map_err(D::Error::custom)?),
            kind => {
                let body = EventBody::deserialize(fields).map_err(D::Error::custom)?;

                match kind {
                    "builder" => Event::Builder(body),
                    "config" => Event::Config(body),
                    "daemon" => Event::Daemon(body),
                    "image" => Event::Image(body),
                    "node" => Event::Node(body),
                    "plugin" => Event::Plugin(body),
                    "secret" => Event::Secret(body),
                    "service" => Event::Service(body),
                    "volume" => Event::Volume(body),
                    kind => Event::Unknown {
                        kind: kind.into(),
                        body,
                    },
                }
            },
        };

        Ok(event)
    }
}

#[derive(Deserialize, Debug)]
pub struct EventBody<Id> {
    #[serde(rename(deserialize = "Action"))]
    pub action: Box<str>,
    #[serde(rename(deserialize = "Actor"))]
    pub actor: EventActor<Id>,
    pub scope: EventScope,
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

#[derive(Deserialize, Debug)]
pub enum EventScope {
    #[serde(rename(deserialize = "local"))]
    Local,
    #[serde(rename(deserialize = "swarm"))]
    Swarm,
}

fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

#[derive(Deserialize, Debug)]
pub struct ContainerId(Box<str>);

impl ContainerId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_short(&self) -> &str {
        short(&self.0)
    }
}

impl std::fmt::Display for ContainerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Deserialize, Debug)]
pub struct NetworkId(Box<str>);

impl NetworkId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn as_short(&self) -> &str {
        short(&self.0)
    }
}

impl std::fmt::Display for NetworkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::models::events::{ContainerId, Event, NetworkId};

    fn parse(json: &str) -> Result<Event, serde_json::Error> {
        serde_json::from_str(json)
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
    fn as_short_truncates_to_twelve() {
        let id =
            ContainerId("0f9fc026ac7481ed0d6fc51f34cf7db5d821a3942cf719cbeef5925f853ab8e8".into());

        assert_eq!(id.as_short(), "0f9fc026ac74");
    }

    #[test]
    fn as_short_passes_shorter_id_through() {
        assert_eq!(NetworkId("abc".into()).as_short(), "abc");
    }

    #[test]
    fn missing_type_is_error() {
        let error = parse(r#"{"Action":"start"}"#).unwrap_err();

        assert_eq!(error.to_string(), "missing field `Type`");
    }

    #[test]
    fn non_string_type_is_error() {
        let error = parse(r#"{"Type":7,"Action":"start"}"#).unwrap_err();

        assert_eq!(error.to_string(), "`Type` is not a string");
    }
}
