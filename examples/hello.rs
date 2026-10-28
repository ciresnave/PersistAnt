// SPDX-License-Identifier: MIT OR Apache-2.0
use persistant::blocking::registry;
use persistant::capability::{CapabilityId, choose::Durability, provider::ProviderNeeds};
use persistant::registry::Endpoint;
fn main() {
    let ids = ["kv.get", "kv.put"].map(|s| CapabilityId::new(s).unwrap());
    let needs = ProviderNeeds::new(ids.to_vec(), Durability::Volatile);
    let kv = registry::open_named("memory", &Endpoint::None, &needs).unwrap();
    kv.put("hello", b"world".to_vec(), None).unwrap();
    assert_eq!(kv.get("hello").unwrap(), Some(b"world".to_vec()));
}
