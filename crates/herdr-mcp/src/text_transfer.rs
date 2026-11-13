//! HOME-scoped text transfer for `herdr_mcp.text.read` / `herdr_mcp.text.write`.
//!
//! `text.write` used to be a "check the path, then act on the path" sequence.
//! That is unsound in the presence of a concurrent path swap, so the publish
//! phase is now descriptor-relative:
//!
//! * Unix (macOS/Linux): the parent directory is opened component by component
//!   from a pinned root descriptor (`O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`), a
//!   temporary file is created *inside that descriptor*, and the rename is a
//!   same-directory `renameat`/`renameat2(RENAME_NOREPLACE)`. Immediately
//!   before publishing, the parent's *current* absolute path is re-derived from
//!   the descriptor itself (`F_GETPATH` on macOS, `/proc/self/fd` on Linux), the
//!   walked path is re-checked against the opened inode, and the single-writer
//!   mutation lease is re-validated through the runtime commit fence.
//! * Windows: no dirfd exists, so the parent/leaf are re-checked through
//!   reparse-point attributes and published with `MoveFileExW`. This is
//!   deliberately weaker than the Unix path (no descriptor pinning, no `nlink`
//!   check): the TOCTOU window is narrower but not eliminated. That branch is
//!   compile-validated by the Windows candidate CI job only.
//! * Other Unix targets: fail closed. There is no unchecked path fallback.
//!
//! Exactly one production entry may commit a write: `write_fenced`, driven by
//! the runtime route in `mcp.rs`. The legacy unfenced `write` entry performs
//! read-only validation and then refuses with
//! `text_write_requires_fenced_route`, so no caller can publish without the
//! commit fence.

use crate::fs_security::{denied_home_credential_path, path_within};
use crate::state_store::{MutationLeaseEnforcementVerdict, StateStore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TEXT_BYTES: usize = 256 * 1024;

pub fn read(params: &Value) -> Value {
    let path = match required_string(params, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let max_bytes = match optional_usize(params, "max_bytes", 1, MAX_TEXT_BYTES) {
        Ok(value) => value.unwrap_or(MAX_TEXT_BYTES),
        Err(error) => return error,
    };
    let path = match safe_home_path(&path, false) {
        Ok(value) => value,
        Err(error) => return error,
    };
    // Open the source by descriptor and never follow a final symlink. A FIFO or
    // device must not block the caller, so the descriptor is opened non-blocking
    // and rejected by `fstat` instead.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // `FILE_FLAG_OPEN_REPARSE_POINT` opens the reparse point itself instead
        // of following it, so a symlinked leaf is observed rather than resolved.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = match options.open(&path) {
        Ok(value) => value,
        Err(error) => return open_source_error(&path, error),
    };
    let metadata = match file.metadata() {
        Ok(value) => value,
        Err(error) => return io_fail("read_failed", &path, error),
    };
    if !metadata.is_file() {
        return fail(
            "path_not_file",
            "text transfer source must be a regular file",
            &path,
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // In-HOME hardlinks are over-refused by design: a second name for the
        // same inode can be reached without crossing any directory boundary the
        // walk would notice. APFS clones and Time Machine snapshots do not
        // raise `st_nlink`, so they are not covered by this check.
        if metadata.nlink() != 1 {
            return hardlink_denied(&path);
        }
    }
    if metadata.len() > max_bytes as u64 {
        return json!({
            "ok": false,
            "code": "text_too_large",
            "path": path,
            "bytes": metadata.len(),
            "max_bytes": max_bytes,
        });
    }
    let mut bytes = Vec::new();
    if let Err(error) = (&mut file)
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
    {
        return io_fail("read_failed", &path, error);
    }
    if bytes.len() > max_bytes {
        return json!({
            "ok": false,
            "code": "text_too_large",
            "path": path,
            "bytes": bytes.len(),
            "max_bytes": max_bytes,
        });
    }
    let content = match String::from_utf8(bytes) {
        Ok(value) => value,
        Err(_) => {
            return fail(
                "text_utf8_required",
                "text transfer accepts UTF-8 text only",
                &path,
            );
        }
    };
    if sensitive_text(&path, &content) {
        return fail(
            "sensitive_content_denied",
            "text transfer refuses detected credentials or private keys",
            &path,
        );
    }
    let digest = sha256_hex(content.as_bytes());
    json!({
        "ok": true,
        "path": path,
        "bytes": content.len(),
        "sha256": digest,
        "content": content,
    })
}

/// Commit-time authority handed to [`write_fenced`].
///
/// It carries the *same* lease authority the runtime uses for admission
/// (`StateStore::check_root_mutation_lease_for_path`) plus the caller's lease
/// credentials. It is not a second lease authority and adds no lock file: the
/// fence re-runs the existing check immediately before the publish rename so a
/// lease acquired after admission still blocks the commit.
pub(crate) struct CommitFence {
    pub(crate) store: Arc<Mutex<StateStore>>,
    pub(crate) holder_id: Option<String>,
    pub(crate) lease_epoch: Option<i64>,
    pub(crate) lease_token: Option<String>,
    pub(crate) session_bound: bool,
}

impl CommitFence {
    fn check_target(&self, target: &Path) -> Result<(), Value> {
        let Some(target) = target.to_str() else {
            return Err(
                json!({"ok": false, "code": "invalid_path", "message": "text transfer requires UTF-8 paths"}),
            );
        };
        let Ok(store) = self.store.lock() else {
            return Err(crate::mcp::format_mutation_lease_conflict(
                "lease_store_unavailable",
                None,
            ));
        };
        match store.check_root_mutation_lease_for_path(
            target,
            self.holder_id.as_deref(),
            self.lease_epoch,
            self.lease_token.as_deref(),
            self.session_bound,
            crate::mcp::mutation_lease_now_ms(),
        ) {
            Ok(MutationLeaseEnforcementVerdict::Allowed { .. }) => Ok(()),
            Ok(MutationLeaseEnforcementVerdict::Conflict {
                reason,
                active_lease,
            }) => Err(crate::mcp::format_mutation_lease_conflict(
                reason,
                Some(&active_lease),
            )),
            Err(_) => Err(crate::mcp::format_mutation_lease_conflict(
                "lease_target_path_invalid",
                None,
            )),
        }
    }
}

/// Legacy unfenced entry point, retained for test-only callers.
///
/// Production builds have exactly one commit entry (`write_fenced`), so no
/// route can publish without the runtime commit fence. This wrapper still
/// performs every read-only validation the old implementation did — existing
/// refusal codes are unchanged — and then refuses with
/// [`fenced_route_required`].
#[cfg(test)]
pub fn write(params: &Value) -> Value {
    write_inner(params, None)
}

/// Unique production commit entry for `herdr_mcp.text.write`.
pub fn write_fenced(params: &Value, fence: &CommitFence) -> Value {
    write_inner(params, Some(fence))
}

fn write_inner(params: &Value, fence: Option<&CommitFence>) -> Value {
    if let Err(error) = crate::mutation::check_global(crate::progressive_skills::TEXT_WRITE_METHOD)
    {
        return error;
    }
    let path = match required_string(params, "path") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let content = match required_string(params, "content") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let expected_sha256 = match required_string(params, "sha256") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let overwrite = match optional_bool(params, "overwrite") {
        Ok(value) => value.unwrap_or(false),
        Err(error) => return error,
    };
    let backup = match optional_bool(params, "backup") {
        Ok(value) => value.unwrap_or(true),
        Err(error) => return error,
    };
    if sensitive_text(Path::new(&path), &content) {
        return fail(
            "sensitive_content_denied",
            "text transfer refuses detected credentials or private keys",
            Path::new(&path),
        );
    }
    if content.len() > MAX_TEXT_BYTES {
        return json!({
            "ok": false,
            "code": "text_too_large",
            "bytes": content.len(),
            "max_bytes": MAX_TEXT_BYTES,
        });
    }
    let actual_sha256 = sha256_hex(content.as_bytes());
    if expected_sha256 != actual_sha256 {
        return json!({
            "ok": false,
            "code": "sha256_mismatch",
            "expected_sha256": expected_sha256,
            "actual_sha256": actual_sha256,
        });
    }
    let target = match safe_home_path(&path, true) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if let Err(error) = crate::mutation::check_text_write_root(&target) {
        return error;
    }
    let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
        return fail(
            "invalid_path",
            "text transfer target has no parent directory",
            &target,
        );
    };
    let (parent, name) = (parent.to_path_buf(), name.to_os_string());
    let home = match home_root() {
        Ok(value) => value,
        Err(error) => return error,
    };
    let plan = PublishPlan {
        home,
        target,
        parent,
        name,
        content: &content,
        overwrite,
        backup,
        sha256: &actual_sha256,
        stamp: backup_stamp(),
    };
    // The unfenced route can never mutate: it stops here with the same refusal
    // the progressive-skills local route returns.
    let Some(fence) = fence else {
        return fenced_route_required();
    };
    platform::publish(&plan, fence)
}

/// Refusal used by every route that would otherwise publish without a fence.
pub(crate) fn fenced_route_required() -> Value {
    json!({
        "ok": false,
        "code": "text_write_requires_fenced_route",
        "retryable": false,
        "delivery_state": "not_delivered",
        "message": "herdr_mcp.text.write must run through the runtime commit fence; unfenced text transfer refuses to publish",
    })
}

struct PublishPlan<'a> {
    home: PathBuf,
    target: PathBuf,
    parent: PathBuf,
    name: OsString,
    content: &'a str,
    overwrite: bool,
    backup: bool,
    sha256: &'a str,
    stamp: u128,
}

