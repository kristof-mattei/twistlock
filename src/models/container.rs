use std::fmt;

use hashbrown::HashMap;
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::models::container_inspect::ContainerNetworkSettings;

fn deserialize_names<'de, D>(deserializer: D) -> Result<Box<[Box<str>]>, D::Error>
where
    D: Deserializer<'de>,
{
    struct SeqVisitor();

    impl<'de> Visitor<'de> for SeqVisitor {
        type Value = Box<[Box<str>]>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a nonempty sequence of items")
        }

        fn visit_seq<M>(self, mut seq: M) -> Result<Self::Value, M::Error>
        where
            M: SeqAccess<'de>,
        {
            let mut buffer = seq.size_hint().map_or_else(Vec::new, Vec::with_capacity);

            while let Some(mut value) = seq.next_element::<String>()? {
                // Docker container name starts with a '/'. I don't know why. But it's useless.
                if value.starts_with('/') {
                    let split = value.split_off(1);

                    buffer.push(split.into_boxed_str());
                } else {
                    buffer.push(value.into_boxed_str());
                }
            }

            Ok(buffer.into())
        }
    }

    let visitor = SeqVisitor();
    deserializer.deserialize_seq(visitor)
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Container {
    pub id: Box<str>,
    #[serde(deserialize_with = "deserialize_names")]
    #[serde(rename(deserialize = "Names"))]
    pub names: Box<[Box<str>]>,
    pub state: Box<str>,
    pub labels: HashMap<Box<str>, Box<str>>,
    // The network settings do differ between list all containers and inspect container
    // but since we only use the common ones, we can reuse the type
    pub network_settings: ContainerNetworkSettings,
}

