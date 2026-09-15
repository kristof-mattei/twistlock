use hashbrown::HashMap;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value as JsonValue};

#[derive(Debug)]
pub enum Event {
    Container(ContainerEvent),
    Network(NetworkEvent),
    Other { kind: Box<str>, action: Box<str> },
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

        let event = match kind.as_str() {
            "container" => ContainerEvent::deserialize(JsonValue::Object(fields))
                .map(Event::Container)
                .map_err(D::Error::custom)?,
            "network" => NetworkEvent::deserialize(JsonValue::Object(fields))
                .map(Event::Network)
                .map_err(D::Error::custom)?,
            _ => {
                let action = match fields.remove("Action") {
                    Some(JsonValue::String(action)) => action,
                    Some(_) => return Err(D::Error::custom("`Action` is not a string")),
                    None => return Err(D::Error::missing_field("Action")),
                };

                Event::Other {
                    kind: kind.into(),
                    action: action.into(),
                }
            },
        };

        Ok(event)
    }
}

#[derive(Deserialize, Debug)]
pub struct ContainerEvent {
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
pub struct NetworkEvent {
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
    fn container_type_selects_container_variant() {
        let Event::Container(container) = parse(&event("container", "start")).unwrap() else {
            panic!("not a container event");
        };

        assert_eq!(&*container.action, "start");
        assert_eq!(&*container.actor.id, "0f9fc026ac74");
    }

    #[test]
    fn network_type_selects_network_variant() {
        let Event::Network(network) = parse(&event("network", "connect")).unwrap() else {
            panic!("not a network event");
        };

        assert_eq!(&*network.action, "connect");
        assert_eq!(&*network.actor.id, "0f9fc026ac74");
    }

    #[test]
    fn unhandled_type_keeps_kind_and_action() {
        let Event::Other { kind, action } = parse(&event("volume", "create")).unwrap() else {
            panic!("not an other event");
        };

        assert_eq!(&*kind, "volume");
        assert_eq!(&*action, "create");
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

    #[test]
    fn missing_action_on_unhandled_type_is_error() {
        let error = parse(r#"{"Type":"volume"}"#).unwrap_err();

        assert_eq!(error.to_string(), "missing field `Action`");
    }

    #[test]
    fn non_string_action_on_unhandled_type_is_error() {
        let error = parse(r#"{"Type":"volume","Action":7}"#).unwrap_err();

        assert_eq!(error.to_string(), "`Action` is not a string");
    }
}