impl PublishPlan<'_> {
    fn success(&self, backup_path: Option<PathBuf>) -> Value {
        json!({
            "ok": true,
            "path": self.target,
            "bytes": self.content.len(),
            "sha256": self.sha256,
            "backup_path": backup_path,
        })
    }
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;
    use rustix::fs::{
        AtFlags, Mode, OFlags, RenameFlags, openat, renameat, renameat_with, unlinkat,
    };
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Component;

    /// A directory descriptor pinned by the walk plus the canonical path that
    /// was used to reach it. The path is only used to detect replacement; the
    /// descriptor is the authority for every mutation.
    struct PinnedParent {
        dir: File,
        walk_path: PathBuf,
        roots: Vec<PathBuf>,
    }

    impl PinnedParent {
        fn open(plan: &PublishPlan<'_>) -> Result<Self, Value> {
            let roots = allowed_roots(&plan.home);
            let Some(root) = roots.iter().find(|root| path_within(root, &plan.parent)) else {
                return Err(fail(
                    "path_outside_home",
                    "text transfer is limited to HOME",
                    &plan.target,
                ));
            };
            let root_fd = openat(
                rustix::fs::CWD,
                root,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|error| open_error(error, root, &plan.target))?;
            let mut dir = File::from(root_fd);
            let relative = plan.parent.strip_prefix(root).unwrap_or(Path::new(""));
            for component in relative.components() {
                let Component::Normal(name) = component else {
                    return Err(fail(
                        "invalid_path",
                        "text transfer refuses non-normal path components",
                        &plan.target,
                    ));
                };
                let next = openat(
                    &dir,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| open_error(error, &plan.parent, &plan.target))?;
                dir = File::from(next);
            }
            Ok(Self {
                dir,
                walk_path: plan.parent.clone(),
                roots,
            })
        }

        /// Re-derive the parent's current absolute path from the descriptor and
        /// prove it is still the directory the walk recorded.
        fn verify_current(&self, target: &Path) -> Result<PathBuf, Value> {
            let current = descriptor_path(&self.dir)
                .map_err(|error| io_fail("parent_resolution_failed", &self.walk_path, error))?;
            require_utf8_path(&current)?;
            if !self.roots.iter().any(|root| path_within(root, &current)) {
                return Err(fail(
                    "text_transfer_parent_escaped",
                    "text transfer parent left the permitted write roots before publish",
                    &current,
                ));
            }
            if denied_home_credential_path(&current) {
                return Err(fail(
                    "secret_path_denied",
                    "text transfer refuses secret-like paths",
                    &current,
                ));
            }
            let opened = self
                .dir
                .metadata()
                .map_err(|error| io_fail("parent_resolution_failed", &self.walk_path, error))?;
            let walked = fs::symlink_metadata(&self.walk_path)
                .map_err(|error| io_fail("parent_resolution_failed", &self.walk_path, error))?;
            if walked.file_type().is_symlink()
                || !walked.is_dir()
                || walked.dev() != opened.dev()
                || walked.ino() != opened.ino()
            {
                return Err(fail(
                    "text_transfer_parent_replaced",
                    "text transfer parent was replaced between the walk and the publish",
                    target,
                ));
            }
            Ok(current)
        }
    }

    pub(super) fn publish(plan: &PublishPlan<'_>, fence: &CommitFence) -> Value {
        let pinned = match PinnedParent::open(plan) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let existing = match open_existing(&pinned.dir, plan) {
            Ok(value) => value,
            Err(error) => return error,
        };
        if existing.is_some() && !plan.overwrite {
            return fail(
                "target_exists",
                "set overwrite=true to replace an existing text file",
                &plan.target,
            );
        }

        let temp_name = temp_file_name(&plan.name);
        let temp_fd = match openat(
            &pinned.dir,
            &temp_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(value) => value,
            Err(error) => {
                return io_fail(
                    "temp_create_failed",
                    &plan.target,
                    std::io::Error::from(error),
                );
            }
        };
        let mut temp = File::from(temp_fd);
        if let Err(error) = temp
            .write_all(plan.content.as_bytes())
            .and_then(|()| temp.sync_all())
        {
            remove_entry(&pinned.dir, &temp_name);
            return io_fail("write_failed", &plan.target, error);
        }
        if let Some((_, metadata)) = &existing
            && let Err(error) =
                temp.set_permissions(fs::Permissions::from_mode(metadata.mode() & 0o7777))
        {
            remove_entry(&pinned.dir, &temp_name);
            return io_fail("permissions_failed", &plan.target, error);
        }

        #[cfg(test)]
        if let Some(hook) = test_hooks::take_before_commit() {
            hook(&pinned.walk_path, &plan.target);
        }

        let current_parent = match pinned.verify_current(&plan.target) {
            Ok(value) => value,
            Err(error) => {
                remove_entry(&pinned.dir, &temp_name);
                return error;
            }
        };
        let current_target = current_parent.join(&plan.name);
        if let Err(error) = fence.check_target(&current_target) {
            remove_entry(&pinned.dir, &temp_name);
            return error;
        }

        let backup_path = match existing {
            Some((file, _)) if plan.backup => {
                match write_backup(&pinned.dir, plan, &current_parent, file) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        remove_entry(&pinned.dir, &temp_name);
                        return error;
                    }
                }
            }
            _ => None,
        };

        let published = if plan.overwrite {
            renameat(&pinned.dir, &temp_name, &pinned.dir, &plan.name)
        } else {
            renameat_with(
                &pinned.dir,
                &temp_name,
                &pinned.dir,
                &plan.name,
                RenameFlags::NOREPLACE,
            )
        };
        if let Err(error) = published {
            remove_entry(&pinned.dir, &temp_name);
            if plan.overwrite {
                return io_fail("replace_failed", &plan.target, std::io::Error::from(error));
            }
            return match error {
                rustix::io::Errno::EXIST => fail(
                    "target_exists",
                    "set overwrite=true to replace an existing text file",
                    &plan.target,
                ),
                // A silently replacing fallback would defeat the no-clobber
                // guarantee, so an unsupported system call fails closed. On
                // macOS `renameatx_np` is a weak symbol; if it is absent the
                // call reports `NOSYS`.
                rustix::io::Errno::NOSYS | rustix::io::Errno::INVAL => fail(
                    "rename_no_replace_unsupported",
                    "atomic no-replace publish is unavailable on this filesystem",
                    &plan.target,
                ),
                other => io_fail("replace_failed", &plan.target, std::io::Error::from(other)),
            };
        }
        let _ = pinned.dir.sync_all();
        plan.success(backup_path)
    }

    fn open_existing(
        dir: &File,
        plan: &PublishPlan<'_>,
    ) -> Result<Option<(File, fs::Metadata)>, Value> {
        let fd = match openat(
            dir,
            &plan.name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(value) => value,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(rustix::io::Errno::LOOP) => {
                return Err(fail(
                    "symlink_denied",
                    "text transfer target must not be a symlink",
                    &plan.target,
                ));
            }
            Err(error) => {
                return Err(io_fail(
                    "target_inspect_failed",
                    &plan.target,
                    std::io::Error::from(error),
                ));
            }
        };
        let file = File::from(fd);
        let metadata = file
            .metadata()
            .map_err(|error| io_fail("target_inspect_failed", &plan.target, error))?;
        if !metadata.is_file() {
            return Err(fail(
                "path_not_file",
                "text transfer target must be a regular file",
                &plan.target,
            ));
        }
        // In-HOME hardlinks are over-refused by design: `st_nlink != 1` means
        // another name for the same inode exists somewhere the walk never saw.
        // APFS clones and Time Machine snapshots do not raise `st_nlink`, so
        // they are outside this check.
        if metadata.nlink() != 1 {
            return Err(hardlink_denied(&plan.target));
        }
        Ok(Some((file, metadata)))
    }

    fn write_backup(
        dir: &File,
        plan: &PublishPlan<'_>,
        current_parent: &Path,
        mut existing: File,
    ) -> Result<PathBuf, Value> {
        let backup_name = backup_file_name(&plan.name, plan.stamp);
        let backup_path = current_parent.join(&backup_name);
        let fd = match openat(
            dir,
            &backup_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(value) => value,
            Err(error) => {
                return Err(io_fail(
                    "backup_failed",
                    &backup_path,
                    std::io::Error::from(error),
                ));
            }
        };
        let mut backup = File::from(fd);
        let mut bytes = Vec::new();
        let read_result = (&mut existing)
            .take(MAX_TEXT_BYTES as u64 + 1)
            .read_to_end(&mut bytes);
        if let Err(error) = read_result {
            remove_entry(dir, &backup_name);
            return Err(io_fail("backup_failed", &backup_path, error));
        }
        if bytes.len() > MAX_TEXT_BYTES {
            remove_entry(dir, &backup_name);
            return Err(fail(
                "backup_failed",
                "existing target exceeds the bounded backup budget; the original was left untouched",
                &backup_path,
            ));
        }
        let write_result = backup.write_all(&bytes).and_then(|()| backup.sync_all());
        if let Err(error) = write_result {
            remove_entry(dir, &backup_name);
            return Err(io_fail("backup_failed", &backup_path, error));
        }
        Ok(backup_path)
    }

    fn remove_entry(dir: &File, name: &OsStr) {
        let _ = unlinkat(dir, name, AtFlags::empty());
    }

    fn open_error(error: rustix::io::Errno, path: &Path, target: &Path) -> Value {
        match error {
            rustix::io::Errno::LOOP => fail(
                "symlink_denied",
                "text transfer refuses a symlinked path component",
                target,
            ),
            rustix::io::Errno::NOENT
            | rustix::io::Errno::NOTDIR
            | rustix::io::Errno::ACCESS
            | rustix::io::Errno::PERM => fail(
                "parent_resolution_failed",
                "text transfer parent directory is not reachable",
                path,
            ),
            other => io_fail(
                "parent_resolution_failed",
                path,
                std::io::Error::from(other),
            ),
        }
    }

    #[cfg(target_os = "macos")]
    fn descriptor_path(dir: &File) -> std::io::Result<PathBuf> {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStringExt;
        let mut buffer = vec![0u8; 4096];
        let result = unsafe {
            libc::fcntl(
                dir.as_raw_fd(),
                libc::F_GETPATH,
                buffer.as_mut_ptr() as *mut libc::c_void,
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        buffer.truncate(end);
        Ok(PathBuf::from(OsString::from_vec(buffer)))
    }

    #[cfg(target_os = "linux")]
    fn descriptor_path(dir: &File) -> std::io::Result<PathBuf> {
        use std::os::fd::AsRawFd;
        // A deleted dirent keeps resolving as `<path> (deleted)`, which the
        // root-scope check then rejects because it no longer matches any root.
        fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd()))
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        GetFileAttributesW, INVALID_FILE_ATTRIBUTES, MOVEFILE_REPLACE_EXISTING,
        MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    const ERROR_FILE_EXISTS: i32 = 80;
    const ERROR_ALREADY_EXISTS: i32 = 183;

    pub(super) fn publish(plan: &PublishPlan<'_>, fence: &CommitFence) -> Value {
        if let Err(error) = verify_parent(plan) {
            return error;
        }
        let existing = match leaf_attributes(&plan.target) {
            Ok(value) => value,
            Err(error) => return error,
        };
        if existing.is_some() && !plan.overwrite {
            return fail(
                "target_exists",
                "set overwrite=true to replace an existing text file",
                &plan.target,
            );
        }

        let temp = plan.parent.join(temp_file_name(&plan.name));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = match options.open(&temp) {
            Ok(value) => value,
            Err(error) => return io_fail("temp_create_failed", &plan.target, error),
        };
        if let Err(error) = file
            .write_all(plan.content.as_bytes())
            .and_then(|()| file.sync_all())
        {
            let _ = fs::remove_file(&temp);
            return io_fail("write_failed", &plan.target, error);
        }
        if existing.is_some()
            && let Ok(metadata) = fs::metadata(&plan.target)
            && let Err(error) = fs::set_permissions(&temp, metadata.permissions())
        {
            let _ = fs::remove_file(&temp);
            return io_fail("permissions_failed", &plan.target, error);
        }

        #[cfg(test)]
        if let Some(hook) = test_hooks::take_before_commit() {
            hook(&plan.parent, &plan.target);
        }

        // No descriptor holds the parent on Windows, so this is a path-based
        // re-check: it narrows the window but cannot close it.
        if let Err(error) = verify_parent(plan) {
            let _ = fs::remove_file(&temp);
            return error;
        }
        if let Err(error) = fence.check_target(&plan.target) {
            let _ = fs::remove_file(&temp);
            return error;
        }

        let backup_path = if plan.backup && existing.is_some() {
            match write_backup(plan) {
                Ok(value) => Some(value),
                Err(error) => {
                    let _ = fs::remove_file(&temp);
                    return error;
                }
            }
        } else {
            None
        };

        let flags = if plan.overwrite {
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH
        } else {
            MOVEFILE_WRITE_THROUGH
        };
        let moved =
            unsafe { MoveFileExW(wide(&temp).as_ptr(), wide(&plan.target).as_ptr(), flags) };
        if moved == 0 {
            let error = std::io::Error::last_os_error();
            let _ = fs::remove_file(&temp);
            return match error.raw_os_error() {
                Some(ERROR_FILE_EXISTS) | Some(ERROR_ALREADY_EXISTS) if !plan.overwrite => fail(
                    "target_exists",
                    "set overwrite=true to replace an existing text file",
                    &plan.target,
                ),
                _ => io_fail("replace_failed", &plan.target, error),
            };
        }
        plan.success(backup_path)
    }

    fn verify_parent(plan: &PublishPlan<'_>) -> Result<(), Value> {
        match attributes(&plan.parent) {
            Some(attrs) if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 => Err(fail(
                "symlink_denied",
                "text transfer parent must not be a reparse point",
                &plan.target,
            )),
            Some(attrs) if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 => Err(fail(
                "parent_resolution_failed",
                "text transfer parent is not a directory",
                &plan.parent,
            )),
            Some(_) => {
                let roots = allowed_roots(&plan.home);
                let current = fs::canonicalize(&plan.parent)
                    .map_err(|error| io_fail("parent_resolution_failed", &plan.parent, error))?;
                if !roots.iter().any(|root| path_within(root, &current)) {
                    return Err(fail(
                        "text_transfer_parent_escaped",
                        "text transfer parent left the permitted write roots before publish",
                        &current,
                    ));
                }
                if denied_home_credential_path(&current) {
                    return Err(fail(
                        "secret_path_denied",
                        "text transfer refuses secret-like paths",
                        &current,
                    ));
                }
                Ok(())
            }
            None => Err(fail(
                "parent_resolution_failed",
                "text transfer parent directory is not reachable",
                &plan.parent,
            )),
        }
    }

    fn leaf_attributes(target: &Path) -> Result<Option<u32>, Value> {
        match attributes(target) {
            Some(attrs) if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 => Err(fail(
                "symlink_denied",
                "text transfer target must not be a symlink",
                target,
            )),
            Some(attrs) if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 => Err(fail(
                "path_not_file",
                "text transfer target must be a regular file",
                target,
            )),
            Some(_) => Ok(Some(1)),
            None => Ok(None),
        }
    }

    fn write_backup(plan: &PublishPlan<'_>) -> Result<PathBuf, Value> {
        let backup_name = backup_file_name(&plan.name, plan.stamp);
        let backup_path = plan.parent.join(&backup_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut backup = match options.open(&backup_path) {
            Ok(value) => value,
            Err(error) => return Err(io_fail("backup_failed", &backup_path, error)),
        };
        let mut read_options = OpenOptions::new();
        read_options
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut existing = match read_options.open(&plan.target) {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_file(&backup_path);
                return Err(io_fail("backup_failed", &backup_path, error));
            }
        };
        let mut bytes = Vec::new();
        if let Err(error) = (&mut existing)
            .take(MAX_TEXT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
        {
            let _ = fs::remove_file(&backup_path);
            return Err(io_fail("backup_failed", &backup_path, error));
        }
        if bytes.len() > MAX_TEXT_BYTES {
            let _ = fs::remove_file(&backup_path);
            return Err(fail(
                "backup_failed",
                "existing target exceeds the bounded backup budget; the original was left untouched",
                &backup_path,
            ));
        }
        if let Err(error) = backup.write_all(&bytes).and_then(|()| backup.sync_all()) {
            let _ = fs::remove_file(&backup_path);
            return Err(io_fail("backup_failed", &backup_path, error));
        }
        Ok(backup_path)
    }

    fn attributes(path: &Path) -> Option<u32> {
        let wide = wide(path);
        let value = unsafe { GetFileAttributesW(wide.as_ptr()) };
        (value != INVALID_FILE_ATTRIBUTES).then_some(value)
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

/// Targets without descriptor-relative primitives fail closed rather than
/// falling back to a path-based publish.
#[cfg(not(any(all(unix, any(target_os = "linux", target_os = "macos")), windows)))]
mod platform {
    use super::*;

    pub(super) fn publish(plan: &PublishPlan<'_>, _fence: &CommitFence) -> Value {
        fail(
            "text_write_platform_unsupported",
            "herdr_mcp.text.write requires the descriptor-relative commit path on this platform",
            &plan.target,
        )
    }
}

fn require_utf8_path(path: &Path) -> Result<(), Value> {
    if path.to_str().is_none() {
        return Err(
            json!({"ok": false, "code": "invalid_path", "message": "text transfer requires UTF-8 paths"}),
        );
    }
    Ok(())
}

fn home_root() -> Result<PathBuf, Value> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| json!({"ok": false, "code": "home_unavailable"}))?;
    require_utf8_path(&home)?;
    let home =
        fs::canonicalize(&home).map_err(|error| io_fail("home_resolution_failed", &home, error))?;
    require_utf8_path(&home)?;
    Ok(home)
}

/// Canonical HOME plus every configured write root. The commit-time path
/// authority accepts the parent while it is still inside one of these.
fn allowed_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.to_path_buf()];
    if let Ok(configured) = std::env::var("HERDR_MCP_WRITE_ROOTS") {
        for entry in configured.split(',').map(str::trim) {
            if entry.is_empty() {
                continue;
            }
            if let Ok(canonical) = fs::canonicalize(entry) {
                roots.push(canonical);
            }
        }
    }
    roots
}

