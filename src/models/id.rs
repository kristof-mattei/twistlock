use serde::Deserialize;

fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

#[derive(Deserialize, Debug, PartialEq, Eq, Hash)]
pub struct ContainerId(Box<str>);

impl ContainerId {
    #[must_use]
    pub fn new<I: Into<Box<str>>>(id: I) -> Self {
        Self(id.into())
    }

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

pub enum ContainerRef<'r> {
    Id(&'r ContainerId),
    IdOrName(&'r str),
}

impl<'r> ContainerRef<'r> {
    pub(crate) fn as_str(&self) -> &'r str {
        match *self {
            ContainerRef::Id(id) => id.as_str(),
            ContainerRef::IdOrName(id_or_name) => id_or_name,
        }
    }
}

impl<'r> From<&'r ContainerId> for ContainerRef<'r> {
    fn from(id: &'r ContainerId) -> Self {
        ContainerRef::Id(id)
    }
}

#[derive(Deserialize, Debug, PartialEq, Eq, Hash)]
pub struct NetworkId(Box<str>);

impl NetworkId {
    #[must_use]
    pub fn new<I: Into<Box<str>>>(id: I) -> Self {
        Self(id.into())
    }

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

pub enum NetworkRef<'r> {
    Id(&'r NetworkId),
    IdOrName(&'r str),
}

impl<'r> NetworkRef<'r> {
    pub(crate) fn as_str(&self) -> &'r str {
        match *self {
            NetworkRef::Id(id) => id.as_str(),
            NetworkRef::IdOrName(id_or_name) => id_or_name,
        }
    }
}

impl<'r> From<&'r NetworkId> for NetworkRef<'r> {
    fn from(id: &'r NetworkId) -> Self {
        NetworkRef::Id(id)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::models::id::{ContainerId, NetworkId};

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
}
