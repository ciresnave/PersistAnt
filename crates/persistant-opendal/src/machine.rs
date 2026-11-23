// SPDX-License-Identifier: MIT OR Apache-2.0
//! The matrix rows of the **machine** (power-loss) test of `blob.durable` (ROADMAP row 4,
//! `scripts/machine-durability/`).
//!
//! The test cuts the power of a Hyper-V guest, so it cannot run in the hosted-Linux job that
//! regenerates `matrix/opendal.tsv`; its rows live in their own file, `matrix/opendal-machine.tsv`,
//! written from one run's measurements by `machine_vm_blob row` (this function; the run id goes in the file's
//! header, never in a row, so two runs that agree give the same rows) and compared by the
//! `machine-durability` workflow. The rule for a row is the one every `on` row follows
//! ([`Verdict::into_row`]): it needs both controls. Here that is the synced std write holding every
//! cycle (positive) and the unsynced one losing in at least one cycle (known-bad).
//!
//! What a row does **not** say, and must not be read to say (in the row's `note`): the physical
//! disk's volatile cache and the host's power, other operating systems and filesystems (NTFS), torn
//! sector writes, and remote object stores.

use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};

/// What one run measured for one OpenDAL `fs` configuration, and for the two controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measured {
    /// `opendal-fs` (no scratch directory) or `opendal-fs-atomic` (`atomic_write_dir` set).
    pub backend: String,
    /// Power-cut cycles run on this backend.
    pub cycles: u32,
    /// Cycles in which every acknowledged file was present with its value.
    pub held: u32,
    /// Cycles that could not be read (the checker errored): they measure nothing.
    pub errors: u32,
    /// Files acknowledged per cycle that the check read back.
    pub acked: u32,
    /// The synced std write (positive control): cycles run and cycles that held.
    pub synced_cycles: u32,
    pub synced_held: u32,
    /// The unsynced std write (known-bad control): cycles run and cycles that lost files.
    pub unsynced_cycles: u32,
    pub unsynced_lost: u32,
}

/// The limits that stay UNVERIFIED, written into every row (`docs/DESIGN-DURABILITY-MACHINE.md` 4).
pub const LIMITS: &str = "guest power loss on ext4 under Hyper-V; UNVERIFIED: the physical disk's volatile cache and host power loss, NTFS and other filesystems, torn sector writes, remote object stores";

/// The row for one measured backend, or `None` when nothing was measured (no cycle ran, or one
/// could not be read: an error is not a loss, so no claim either way).
///
/// All cycles held and both controls as required: `on`. A lost file: `no`, `wrong`. All cycles held
/// but a control did not behave: `no`, `untested` (the harness proved nothing).
pub fn blob_durable_row(m: &Measured, version: &str) -> Option<Row> {
    if m.cycles == 0 || m.errors > 0 {
        return None;
    }
    let atomic = match m.backend.as_str() {
        "opendal-fs" => "unset",
        "opendal-fs-atomic" => "set",
        _ => return None,
    };
    let controls = Controls {
        positive_passed: m.synced_cycles > 0 && m.synced_held == m.synced_cycles,
        known_bad_failed: m.unsynced_cycles > 0 && m.unsynced_lost > 0,
    };
    let verdict = if m.held == m.cycles {
        Verdict::Exact
    } else {
        Verdict::Wrong
    };
    let metadata = Config::new()
        .with("arch", std::env::consts::ARCH)
        .with("os", std::env::consts::OS)
        .with("test", "machine::blob.durable")
        .with("kill", "power_cut")
        .with("host", "hyperv")
        .with("fs", "ext4")
        .with("cycles", m.cycles.to_string())
        .with("held", m.held.to_string())
        .with("acked", m.acked.to_string());
    let id = CapabilityId::new("blob.durable").expect("valid id");
    let config = Config::new().with("atomic_write_dir", atomic);
    let mut row = match verdict.into_row(
        id.clone(),
        &m.backend,
        version,
        config.clone(),
        metadata.clone(),
        controls,
    ) {
        Ok(r) => r,
        Err(_) => Row {
            subject_kind: "capability".to_string(),
            subject: id,
            backend: m.backend.clone(),
            version: version.to_string(),
            config,
            enabled: false,
            reason: Some(Reason::Untested),
            metadata,
            note: format!("controls did not hold: {controls:?}"),
            extra: Default::default(),
        },
    };
    if row.note.is_empty() {
        row.note = LIMITS.to_string();
    }
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good(backend: &str) -> Measured {
        Measured {
            backend: backend.to_string(),
            cycles: 3,
            held: 3,
            errors: 0,
            acked: 300,
            synced_cycles: 3,
            synced_held: 3,
            unsynced_cycles: 3,
            unsynced_lost: 3,
        }
    }

    #[test]
    fn every_cycle_held_with_both_controls_is_on() {
        let r = blob_durable_row(&good("opendal-fs"), "0.59.5").unwrap();
        assert!(r.enabled && r.reason.is_none());
        assert_eq!(r.config.get("atomic_write_dir"), Some("unset"));
        assert!(r.note.contains("UNVERIFIED"));
        let r = blob_durable_row(&good("opendal-fs-atomic"), "0.59.5").unwrap();
        assert_eq!(r.config.get("atomic_write_dir"), Some("set"));
    }

    #[test]
    fn one_lost_cycle_is_off_as_wrong() {
        let m = Measured {
            held: 2,
            ..good("opendal-fs")
        };
        let r = blob_durable_row(&m, "0.59.5").unwrap();
        assert!(!r.enabled);
        assert_eq!(r.reason, Some(Reason::Wrong));
    }

    #[test]
    fn a_harness_that_could_not_lose_proves_nothing() {
        // The known-bad control lost nothing: HELD means nothing.
        let m = Measured {
            unsynced_lost: 0,
            ..good("opendal-fs")
        };
        let r = blob_durable_row(&m, "0.59.5").unwrap();
        assert!(!r.enabled);
        assert_eq!(r.reason, Some(Reason::Untested));
        // The positive control lost a file: the harness itself is broken.
        let m = Measured {
            synced_held: 2,
            ..good("opendal-fs")
        };
        let r = blob_durable_row(&m, "0.59.5").unwrap();
        assert_eq!(r.reason, Some(Reason::Untested));
    }

    #[test]
    fn nothing_measured_is_no_row() {
        let m = Measured {
            errors: 1,
            held: 2,
            ..good("opendal-fs")
        };
        assert!(blob_durable_row(&m, "0.59.5").is_none());
        let m = Measured {
            cycles: 0,
            held: 0,
            ..good("opendal-fs")
        };
        assert!(blob_durable_row(&m, "0.59.5").is_none());
        assert!(blob_durable_row(&good("std-synced"), "0.59.5").is_none());
    }
}