pub(crate) fn safe_home_path(input: &str, allow_missing: bool) -> Result<PathBuf, Value> {
    let home = home_root()?;
    let candidate = PathBuf::from(input);
    let candidate = if candidate.is_absolute() {
        candidate
    } else {
        home.join(candidate)
    };
    if denied_home_credential_path(&candidate) {
        return Err(fail(
            "secret_path_denied",
            "text transfer refuses secret-like paths",
            &candidate,
        ));
    }
    match fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(fail(
                "symlink_denied",
                "text transfer refuses a symlink as the final path component",
                &candidate,
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && allow_missing => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(io_fail("path_resolution_failed", &candidate, error));
        }
        Err(error) => return Err(io_fail("path_inspect_failed", &candidate, error)),
    }
    if !allow_missing || candidate.exists() {
        let real = fs::canonicalize(&candidate)
            .map_err(|error| io_fail("path_resolution_failed", &candidate, error))?;
        if !path_within(&home, &real) {
            return Err(fail(
                "path_outside_home",
                "text transfer is limited to HOME",
                &candidate,
            ));
        }
        if denied_home_credential_path(&real) {
            return Err(fail(
                "secret_path_denied",
                "text transfer refuses secret-like paths",
                &candidate,
            ));
        }
        require_utf8_path(&real)?;
        return Ok(real);
    }
    let parent = candidate.parent().ok_or_else(|| {
        fail(
            "invalid_path",
            "text transfer target has no parent directory",
            &candidate,
        )
    })?;
    let parent_real = fs::canonicalize(parent)
        .map_err(|error| io_fail("parent_resolution_failed", parent, error))?;
    if !path_within(&home, &parent_real) {
        return Err(fail(
            "path_outside_home",
            "text transfer is limited to HOME",
            &candidate,
        ));
    }
    let name = candidate.file_name().ok_or_else(|| {
        fail(
            "invalid_path",
            "text transfer target has no file name",
            &candidate,
        )
    })?;
    let resolved = parent_real.join(name);
    if denied_home_credential_path(&resolved) {
        return Err(fail(
            "secret_path_denied",
            "text transfer refuses secret-like paths",
            &candidate,
        ));
    }
    require_utf8_path(&resolved)?;
    Ok(resolved)
}

