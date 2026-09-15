use hashbrown::HashMap;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value as JsonValue};

#[derive(Debug)]
pub enum Event {
    Builder(EventBody),
    Config(EventBody),
    Container(EventBody),
    Daemon(EventBody),
    Image(EventBody),
    Network(EventBody),
    Node(EventBody),
    Plugin(EventBody),
    Secret(EventBody),
    Service(EventBody),
    Volume(EventBody),
    Unknown { kind: Box<str>, body: EventBody },
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

        let body = EventBody::deserialize(JsonValue::Object(fields)).map_err(D::Error::custom)?;

        let event = match kind.as_str() {
            "builder" => Event::Builder(body),
            "config" => Event::Config(body),
            "container" => Event::Container(body),
            "daemon" => Event::Daemon(body),
            "image" => Event::Image(body),
            "network" => Event::Network(body),
            "node" => Event::Node(body),
            "plugin" => Event::Plugin(body),
            "secret" => Event::Secret(body),
            "service" => Event::Service(body),
            "volume" => Event::Volume(body),
            kind => Event::Unknown {
                kind: kind.into(),
                body,
            },
        };

        Ok(event)
    }
}

#[derive(Deserialize, Debug)]
pub struct EventBody {
    #[serde(rename(deserialize = "Action"))]
    pub action: Box<str>,
    #[serde(rename(deserialize = "Actor"))]
    pub actor: EventActor,
    pub scope: EventScope,
    pub time: u64,
    #[serde(rename(deserialize = "timeNano"))]
    pub time_nano: u64,
}

#[derive(Deserialize, Debug)]
pub struct EventActor {
    #[serde(rename(deserialize = "ID"))]
    pub id: Box<str>,
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

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::models::events::Event;

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