impl Container {
    #[must_use]
    pub fn get_short_id(&self) -> &str {
        #[expect(
            clippy::string_slice,
            reason = "ID is guaranteed to be hex, and thus ASCII"
        )]
        &self.id[0..12]
    }

    #[must_use]
    pub fn get_name(&self) -> Option<Box<str>> {
        let mut iter = self.names.iter();

        if let Some(first) = iter.next() {
            use std::fmt::Write as _;

            let mut names = (**first).to_owned();

            for next in iter {
                write!(names, ", {}", next).expect("Writing to a String never fails");
            }

            Some(names.into_boxed_str())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use hashbrown::HashMap;
    use pretty_assertions::assert_eq;

    use crate::models::container::Container;

    #[test]
    fn deserialize() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","Names":["/photoprism"],"Labels":{},"State":"running","NetworkSettings":{"Networks":{}}},{"Id":"281ea0c72e2e4a41fd2f81df945da9dfbfbc7ea0fe5e59c3d2a8234552e367cf","Names":["/whoogle-search"],"Labels":{},"State":"running","NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        assert!(deserialized.is_ok());

        let containers = deserialized.unwrap();
        assert_eq!(containers.len(), 2);

        assert_eq!(
            containers[0].id.as_ref(),
            "582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae"
        );
        assert_eq!(containers[0].names.len(), 1);
        assert_eq!(containers[0].names[0].as_ref(), "photoprism");
        assert_eq!(containers[0].state.as_ref(), "running");
        assert_eq!(containers[0].labels, HashMap::new());

        assert_eq!(
            containers[1].id.as_ref(),
            "281ea0c72e2e4a41fd2f81df945da9dfbfbc7ea0fe5e59c3d2a8234552e367cf"
        );
        assert_eq!(containers[1].names.len(), 1);
        assert_eq!(containers[1].names[0].as_ref(), "whoogle-search");
        assert_eq!(containers[1].state.as_ref(), "running");
        assert_eq!(containers[0].labels, HashMap::new());
    }

    #[test]
    fn deserialize_multiple_names() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","Names":["/photoprism-1","/photoprism-2"],"Labels":{}, "State":"running","NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        assert!(deserialized.is_ok());

        let containers = deserialized.unwrap();
        assert_eq!(containers.len(), 1);

        assert_eq!(
            containers[0].id.as_ref(),
            "582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae"
        );
        assert_eq!(containers[0].names.len(), 2);
        assert_eq!(containers[0].names[0].as_ref(), "photoprism-1");
        assert_eq!(containers[0].names[1].as_ref(), "photoprism-2");
        assert_eq!(containers[0].state.as_ref(), "running");
        assert_eq!(containers[0].labels, HashMap::new());
    }

    #[test]
    fn deserialize_timeout() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","Names":["/photoprism"],"State":"running","Labels":{"autoheal.stop.timeout":"12"},"NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        assert!(deserialized.is_ok());

        let containers = deserialized.unwrap();
        assert_eq!(containers.len(), 1);

        assert_eq!(
            containers[0].id.as_ref(),
            "582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae"
        );
        assert_eq!(containers[0].names.len(), 1);
        assert_eq!(containers[0].names[0].as_ref(), "photoprism");
        assert_eq!(containers[0].state.as_ref(), "running");
        assert_eq!(
            containers[0].labels,
            HashMap::from_iter([("autoheal.stop.timeout".into(), "12".into())])
        );
    }

    #[test]
    fn deserialize_no_labels() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","Names":["/photoprism"],"State":"running","NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        deserialized.unwrap_err();
    }

    #[test]
    fn deserialize_missing_timeout() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","Names":["/photoprism"],"State":"running","Labels":{"autoheal.stop.other_label":"some_value"},"NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        assert!(deserialized.is_ok());

        let containers = deserialized.unwrap();
        assert_eq!(containers.len(), 1);

        assert_eq!(
            containers[0].id.as_ref(),
            "582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae"
        );
        assert_eq!(containers[0].names.len(), 1);
        assert_eq!(containers[0].names[0].as_ref(), "photoprism");
        assert_eq!(containers[0].state.as_ref(), "running");
        assert_eq!(
            containers[0].labels,
            HashMap::from_iter([("autoheal.stop.other_label".into(), "some_value".into())])
        );
    }

    #[test]
    fn deserialize_with_no_names_array() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","State":"running","Labels":{"autoheal.stop.other_label":"some_value"},"NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        deserialized.unwrap_err();
    }

    #[test]
    fn deserialize_names_empty_names_array() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","Names":[],"State":"running","Labels":{"autoheal.stop.other_label":"some_value"},"NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        assert!(deserialized.is_ok());

        let containers = deserialized.unwrap();
        assert_eq!(containers.len(), 1);

        assert_eq!(
            containers[0].id.as_ref(),
            "582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae"
        );
        assert_eq!(containers[0].names.len(), 0);
        assert_eq!(containers[0].state.as_ref(), "running");
        assert_eq!(
            containers[0].labels,
            HashMap::from_iter([("autoheal.stop.other_label".into(), "some_value".into())])
        );
    }

    #[test]
    fn deserialize_multiple_names_with_and_without_slash() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","Names":["/photoprism-1","photoprism-2"],"Labels":{},"State":"running","NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        assert!(deserialized.is_ok());

        let containers = deserialized.unwrap();
        assert_eq!(containers.len(), 1);

        assert_eq!(
            containers[0].id.as_ref(),
            "582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae"
        );
        assert_eq!(containers[0].names.len(), 2);
        assert_eq!(containers[0].names[0].as_ref(), "photoprism-1");
        assert_eq!(containers[0].names[1].as_ref(), "photoprism-2");
        assert_eq!(containers[0].state.as_ref(), "running");
        assert_eq!(containers[0].labels, HashMap::new());
    }

    #[test]
    fn deserialize_invalid_labels() {
        let input = r#"[{"Id":"582036c7a5e8719bbbc9476e4216bfaf4fd318b61723f41f2e8fe3b60d8182ae","Names":["/foo"],"State":"running","Labels":"I am not a map, but a string","NetworkSettings":{"Networks":{}}}]"#;

        let deserialized: Result<Vec<Container>, _> = serde_json::from_slice(input.as_bytes());

        assert!(deserialized.is_err());

        assert_eq!(
            deserialized.unwrap_err().to_string(),
            "invalid type: string \"I am not a map, but a string\", expected a map at line 1 column 148"
        );
    }
}