fn lossy_name(name: &OsStr) -> String {
    name.to_str().unwrap_or("text").to_owned()
}

fn temp_file_name(name: &OsStr) -> OsString {
    OsString::from(format!(
        ".{}.herdr-transfer-{}-{}",
        lossy_name(name),
        std::process::id(),
        random_suffix()
    ))
}

fn backup_file_name(name: &OsStr, stamp: u128) -> OsString {
    #[cfg(test)]
    if test_hooks::pinned_backup_stamp().is_some() {
        return OsString::from(format!("{}.herdr-backup-{stamp}", lossy_name(name)));
    }
    OsString::from(format!(
        "{}.herdr-backup-{stamp}-{}",
        lossy_name(name),
        random_suffix()
    ))
}

/// pid + 64 random bits. `O_EXCL` creation is the real guarantee; the random
/// suffix only avoids predictable names.
fn random_suffix() -> String {
    let mut bytes = [0u8; 8];
    match getrandom::fill(&mut bytes) {
        Ok(()) => format!("{:016x}", u64::from_le_bytes(bytes)),
        Err(_) => format!("{:x}", now_nanos()),
    }
}

fn backup_stamp() -> u128 {
    #[cfg(test)]
    if let Some(stamp) = test_hooks::pinned_backup_stamp() {
        return stamp;
    }
    now_ms()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sensitive_text(path: &Path, content: &str) -> bool {
    if content.contains("-----BEGIN PRIVATE KEY-----")
        || content.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
        || content.contains("-----BEGIN RSA PRIVATE KEY-----")
        || contains_secret_assignment(content)
    {
        return true;
    }
    if path.extension().and_then(|value| value.to_str()) != Some("json") {
        return false;
    }
    serde_json::from_str::<Value>(content)
        .ok()
        .is_some_and(|value| json_contains_secret(&value))
}

fn contains_secret_assignment(content: &str) -> bool {
    const KEYS: &[&str] = &[
        "api_key",
        "apikey",
        "access_token",
        "refresh_token",
        "client_secret",
        "token",
        "password",
    ];
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            return false;
        }
        let lower = trimmed.to_ascii_lowercase();
        KEYS.iter().any(|key| {
            lower
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .is_some_and(|value| !value.trim().is_empty())
        }) || lower.starts_with("authorization: bearer ")
    })
}

