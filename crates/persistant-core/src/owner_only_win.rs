// SPDX-License-Identifier: MIT OR Apache-2.0
//! Owner-only files on Windows: the Win32 half of what `0600` does on Unix.
//!
//! A new file on Windows inherits the ACL of its directory, which usually lets the other local users
//! (and `Authenticated Users`) read it. [`restrict_to_current_user`] replaces that with a *protected*
//! DACL (nothing is inherited from the directory) holding one entry: full control for the user the
//! process runs as. `SYSTEM` and the administrators are not listed, as root is not on a `0600` file;
//! an administrator can still take ownership, which Windows allows anywhere.

#![allow(
    unsafe_code,
    reason = "the Win32 security API is C; each call is bracketed and its result checked"
)]

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Inherit nothing to children (`NO_INHERITANCE`, an ACE flag value of 0).
const NO_INHERITANCE: u32 = 0;

/// `GENERIC_ALL`: read, write, delete, change the permissions, take ownership.
const GENERIC_ALL: u32 = 0x1000_0000;

/// Closes a handle on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful OpenProcessToken and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Frees an ACL that `SetEntriesInAclW` allocated, on drop.
struct LocalAcl(*mut ACL);

impl Drop for LocalAcl {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a successful SetEntriesInAclW, which allocates with LocalAlloc.
        unsafe { LocalFree(self.0.cast()) };
    }
}

fn win32(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// Make `path` readable and writable by the current user only, and by nobody through inheritance.
pub fn restrict_to_current_user(path: &Path) -> io::Result<()> {
    // The process token, then the user SID in it.
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess is a pseudo-handle; `token` receives a real handle on success.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut len = 0u32;
    // SAFETY: a first call with no buffer only reports the size needed.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    // 8-byte aligned storage for a TOKEN_USER and the SID it points into.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `len` bytes and suitably aligned.
    if unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call above filled `buf` with a TOKEN_USER.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };

    // One entry: this user, full control, nothing for children.
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
        },
    };
    let mut acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: one well-formed entry whose SID outlives the call; `acl` receives a LocalAlloc block.
    let code = unsafe { SetEntriesInAclW(1, &entry, std::ptr::null(), &mut acl) };
    if code != ERROR_SUCCESS {
        return Err(win32(code));
    }
    let acl = LocalAcl(acl);

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated; PROTECTED stops the directory's ACL being inherited.
    let code = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl.0,
            std::ptr::null(),
        )
    };
    if code != ERROR_SUCCESS {
        return Err(win32(code));
    }
    Ok(())
}
