// SPDX-License-Identifier: MIT OR Apache-2.0
//! Files and directories only their creator can use, on Unix and on Windows.
//!
//! On Unix a file is created with mode `0600` and a directory with `0700`, whatever the process
//! umask. On Windows a new file gets a *protected* DACL (nothing inherited from its directory) with
//! one entry, full control for the user the process runs as; `restrict_to_current_user` is the
//! Win32 half. A file that already exists is never changed: the mode its owner gave it stays.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(windows)]
#[path = "owner_only_win.rs"]
mod win;

#[cfg(windows)]
pub use win::restrict_to_current_user;

/// Create `path` as a new, empty, owner-only file opened for reading and writing, or fail with
/// [`io::ErrorKind::AlreadyExists`] if anything is there. On Windows a file that cannot be
/// restricted is removed again and the error returned: nothing is left open to the directory's
/// readers.
pub fn create_new(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    #[cfg(windows)]
    if let Err(e) = restrict_to_current_user(path) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(file)
}

/// Create `path` and any missing parents; on Unix each directory this call creates is `0700`.
/// Existing directories are left as they are. Windows directories inherit their parent's ACL (a
/// file inside is restricted by its own, which does not depend on the directory).
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}