fn json_contains_secret(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            let sensitive_key = key.contains("apikey")
                || key.contains("api_key")
                || key.contains("token")
                || key.contains("secret")
                || key.contains("credential")
                || key == "authorization"
                || key == "password";
            (sensitive_key && !value.is_null() && value != "") || json_contains_secret(value)
        }),
        Value::Array(items) => items.iter().any(json_contains_secret),
        _ => false,
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn required_string(params: &Value, key: &str) -> Result<String, Value> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be a non-empty string")}))
}

fn optional_bool(params: &Value, key: &str) -> Result<Option<bool>, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(
            json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be a boolean")}),
        ),
    }
}

fn optional_usize(
    params: &Value,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<usize>, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_u64().and_then(|value| usize::try_from(value).ok()) {
            Some(value) if value >= min && value <= max => Ok(Some(value)),
            _ => Err(
                json!({"ok": false, "code": "invalid_params", "message": format!("{key} must be an integer in {min}..={max}")}),
            ),
        },
    }
}

#[cfg(unix)]
fn open_source_error(path: &Path, error: std::io::Error) -> Value {
    if error.raw_os_error() == Some(libc::ELOOP) {
        return fail(
            "symlink_denied",
            "text transfer source must not be a symlink",
            path,
        );
    }
    io_fail("read_failed", path, error)
}

