use std::fmt::Display;

use hashbrown::{HashMap, HashSet};
use serde::ser::SerializeSeq as _;
use serde::{Serialize, Serializer};

#[expect(clippy::ref_option, reason = "Serde API")]
fn single_to_string_array<S, T>(v: &Option<T>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Display,
{
    match *v {
        Some(ref value) => {
            let mut seq = serializer.serialize_seq(Some(1))?;
            seq.serialize_element(&value.to_string())?;
            seq.end()
        },
        None => serializer.serialize_none(),
    }
}

#[expect(clippy::ref_option, reason = "Serde API")]
fn multiple_to_string_array<S, T, U>(value: &Option<T>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    for<'a> &'a T: IntoIterator<Item = &'a U>,
    U: Display,
{
    match *value {
        Some(ref iter) => {
            let iter = iter.into_iter();

            let (_lower, upper) = iter.size_hint();

            let mut seq = serializer.serialize_seq(upper)?;

            for next in iter {
                seq.serialize_element(&next.to_string())?;
            }

            seq.end()
        },
        None => serializer.serialize_none(),
    }
}

#[expect(clippy::ref_option, reason = "Serde API")]
fn serialize_labels<S, T, U>(
    value: &Option<HashMap<T, Option<U>>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Display,
    U: Display,
{
    match *value {
        Some(ref iter) => {
            let iter = iter.into_iter();

            let (_lower, upper) = iter.size_hint();

            let mut seq = serializer.serialize_seq(upper)?;

            for (key, value) in iter {
                if let Some(ref value) = *value {
                    seq.serialize_element(&format!("{}={}", key, value))?;
                } else {
                    seq.serialize_element(&key.to_string())?;
                }
            }

            seq.end()
        },
        None => serializer.serialize_none(),
    }
}

pub enum Status {
    Created,
    Restarting,
    Running,
    Removing,
    Paused,
    Exited,
    Dead,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match *self {
            Status::Created => "created",
            Status::Restarting => "restarting",
            Status::Running => "running",
            Status::Removing => "removing",
            Status::Paused => "paused",
            Status::Exited => "exited",
            Status::Dead => "dead",
        };

        f.write_str(s)
    }
}

pub enum Health {
    Starting,
    Healthy,
    Unhealthy,
    None,
}

impl std::fmt::Display for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match *self {
            Health::Starting => "starting",
            Health::Healthy => "healthy",
            Health::Unhealthy => "unhealthy",
            Health::None => "none",
        };

        f.write_str(s)
    }
}

// ancestor=(<image-name>[:<tag>], <image id>, or <image@digest>)
// before=(<container id> or <container name>)
// expose=(<port>[/<proto>]|<startport-endport>/[<proto>])
// exited=<int> containers with exit code of <int>
// health=(starting|healthy|unhealthy|none)
// id=<ID> a container's ID
// isolation=(default|process|hyperv) (Windows daemon only)
// is-task=(true|false)
// label=key or label="key=value" of a container label
// name=<name> a container's name
// network=(<network id> or <network name>)
// publish=(<port>[/<proto>]|<startport-endport>/[<proto>])
// since=(<container id> or <container name>)
// status=(created|restarting|running|removing|paused|exited|dead)
// volume=(<volume name> or <mount point destination>)
// most members support supports `null` and `[]`, but in general we omit them when empty.
// Notable exception: `is_task`.
// by default there is an implicit `status=["running"]` filter. If you want to see e.g. `exited=["1"]`
// you have to manually include a `status=["exited"]`...
#[derive(Serialize, Default)]
pub struct Filters {
    #[serde(
        serialize_with = "single_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Yes, No, both (filter not emitted).
    // is-task=(true|false)
    // `skip_serializing_if` because `null` or empty list are not valid values
    // enum because you can only have 1 value in the list
    // `is-task=["true", "false"]` is invalid.
    // `is-task=[]` is invalid.
    // `is-task=null` is invalid.
    is_task: Option<bool>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Container Status, multiple values means containers with `StatusA OR StatusB`.
    // status=(created|restarting|running|removing|paused|exited|dead)
    status: Option<HashSet<Status>>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Filter by container exit code.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    ///
    /// TODO: This has NO effect if `Status::Exited` is NOT included in `status`.
    // exited=<int> containers with exit code of <int>
    exited: Option<HashSet<i32>>,

    #[serde(
        serialize_with = "serialize_labels",
        skip_serializing_if = "Option::is_none"
    )]
    /// Filter container with a certain label, or label with a value.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    /// * Filters are not `boolean`, e.g. `traefik.enable=false` is not the opposite set of `traefik.enable=true`.
    // label=key or label="key=value" of a container label
    label: Option<HashMap<Box<str>, Option<Box<str>>>>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Container Health, multiple values means containers with `HealthA OR HealthB`.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    // health=(starting|healthy|unhealthy|none)
    health: Option<HashSet<Health>>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Container name, multiple values means containers with `HealthA OR HealthB`.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    /// * `/foo` and `foo` are the same.
    // name=<name> a container's name
    name: Option<HashSet<Box<str>>>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Container ID, multiple values means containers with `IdA OR IdB`.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    /// * You do not need to provide the full ID, however, if you provide a partial it needs to uniquely match the container.
    ///   if 2 containers start with the same partial ID, nothing is returned
    // id=<ID> a container's ID
    id: Option<HashSet<Box<str>>>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Container volume, multiple values means containers with `VolumeA OR VolumeB`.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    // volume=(<volume name> or <mount point destination>)
    volume: Option<HashSet<Box<str>>>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Container network, multiple values means containers with `NetworkA OR NetworkB`.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    // network=(<network id> or <network name>)
    network: Option<HashSet<Box<str>>>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Filters container by ancestor, multiple values means containers with `ancestorA OR ancestorB`.
    ///
    /// Supports:
    /// * `<image-name>`, implies `<image-name>:latest`
    /// * `<image-name>:<tag>`
    /// * `<image@digest>`
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    // Does not support `null` or `[]`. Passing in these values filters out the whole list.
    // ancestor=(<image-name>[:<tag>], <image id>, or <image@digest>)
    ancestor: Option<HashSet<Box<str>>>,

    #[serde(
        serialize_with = "single_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Filters containers created before given container ID or name.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    // API takes `[...]` but only last value is considered
    // before=(<container id> or <container name>)
    before: Option<Box<str>>,

    #[serde(
        serialize_with = "single_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Filters containers created since given container ID or name.
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    // API takes `[...]` but only last value is considered
    // since=(<container id> or <container name>)
    since: Option<Box<str>>,

    #[serde(
        serialize_with = "multiple_to_string_array",
        skip_serializing_if = "Option::is_none"
    )]
    /// Filters containers by exposed ports.
    ///
    /// Supports:
    /// * `<port>[/<proto>]`
    /// * `<startport-endport>/[<proto>]`
    ///
    /// Notes:
    /// * Does not include exited containers by default.
    /// * API defines an alias of `expose`.
    // publish=(<port>[/<proto>]|<startport-endport>/[<proto>])
    publish: Option<HashSet<Box<str>>>,
    // alias of `publish`
    // expose=(<port>[/<proto>]|<startport-endport>/[<proto>])
    // expose: Option<HashSet<Box<str>>>,

    // TODO
    // isolation=(default|process|hyperv) (Windows daemon only)
}
