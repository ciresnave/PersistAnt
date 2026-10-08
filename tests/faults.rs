// SPDX-License-Identifier: MIT OR Apache-2.0
//! The fault-injecting fake: a program can test its own failure handling without a broken disk.

use persistant::{Config, Error, Faults, Need, Needs, Store};

fn needs() -> Needs {
    Needs::new()
        .with(Need::Read)
        .with(Need::Write)
        .with(Need::AtomicReplace)
}

fn faulty(faults: Faults) -> Config {
    Config::Faulty {
        inner: Box::new(Config::Memory),
        faults,
    }
}

#[tokio::test]
async fn writes_fail_after_the_allowed_count() {
    let s = Store::open(faulty(Faults::fail_writes_after(2)), needs())
        .await
        .unwrap();
    s.replace("a", b"1".to_vec()).await.unwrap();
    s.replace("b", b"2".to_vec()).await.unwrap();
    let err = s.replace("c", b"3".to_vec()).await.unwrap_err();
    assert!(matches!(err, Error::Injected(_)), "{err:?}");
    assert_eq!(s.read("b").await.unwrap(), b"2");
}

#[tokio::test]
async fn failed_write_leaves_the_old_value() {
    let s = Store::open(faulty(Faults::fail_writes_after(1)), needs())
        .await
        .unwrap();
    s.replace("a", b"old".to_vec()).await.unwrap();
    assert!(s.replace("a", b"new".to_vec()).await.is_err());
    assert_eq!(s.read("a").await.unwrap(), b"old");
}

#[tokio::test]
async fn interrupted_write_writes_half_and_leaves_the_old_value() {
    let s = Store::open(faulty(Faults::interrupt_writes_after(1)), needs())
        .await
        .unwrap();
    s.replace("a", b"old".to_vec()).await.unwrap();
    let err = s.replace("a", b"new-value".to_vec()).await.unwrap_err();
    assert!(matches!(err, Error::Injected(_)), "{err:?}");
    assert_eq!(s.read("a").await.unwrap(), b"old");
}

#[tokio::test]
async fn no_faults_configured_means_no_failures() {
    let s = Store::open(faulty(Faults::none()), needs()).await.unwrap();
    for i in 0..20 {
        s.replace("a", vec![i]).await.unwrap();
    }
}

#[tokio::test]
async fn fault_wrapper_does_not_hide_a_refusal() {
    let masked = Config::Masked {
        inner: Box::new(faulty(Faults::none())),
        without: vec![Need::Read],
    };
    let err = Store::open(masked, needs()).await.unwrap_err();
    assert!(matches!(err, Error::Refused { .. }), "{err:?}");
}