#[cfg(not(unix))]
fn open_source_error(path: &Path, error: std::io::Error) -> Value {
    io_fail("read_failed", path, error)
}

fn hardlink_denied(path: &Path) -> Value {
    json!({
        "ok": false,
        "code": "text_transfer_hardlink_denied",
        "message": "text transfer refuses files with more than one hard link; in-HOME hardlinks are over-refused by design",
        "path": path,
    })
}

fn fail(code: &str, message: &str, path: &Path) -> Value {
    json!({"ok": false, "code": code, "message": message, "path": path})
}

fn io_fail(code: &str, path: &Path, error: std::io::Error) -> Value {
    json!({"ok": false, "code": code, "message": error.to_string(), "path": path})
}

/// Deterministic seams for adversarial tests: the parent swap happens between
/// the directory walk and the publish, which no public entry point can express.
#[cfg(test)]
mod test_hooks {
    use std::path::Path;
    use std::sync::Mutex;

    type BeforeCommit = Box<dyn FnOnce(&Path, &Path) + Send>;

    static BEFORE_COMMIT: Mutex<Option<BeforeCommit>> = Mutex::new(None);
    static BACKUP_STAMP: Mutex<Option<u128>> = Mutex::new(None);

    pub(super) fn set_before_commit(hook: BeforeCommit) {
        *BEFORE_COMMIT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(hook);
    }

