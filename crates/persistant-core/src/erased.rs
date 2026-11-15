// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a `delete_erased` test found, and the matrix row it becomes. Shared by the drivers that
//! certify `blob.delete_erased` and `kv.delete_erased`, so each records the same row for the same
//! finding: **on** only when the erased delete left no canary and both controls held, **off as
//! missing** when the backend says it cannot, **off as wrong** when the canary survived.

use crate::{CapabilityId, Config, Controls, Reason, Row, Verdict};

/// What one trial found.
#[derive(Debug, Clone, Default)]
pub struct ErasedTrial {
    /// The backend said it cannot (`Unsupported`), with its words.
    pub unsupported: Option<String>,
    /// Before any delete, the second link showed the canary (the positive control).
    pub positive: bool,
    /// After a plain delete, the second link still showed the canary (the known-bad control).
    pub control_kept: bool,
    /// After the erased delete the key was gone and its second link showed no canary.
    pub erased: bool,
}

/// One matrix row from a trial. `Exact` only with both controls; an `Unsupported` is `Missing`.
pub fn erased_row(
    id: &str,
    backend: &str,
    version: &str,
    config: Config,
    env: &Config,
    test: &str,
    t: &ErasedTrial,
) -> Row {
    let id = CapabilityId::new(id).expect("valid id");
    let controls = Controls {
        positive_passed: t.positive,
        known_bad_failed: t.control_kept,
    };
    let verdict = if t.unsupported.is_some() {
        Verdict::Missing
    } else if t.erased {
        Verdict::Exact
    } else {
        Verdict::Wrong
    };
    let metadata = env.clone().with("test", test).with(
        "controls",
        if t.unsupported.is_some() {
            "not-run"
        } else if controls.positive_passed && controls.known_bad_failed {
            "held"
        } else {
            "failed"
        },
    );
    match verdict.into_row(
        id.clone(),
        backend,
        version,
        config.clone(),
        metadata.clone(),
        controls,
    ) {
        Ok(mut row) => {
            if let Some(m) = &t.unsupported {
                row.note = m.clone();
            }
            row
        }
        Err(_) => Row {
            subject_kind: "capability".to_string(),
            subject: id,
            backend: backend.to_string(),
            version: version.to_string(),
            config,
            enabled: false,
            reason: Some(Reason::Untested),
            metadata,
            note: format!("controls did not hold: {controls:?}"),
            extra: Default::default(),
        },
    }
}
