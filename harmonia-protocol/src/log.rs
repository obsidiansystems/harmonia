// SPDX-FileCopyrightText: 2024 griff
// SPDX-FileCopyrightText: 2025 Jörg Thalheim
// SPDX-License-Identifier: EUPL-1.2 OR MIT
//
// Logging types for the Nix daemon protocol.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
#[cfg(any(test, feature = "test"))]
use test_strategy::Arbitrary;

#[cfg(test)]
use crate::ProtocolVersion;

#[cfg(any(test, feature = "test"))]
use harmonia_utils_test::arb_byte_string;

pub type ByteString = Bytes;

fn serialize_byte_string<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let s = String::from_utf8_lossy(bytes);
    serializer.serialize_str(&s)
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize,
)]
#[serde(try_from = "u16", into = "u16")]
#[cfg_attr(any(test, feature = "test"), derive(Arbitrary))]
#[repr(u16)]
pub enum Verbosity {
    #[default]
    Error = 0,
    Warn = 1,
    Notice = 2,
    Info = 3,
    Talkative = 4,
    Chatty = 5,
    Debug = 6,
    Vomit = 7,
}

impl From<u16> for Verbosity {
    fn from(value: u16) -> Self {
        match value {
            0 => Self::Error,
            1 => Self::Warn,
            2 => Self::Notice,
            3 => Self::Info,
            4 => Self::Talkative,
            5 => Self::Chatty,
            6 => Self::Debug,
            _ => Self::Vomit,
        }
    }
}

impl From<Verbosity> for u16 {
    fn from(value: Verbosity) -> u16 {
        value as u16
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(try_from = "u16", into = "u16")]
#[cfg_attr(any(test, feature = "test"), derive(Arbitrary))]
#[repr(u16)]
pub enum ActivityType {
    Unknown = 0,
    CopyPath = 100,
    FileTransfer = 101,
    Realise = 102,
    CopyPaths = 103,
    Builds = 104,
    Build = 105,
    OptimiseStore = 106,
    VerifyPaths = 107,
    Substitute = 108,
    QueryPathInfo = 109,
    PostBuildHook = 110,
    BuildWaiting = 111,
    FetchTree = 112,
}

impl TryFrom<u16> for ActivityType {
    type Error = u16;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unknown),
            100 => Ok(Self::CopyPath),
            101 => Ok(Self::FileTransfer),
            102 => Ok(Self::Realise),
            103 => Ok(Self::CopyPaths),
            104 => Ok(Self::Builds),
            105 => Ok(Self::Build),
            106 => Ok(Self::OptimiseStore),
            107 => Ok(Self::VerifyPaths),
            108 => Ok(Self::Substitute),
            109 => Ok(Self::QueryPathInfo),
            110 => Ok(Self::PostBuildHook),
            111 => Ok(Self::BuildWaiting),
            112 => Ok(Self::FetchTree),
            other => Err(other),
        }
    }
}

impl From<ActivityType> for u16 {
    fn from(value: ActivityType) -> u16 {
        value as u16
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(try_from = "u16", into = "u16")]
#[cfg_attr(any(test, feature = "test"), derive(Arbitrary))]
#[repr(u16)]
pub enum ResultType {
    FileLinked = 100,
    BuildLogLine = 101,
    UntrustedPath = 102,
    CorruptedPath = 103,
    SetPhase = 104,
    Progress = 105,
    SetExpected = 106,
    PostBuildLogLine = 107,
    FetchStatus = 108,
}

impl TryFrom<u16> for ResultType {
    type Error = u16;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            100 => Ok(Self::FileLinked),
            101 => Ok(Self::BuildLogLine),
            102 => Ok(Self::UntrustedPath),
            103 => Ok(Self::CorruptedPath),
            104 => Ok(Self::SetPhase),
            105 => Ok(Self::Progress),
            106 => Ok(Self::SetExpected),
            107 => Ok(Self::PostBuildLogLine),
            108 => Ok(Self::FetchStatus),
            other => Err(other),
        }
    }
}