    pub(super) fn take_before_commit() -> Option<BeforeCommit> {
        BEFORE_COMMIT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub(super) fn pin_backup_stamp(stamp: u128) {
        *BACKUP_STAMP
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(stamp);
    }

    pub(super) fn clear_backup_stamp() {
        *BACKUP_STAMP
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    pub(super) fn pinned_backup_stamp() -> Option<u128> {
        *BACKUP_STAMP
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    /// Isolates HOME for the lifetime of one test. HOME and the mutation policy
    /// are process-global, so every test holds the crate-wide env lock.
    struct Home {
        _guard: MutexGuard<'static, ()>,
        root: PathBuf,
        previous: Option<OsString>,
    }

    impl Home {
        fn new(tag: &str) -> Self {
            let guard = crate::test_env::lock();
            let previous = std::env::var_os("HOME");
            let root = std::env::temp_dir().join(format!(
                "herdr-text-{tag}-{}-{}",
                std::process::id(),
                now_nanos()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            let root = fs::canonicalize(&root).unwrap();
            unsafe { std::env::set_var("HOME", &root) };
            // A previously panicking test may have leaked a commit hook or a
            // pinned backup stamp; both are process-global test seams.
            let _ = test_hooks::take_before_commit();
            test_hooks::clear_backup_stamp();
            Self {
                _guard: guard,
                root,
                previous,
            }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn entries(&self, relative: &str) -> usize {
            fs::read_dir(self.path(relative)).unwrap().count()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fence() -> CommitFence {
        CommitFence {
            store: Arc::new(Mutex::new(StateStore::open(":memory:").unwrap())),
            holder_id: None,
            lease_epoch: None,
            lease_token: None,
            session_bound: false,
        }
    }

    fn params(path: &str, content: &str, overwrite: bool) -> Value {
        json!({
            "path": path,
            "content": content,
            "sha256": sha256_hex(content.as_bytes()),
            "overwrite": overwrite,
        })
    }

    fn write_params(path: &str, content: &str, overwrite: bool, fence: &CommitFence) -> Value {
        write_fenced(&params(path, content, overwrite), fence)
    }

    fn assert_text(path: &Path, expected: &str) {
        assert_eq!(fs::read_to_string(path).unwrap(), expected);
    }

    #[test]
    fn text_transfer_round_trip_preserves_backup_and_checksum() {
        let home = Home::new("round-trip");
        let fence = fence();
        fs::write(home.path("source.txt"), "新版本\n").unwrap();
        fs::write(home.path("target.txt"), "旧版本\n").unwrap();
        let read_result = read(&json!({"path": "source.txt"}));
        assert_eq!(read_result["ok"], true);
        let result = write_params(
            "target.txt",
            read_result["content"].as_str().unwrap(),
            true,
            &fence,
        );
        assert_eq!(result["ok"], true, "{result}");
        assert_text(&home.path("target.txt"), "新版本\n");
        let backup = PathBuf::from(result["backup_path"].as_str().unwrap());
        assert_text(&backup, "旧版本\n");
        assert_eq!(home.entries(""), 3, "target, source and one backup");
    }

    #[test]
    fn text_transfer_rejects_integrity_and_secret_failures() {
        let result = write(&json!({
            "path": "target.txt",
            "content": "hello",
            "sha256": "wrong",
        }));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "sha256_mismatch");

        let home = Home::new("sensitive");
        fs::write(home.path("models.json"), r#"{"apiKey":"secret-value"}"#).unwrap();
        let result = read(&json!({"path": "models.json"}));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "sensitive_content_denied");
    }

    #[cfg(unix)]
    #[test]
    fn text_transfer_rejects_final_symlink_inside_home() {
        use std::os::unix::fs::symlink;

        let home = Home::new("symlink");
        let fence = fence();
        fs::write(home.path("real.txt"), "safe\n").unwrap();
        symlink(home.path("real.txt"), home.path("alias.txt")).unwrap();

        let result = read(&json!({"path": "alias.txt"}));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "symlink_denied");

        let result = write_params("alias.txt", "replacement\n", true, &fence);
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "symlink_denied");
        assert_text(&home.path("real.txt"), "safe\n");
        assert_eq!(home.entries(""), 2);
    }

    #[test]
    fn text_transfer_rejects_obvious_secret_assignment_in_plain_text() {
        let content = "password=fixture-value\n";
        let result = write(&json!({
            "path": "notes.txt",
            "content": content,
            "sha256": sha256_hex(content.as_bytes()),
        }));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "sensitive_content_denied");
    }

    #[test]
    fn unfenced_write_route_cannot_publish() {
        let home = Home::new("unfenced");
        let result = write(&params("new.txt", "hello", false));
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "text_write_requires_fenced_route");
        assert!(!home.path("new.txt").exists());
        assert_eq!(home.entries(""), 0);
    }

    #[cfg(unix)]
    #[test]
    fn parent_directory_swap_before_publish_fails_closed() {
        use std::os::unix::fs::symlink;

        let home = Home::new("swap");
        let fence = fence();
        let nested = home.path("nested");
        fs::create_dir(&nested).unwrap();
        let outside = home.root.parent().unwrap().join(format!(
            "herdr-text-swap-outside-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        fs::create_dir(&outside).unwrap();
        let outside_hook = outside.clone();
        let nested_hook = nested.clone();
        test_hooks::set_before_commit(Box::new(move |parent, _target| {
            assert_eq!(parent, nested_hook);
            let moved = parent.with_file_name("moved-parent");
            fs::rename(parent, &moved).unwrap();
            symlink(&outside_hook, parent).unwrap();
        }));

        let result = write_params("nested/new.txt", "hello", false, &fence);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "text_transfer_parent_replaced", "{result}");
        assert!(!outside.join("new.txt").exists());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(home.path("moved-parent")).unwrap().count(),
            0,
            "the temporary file must be removed through the pinned descriptor"
        );
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn pinned_parent_moved_outside_root_fails_closed() {
        let home = Home::new("escape");
        let fence = fence();
        let nested = home.path("nested");
        fs::create_dir(&nested).unwrap();
        let outside = home.root.parent().unwrap().join(format!(
            "herdr-text-escape-outside-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        fs::create_dir(&outside).unwrap();
        let outside_hook = outside.clone();
        test_hooks::set_before_commit(Box::new(move |parent, _target| {
            fs::rename(parent, outside_hook.join("nested")).unwrap();
        }));

        let result = write_params("nested/new.txt", "hello", false, &fence);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "text_transfer_parent_escaped", "{result}");
        assert!(!outside.join("nested/new.txt").exists());
        assert_eq!(
            fs::read_dir(outside.join("nested")).unwrap().count(),
            0,
            "the temporary file must be removed through the pinned descriptor"
        );
        assert!(!nested.exists());
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn existing_in_home_hardlink_is_denied() {
        let home = Home::new("hardlink");
        let fence = fence();
        fs::write(home.path("original.txt"), "original\n").unwrap();
        fs::hard_link(home.path("original.txt"), home.path("alias.txt")).unwrap();

        let result = write_params("alias.txt", "hello", true, &fence);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "text_transfer_hardlink_denied", "{result}");
        assert_text(&home.path("original.txt"), "original\n");
        assert_text(&home.path("alias.txt"), "original\n");
        assert_eq!(home.entries(""), 2);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_directory_leaves_are_denied() {
        use std::os::unix::ffi::OsStrExt;

        let home = Home::new("leaf-types");
        let fence = fence();
        let fifo = home.path("pipe.txt");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let result = write_params("pipe.txt", "hello", false, &fence);
        assert_eq!(result["code"], "path_not_file", "{result}");
        assert_eq!(read(&json!({"path": "pipe.txt"}))["code"], "path_not_file");

        fs::create_dir(home.path("directory.txt")).unwrap();
        let result = write_params("directory.txt", "hello", true, &fence);
        assert_eq!(result["code"], "path_not_file", "{result}");
        assert_eq!(
            read(&json!({"path": "directory.txt"}))["code"],
            "path_not_file"
        );
        assert_eq!(home.entries(""), 2);
    }

    #[test]
    fn overwrite_false_never_replaces() {
        let home = Home::new("no-replace");
        let fence = fence();
        let target = home.path("race.txt");
        let target_hook = target.clone();
        test_hooks::set_before_commit(Box::new(move |_parent, _target| {
            fs::write(&target_hook, "intruder").unwrap();
        }));

        let result = write_params("race.txt", "hello", false, &fence);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "target_exists", "{result}");
        assert_text(&target, "intruder");
        assert_eq!(home.entries(""), 1, "no temporary file may survive");
    }

    #[test]
    fn backup_is_created_exclusively() {
        let home = Home::new("backup");
        let fence = fence();
        fs::write(home.path("target.txt"), "original").unwrap();
        test_hooks::pin_backup_stamp(777_001);
        let would_be_backup = home.path("target.txt.herdr-backup-777001");
        fs::write(&would_be_backup, "occupied").unwrap();

        let result = write_params("target.txt", "hello", true, &fence);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "backup_failed", "{result}");
        assert_text(&home.path("target.txt"), "original");
        assert_text(&would_be_backup, "occupied");
        assert_eq!(home.entries(""), 2, "the temporary file must be removed");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = home.root.parent().unwrap().join(format!(
                "herdr-text-backup-outside-{}-{}",
                std::process::id(),
                now_nanos()
            ));
            fs::create_dir(&outside).unwrap();
            let sentinel = outside.join("sentinel.txt");
            fs::write(&sentinel, "sentinel").unwrap();
            let planted_stamp = 777_003;
            let planted_backup = home.path("target.txt.herdr-backup-777003");
            symlink(&sentinel, &planted_backup).unwrap();
            test_hooks::pin_backup_stamp(planted_stamp);
            let result = write_params("target.txt", "hello", true, &fence);
            assert_eq!(result["ok"], false, "{result}");
            assert_eq!(result["code"], "backup_failed", "{result}");
            assert_text(&sentinel, "sentinel");
            assert!(
                fs::symlink_metadata(&planted_backup)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            fs::remove_dir_all(outside).unwrap();
        }

        fs::remove_file(&would_be_backup).unwrap();
        test_hooks::pin_backup_stamp(777_002);
        let result = write_params("target.txt", "hello", true, &fence);
        assert_eq!(result["ok"], true, "{result}");
        let backup = PathBuf::from(result["backup_path"].as_str().unwrap());
        assert_eq!(backup, home.path("target.txt.herdr-backup-777002"));
        assert_text(&backup, "original");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&backup).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the backup is created with a private mode");
        }
        assert_text(&home.path("target.txt"), "hello");
        test_hooks::clear_backup_stamp();
    }

    #[test]
    fn lease_acquired_after_admission_blocks_commit() {
        let home = Home::new("lease");
        let fence = fence();
        fs::create_dir(home.path("project")).unwrap();
        let project_root = fs::canonicalize(home.path("project"))
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        {
            let mut store = fence.store.lock().unwrap();
            let outcome = store
                .acquire_mutation_lease(crate::state_store::MutationLeaseAcquireInput {
                    project_root: &project_root,
                    lane_key: None,
                    holder_id: "competing-holder",
                    lease_token: None,
                    ttl_seconds: Some(120),
                    workspace_id: None,
                    pane_id: None,
                    branch: None,
                    now_ms: now_ms() as i64,
                })
                .unwrap();
            assert!(matches!(
                outcome,
                crate::state_store::MutationLeaseAcquireOutcome::Acquired { .. }
            ));
        }

        let result = write_params("project/new.txt", "hello", false, &fence);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["code"], "single_writer_lease_conflict", "{result}");
        assert_eq!(
            result["conflict_reason"], "lease_credentials_required",
            "{result}"
        );
        assert!(!home.path("project/new.txt").exists());
        assert_eq!(home.entries("project"), 0, "no temporary file may survive");

        // True post-admission race: no lease exists on `project-raced` when
        // admission checks start, then a competing holder acquires one right
        // before the commit fence.
        fs::create_dir(home.path("project-raced")).unwrap();
        let raced_root = fs::canonicalize(home.path("project-raced"))
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let store_hook = Arc::clone(&fence.store);
        test_hooks::set_before_commit(Box::new(move |_parent, _target| {
            let mut store = store_hook.lock().unwrap();
            let outcome = store
                .acquire_mutation_lease(crate::state_store::MutationLeaseAcquireInput {
                    project_root: &raced_root,
                    lane_key: None,
                    holder_id: "raced-holder",
                    lease_token: None,
                    ttl_seconds: Some(120),
                    workspace_id: None,
                    pane_id: None,
                    branch: None,
                    now_ms: now_ms() as i64,
                })
                .unwrap();
            assert!(matches!(
                outcome,
                crate::state_store::MutationLeaseAcquireOutcome::Acquired { .. }
            ));
        }));
        let raced = write_params("project-raced/after-admission.txt", "hello", false, &fence);
        assert_eq!(raced["ok"], false, "{raced}");
        assert_eq!(raced["code"], "single_writer_lease_conflict", "{raced}");
        assert_eq!(raced["holder_id"], "raced-holder", "{raced}");
        assert!(!home.path("project-raced/after-admission.txt").exists());
        assert_eq!(
            home.entries("project-raced"),
            0,
            "raced temp file must be removed"
        );
    }
}
