// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed records with schema versions, on the in-memory backend.

use persistant::{Config, Error, Need, Needs, Record, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Agent {
    name: String,
    restarts: u32,
}
impl Record for Agent {
    const KIND: &'static str = "agent";
    const SCHEMA: u32 = 1;
}

/// Version 2 renamed `restarts` to `restart_count`; version 1 data is upgraded on read.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct AgentV2 {
    name: String,
    restart_count: u32,
}
impl Record for AgentV2 {
    const KIND: &'static str = "agent";
    const SCHEMA: u32 = 2;
    fn upgrade(from: u32, mut data: Value) -> Result<Value, Error> {
        match from {
            1 => {
                let n = data["restarts"].take();
                data["restart_count"] = n;
                data.as_object_mut().unwrap().remove("restarts");
                Ok(data)
            }
            _ => Err(Error::SchemaUnsupported {
                kind: Self::KIND,
                found: from,
                expected: Self::SCHEMA,
            }),
        }
    }
}

fn needs() -> Needs {
    Needs::new()
        .with(Need::Read)
        .with(Need::Write)
        .with(Need::AtomicReplace)
        .with(Need::Delete)
}

async fn store() -> Store {
    Store::open(Config::Memory, needs()).await.unwrap()
}

#[tokio::test]
async fn record_round_trips() {
    let s = store().await;
    let a = Agent {
        name: "x".into(),
        restarts: 3,
    };
    s.put_record("agents/x", &a).await.unwrap();
    assert_eq!(s.get_record::<Agent>("agents/x").await.unwrap(), Some(a));
}

#[tokio::test]
async fn missing_key_is_none_not_an_error() {
    let s = store().await;
    assert_eq!(s.get_record::<Agent>("nope").await.unwrap(), None);
}

#[tokio::test]
async fn stored_form_carries_kind_and_schema() {
    let s = store().await;
    s.put_record(
        "k",
        &Agent {
            name: "x".into(),
            restarts: 1,
        },
    )
    .await
    .unwrap();
    let raw: Value = serde_json::from_slice(&s.read("k").await.unwrap()).unwrap();
    assert_eq!(raw["kind"], "agent");
    assert_eq!(raw["schema"], 1);
    assert_eq!(raw["data"]["restarts"], 1);
}

#[tokio::test]
async fn older_schema_is_upgraded_on_read() {
    let s = store().await;
    s.put_record(
        "k",
        &Agent {
            name: "x".into(),
            restarts: 4,
        },
    )
    .await
    .unwrap();
    let got = s.get_record::<AgentV2>("k").await.unwrap();
    assert_eq!(
        got,
        Some(AgentV2 {
            name: "x".into(),
            restart_count: 4
        })
    );
}

#[tokio::test]
async fn newer_schema_than_the_program_knows_is_refused() {
    let s = store().await;
    s.put_record(
        "k",
        &AgentV2 {
            name: "x".into(),
            restart_count: 4,
        },
    )
    .await
    .unwrap();
    let err = s.get_record::<Agent>("k").await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::SchemaUnsupported {
                found: 2,
                expected: 1,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn record_of_another_kind_is_refused() {
    let s = store().await;
    s.replace(
        "k",
        serde_json::to_vec(&json!({"kind":"other","schema":1,"data":{}})).unwrap(),
    )
    .await
    .unwrap();
    let err = s.get_record::<Agent>("k").await.unwrap_err();
    assert!(matches!(err, Error::KindMismatch { .. }), "{err:?}");
}

#[tokio::test]
async fn corrupt_bytes_are_a_typed_error() {
    let s = store().await;
    s.replace("k", b"{ not json".to_vec()).await.unwrap();
    let err = s.get_record::<Agent>("k").await.unwrap_err();
    assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
}

#[tokio::test]
async fn delete_removes_the_record_and_is_idempotent() {
    let s = store().await;
    s.put_record(
        "k",
        &Agent {
            name: "x".into(),
            restarts: 1,
        },
    )
    .await
    .unwrap();
    s.delete("k").await.unwrap();
    assert_eq!(s.get_record::<Agent>("k").await.unwrap(), None);
    s.delete("k").await.unwrap();
}

#[tokio::test]
async fn put_record_needs_the_declaration() {
    let s = Store::open(Config::Memory, Needs::new().with(Need::Read))
        .await
        .unwrap();
    let err = s
        .put_record(
            "k",
            &Agent {
                name: "x".into(),
                restarts: 1,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotDeclared(_)), "{err:?}");
}
