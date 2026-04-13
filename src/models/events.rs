use hashbrown::HashMap;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Event {
    #[serde(rename(deserialize = "Type"))]
    pub r#type: EventType,
    #[serde(rename(deserialize = "Action"))]
    pub action: EventAction,
    #[serde(rename(deserialize = "Actor"))]
    pub actor: EventActor,
    pub scope: EventScope,
    pub time: u64,
    #[serde(rename(deserialize = "timeNano"))]
    pub time_nano: u64,
}

#[derive(Deserialize, Debug)]
pub enum EventType {
    #[serde(rename(deserialize = "builder"))]
    Builder,
    #[serde(rename(deserialize = "config"))]
    Config,
    #[serde(rename(deserialize = "container"))]
    Container,
    #[serde(rename(deserialize = "daemon"))]
    Daemon,
    #[serde(rename(deserialize = "image"))]
    Image,
    #[serde(rename(deserialize = "network"))]
    Network,
    #[serde(rename(deserialize = "node"))]
    Node,
    #[serde(rename(deserialize = "plugin"))]
    Plugin,
    #[serde(rename(deserialize = "secret"))]
    Secret,
    #[serde(rename(deserialize = "service"))]
    Service,
    #[serde(rename(deserialize = "volume"))]
    Volume,
}

type EventAction = Box<str>;

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
