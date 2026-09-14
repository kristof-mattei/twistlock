use std::str::FromStr;

use serde::de::Error;
use serde::{Deserialize as _, Deserializer};

pub(crate) fn deserialize_null_as_empty<'de, D>(
    deserializer: D,
) -> Result<Box<[Box<str>]>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<Box<[Box<str>]>>::deserialize(deserializer)?.unwrap_or_default())
}

pub(crate) fn deserialize_empty_as_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: std::fmt::Display,
{
    match Option::<&str>::deserialize(deserializer)? {
        None | Some("") => Ok(None),
        Some(s) => T::from_str(s).map(Some).map_err(Error::custom),
    }
}
