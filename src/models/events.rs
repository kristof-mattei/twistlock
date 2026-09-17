use hashbrown::HashMap;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[serde(from = "RawEvent")]
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

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename(deserialize = "Type"))]
    kind: Box<str>,
    #[serde(flatten)]
    body: EventBody,
}

impl From<RawEvent> for Event {
    fn from(RawEvent { kind, body }: RawEvent) -> Self {
        match &*kind {
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
            _ => Event::Unknown { kind, body },
        }
    }
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
}