impl From<ResultType> for u16 {
    fn from(value: ResultType) -> u16 {
        value as u16
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum LogMessage {
    #[serde(rename = "msg")]
    Message(Message),
    #[serde(rename = "start")]
    StartActivity(Activity),
    #[serde(rename = "stop")]
    StopActivity(StopActivity),
    #[serde(rename = "result")]
    Result(ActivityResult),
}

impl LogMessage {
    pub fn message<T: Into<ByteString>>(text: T) -> LogMessage {
        LogMessage::Message(Message {
            level: Verbosity::Error,
            text: text.into(),
        })
    }
}

#[cfg(test)]
impl proptest::arbitrary::Arbitrary for LogMessage {
    type Parameters = ProtocolVersion;
    type Strategy = proptest::strategy::BoxedStrategy<Self>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        use proptest::prelude::*;
        if args.minor() >= 20 {
            prop_oneof![
                any::<Message>().prop_map(LogMessage::Message),
                any::<Activity>().prop_map(LogMessage::StartActivity),
                any::<ActivityResult>().prop_map(LogMessage::Result),
                any::<StopActivity>().prop_map(LogMessage::StopActivity)
            ]
            .boxed()
        } else {
            any::<Message>().prop_map(LogMessage::Message).boxed()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(Arbitrary))]
pub struct Message {
    pub level: Verbosity,
    #[cfg_attr(any(test, feature = "test"), strategy(arb_byte_string()))]
    #[serde(rename = "msg", serialize_with = "serialize_byte_string")]
    pub text: ByteString,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(Arbitrary))]
pub struct Activity {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    pub id: u64,
    pub level: Verbosity,
    pub parent: u64,
    #[cfg_attr(any(test, feature = "test"), strategy(arb_byte_string()))]
    #[serde(serialize_with = "serialize_byte_string")]
    pub text: ByteString, // If logger is JSON, invalid UTF-8 is replaced with U+FFFD
    #[serde(rename = "type")]
    pub activity_type: ActivityType,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(Arbitrary))]
pub struct StopActivity {
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(Arbitrary))]
pub struct ActivityResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    pub id: u64,
    #[serde(rename = "type")]
    pub result_type: ResultType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum FieldType {
    Int = 0,
    String = 1,
}

impl TryFrom<u16> for FieldType {
    type Error = u16;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Int),
            1 => Ok(Self::String),
            other => Err(other),
        }
    }
}

impl From<FieldType> for u16 {
    fn from(value: FieldType) -> u16 {
        value as u16
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "test"), derive(Arbitrary))]
#[serde(untagged)]
pub enum Field {
    Int(u64),
    String(
        #[cfg_attr(any(test, feature = "test"), strategy(arb_byte_string()))]
        #[serde(serialize_with = "serialize_byte_string")]
        ByteString,
    ),
}

#[cfg(test)]
mod unittests {
    use rstest::rstest;

    use super::{
        Activity, ActivityResult, ActivityType, Field, LogMessage, Message, ResultType,
        StopActivity, Verbosity,
    };

    #[rstest]
    #[case::message(
        r#"{"action":"msg","level":3,"msg":"these 501 derivations will be built:"}"#,
        LogMessage::Message(Message{level: Verbosity::Info, text: "these 501 derivations will be built:".into()})
    )]
    #[case::result(
        r#"{"action":"result","fields":[3,3,0,0],"id":342850059370512,"type":105}"#,
        LogMessage::Result(ActivityResult { id: 342850059370512, result_type: ResultType::Progress, fields: vec![Field::Int(3), Field::Int(3), Field::Int(0), Field::Int(0)] })
    )]
    #[case::start(
        r#"{"action":"start","fields":["/nix/store/rpd4ahsq8kk6i6ji31yww38466zxsmnx-cargo-vendor-dir","https://cache.nixos.org"],"id":342850059370553,"level":4,"parent":0,"text":"querying info about '/nix/store/rpd4ahsq8kk6i6ji31yww38466zxsmnx-cargo-vendor-dir' on 'https://cache.nixos.org'","type":109}"#,
        LogMessage::StartActivity(Activity {
            id: 342850059370553,
            level: Verbosity::Talkative,
            activity_type: ActivityType::QueryPathInfo,
            text: "querying info about '/nix/store/rpd4ahsq8kk6i6ji31yww38466zxsmnx-cargo-vendor-dir' on 'https://cache.nixos.org'".into(),
            fields: vec![
                Field::String("/nix/store/rpd4ahsq8kk6i6ji31yww38466zxsmnx-cargo-vendor-dir".into()),
                Field::String("https://cache.nixos.org".into()),
            ],
            parent: 0,
        })
    )]
    #[case::start_no_fields(
        r#"{"action":"start","id":342631016038421,"level":5,"parent":0,"text":"copying '/home/myself/nixpkgs/pkgs/build-support/fetchurl/write-mirror-list.sh' to the store","type":0}"#,
        LogMessage::StartActivity(Activity {
            id: 342631016038421,
            level: Verbosity::Chatty,
            activity_type: ActivityType::Unknown,
            text: "copying '/home/myself/nixpkgs/pkgs/build-support/fetchurl/write-mirror-list.sh' to the store".into(),
            fields: vec![],
            parent: 0,
        })
    )]
    #[case::stop(
        r#"{"action":"stop","id":342850059370518}"#,
        LogMessage::StopActivity(StopActivity { id: 342850059370518 })
    )]
    fn serialize_deserialize(#[case] json: &str, #[case] msg: LogMessage) {
        let actual: LogMessage = serde_json::from_str(json).unwrap();
        assert_eq!(actual, msg);
        let actual_s = serde_json::to_string(&msg).unwrap();
        assert_eq!(actual_s, json);
    }
}
