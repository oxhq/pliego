/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Process containment for the production document runtime.
//!
//! Servo and its native dependencies run only in the worker process. The parent accepts a result
//! after that process has exited normally, validates its bounded receipt and private artifact tree,
//! creates a recoverable publication plan, and atomically exposes the tree. A worker crash or
//! malformed receipt therefore cannot publish the requested PDF or artifact root.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand::TryRng;
use rand::rngs::SysRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, get_current_pid};

#[cfg(windows)]
use super::session::windows_short_path_aliases;
use super::session::{
    PreparedDocumentPdf, SessionArtifacts, StagedArtifactLimitExceeded, create_private_directory,
    preflight_publication_request, promote_staged_artifacts, remove_empty_private_container,
    validate_staged_artifacts, validated_publication_target,
};
use super::supervised_artifact_contract::{
    CapturedArtifactExpectation, CapturedInputExpectation, FailedArtifactExpectation,
    validate_captured_artifact_contract, validate_failed_artifact_contract,
};
use super::{
    Command, DeferredCapturedPublication, PublicationRecoveryState, RenderError, RenderOutcome,
    RenderRequest, SESSION_CREATE_ATTEMPTS, SupervisorRenderIdentity, WorkerPublicationPaths,
    finalize_supervised_publication, output_overlaps_artifacts,
    output_overlaps_uncreated_artifacts, parse_args, preflight_supervised_publication_outcome,
    recover_supervised_publication, render_controlled_document_session_in_process,
    render_document_session_in_process, supervisor_render_identity,
};

const WORKER_MARKER_ENV: &str = "PLIEGO_INTERNAL_RENDER_WORKER_V1";
const WORKER_PARENT_PID_ENV: &str = "PLIEGO_INTERNAL_RENDER_PARENT_PID";
const WORKER_STAGE_CONTAINER_ENV: &str = "PLIEGO_INTERNAL_RENDER_STAGE_CONTAINER";
const WORKER_STAGE_ARTIFACTS_ENV: &str = "PLIEGO_INTERNAL_RENDER_STAGE_ARTIFACTS";
const WORKER_PUBLIC_ARTIFACTS_ENV: &str = "PLIEGO_INTERNAL_RENDER_PUBLIC_ARTIFACTS";
const WORKER_PUBLIC_OUTPUT_ENV: &str = "PLIEGO_INTERNAL_RENDER_PUBLIC_OUTPUT";
const WORKER_MANIFEST_SCHEMA: &str = "pliego.internal-render-worker";
const WORKER_FRAME_SCHEMA: &str = "pliego.internal-render-result";
const PROTOCOL_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const STAGING_PATH_NONCE_HEX_LEN: usize = 32;
const PROCESS_TEARDOWN_GRACE: Duration = Duration::from_secs(30);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(1);
#[cfg(any(unix, windows))]
const PROCESS_TREE_DRAIN_GRACE: Duration = Duration::from_secs(1);
const PRIVATE_CONTAINER_CLEANUP_ATTEMPTS: usize = 16;
const PRIVATE_CONTAINER_CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(10);

static WORKER_CONTEXT: OnceLock<WorkerContext> = OnceLock::new();
static WORKER_IDENTITY: OnceLock<SupervisorRenderIdentity> = OnceLock::new();

struct WorkerContext {
    paths: WorkerPublicationPaths,
    nonce: String,
    expected_render_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerManifest {
    schema: String,
    version: u32,
    nonce: String,
    parent_pid: u32,
    paths_sha256: String,
    controlled: bool,
    render_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkerFrame {
    schema: String,
    version: u32,
    nonce: String,
    #[serde(flatten)]
    result: WorkerResult,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum WorkerResult {
    Captured {
        deferred: DeferredCapturedPublication,
    },
    Failed {
        error: WireRenderError,
        evidence: FailureEvidenceDisposition,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum FailureEvidenceDisposition {
    None,
    Staged,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WireRenderError {
    code: String,
    message: String,
    exit_code: u8,
    artifacts: Option<String>,
    document_pdf: Option<String>,
    render_id: Option<String>,
    warnings: Vec<String>,
}

impl WireRenderError {
    fn from_error(error: RenderError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            exit_code: error.exit_code,
            artifacts: error
                .artifacts
                .map(|path| path.to_string_lossy().into_owned()),
            document_pdf: error
                .document_pdf
                .map(|path| path.to_string_lossy().into_owned()),
            render_id: error.render_id,
            warnings: error.warnings,
        }
    }

    fn into_trusted_error(
        self,
        paths: &WorkerPublicationPaths,
        identity: &SupervisorRenderIdentity,
    ) -> Result<RenderError, ()> {
        let has_complete_publication =
            self.artifacts.is_some() && self.document_pdf.is_some() && self.render_id.is_some();
        let has_no_publication =
            self.artifacts.is_none() && self.document_pdf.is_none() && self.render_id.is_none();
        if (!has_complete_publication && !has_no_publication) ||
            !matches!(self.exit_code, 1 | 2) ||
            (self.exit_code == 1 && !has_complete_publication) ||
            (self.exit_code == 2 && !has_no_publication)
        {
            return Err(());
        }
        if has_complete_publication &&
            (self.artifacts.as_deref() !=
                Some(paths.public_artifacts.to_string_lossy().as_ref()) ||
                self.document_pdf.as_deref() !=
                    Some(paths.public_output.to_string_lossy().as_ref()) ||
                self.render_id.as_deref() != Some(identity.render_id.as_str()))
        {
            return Err(());
        }
        Ok(RenderError {
            code: self.code,
            message: self.message,
            exit_code: self.exit_code,
            artifacts: has_complete_publication.then(|| paths.public_artifacts.clone()),
            document_pdf: has_complete_publication.then(|| paths.public_output.clone()),
            render_id: has_complete_publication.then(|| identity.render_id.clone()),
            warnings: self.warnings,
        })
    }
}

struct BoundedOutput {
    bytes: Vec<u8>,
    overflowed: bool,
}

struct ChildResult {
    status: ExitStatus,
    stdout: BoundedOutput,
    stderr: BoundedOutput,
    timed_out: bool,
}

#[cfg(unix)]
struct ChildContainment {
    process_group: libc::pid_t,
    termination_sent: bool,
    quiesced: bool,
}

#[cfg(unix)]
impl ChildContainment {
    fn configure(command: &mut ProcessCommand) {
        use std::os::unix::process::CommandExt;

        command.process_group(0);
    }

    fn resume(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn bind(child: &Child) -> io::Result<Self> {
        let process_group = libc::pid_t::try_from(child.id()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "internal worker process ID is outside pid_t",
            )
        })?;
        Ok(Self {
            process_group,
            termination_sent: false,
            quiesced: false,
        })
    }

    fn terminate(&mut self) -> io::Result<()> {
        if self.termination_sent {
            return Ok(());
        }
        // Record the bounded numeric-PGID termination operation before making it. Cleanup must
        // never start a new signal operation after the leader is reaped and the number can be
        // reused by an unrelated process group.
        self.termination_sent = true;
        #[cfg(target_os = "macos")]
        {
            self.terminate_macos()
        }

        #[cfg(not(target_os = "macos"))]
        {
            let result = unsafe { libc::kill(-self.process_group, libc::SIGKILL) };
            if result == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            Err(error)
        }
    }

    #[cfg(target_os = "macos")]
    fn terminate_macos(&self) -> io::Result<()> {
        let deadline = Instant::now()
            .checked_add(PROCESS_TREE_DRAIN_GRACE)
            .unwrap_or_else(Instant::now);
        loop {
            let result = unsafe { libc::kill(-self.process_group, libc::SIGKILL) };
            if result == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if error.raw_os_error() != Some(libc::EPERM) {
                return Err(error);
            }

            // Darwin's POSIX killpg implementation can report EPERM while a member is
            // transitioning after the group leader exits, and also reports EPERM for a
            // zombie-only group because zombies are excluded from signal delivery. The unreaped
            // leader still reserves this PGID throughout this bounded operation, so a retry cannot
            // target a reused group. Accept only a proven zombie-only group; an orphan that is
            // temporarily hidden by MAC process-info policy remains untrusted until it disappears.
            match macos_process_group_observation(self.process_group).map_err(|inspection_error| {
                io::Error::new(
                    inspection_error.kind(),
                    format!(
                        "Darwin process-group inspection failed after termination was denied: {inspection_error}"
                    ),
                )
            })? {
                MacosProcessGroupObservation::ZombieOnly => return Ok(()),
                MacosProcessGroupObservation::Live if Instant::now() >= deadline => {
                    return Err(error);
                },
                MacosProcessGroupObservation::TemporarilyUnobservable
                    if Instant::now() >= deadline =>
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Darwin process group remained unobservable after termination was denied",
                    ));
                },
                MacosProcessGroupObservation::Live |
                MacosProcessGroupObservation::TemporarilyUnobservable => {
                    thread::sleep(PROCESS_POLL_INTERVAL);
                },
            }
        }
    }

    fn quiesce(&mut self) -> io::Result<()> {
        if self.quiesced {
            return Ok(());
        }
        // The group leader must still reserve this numeric PGID when `terminate` first runs.
        // Normal waiting uses waitid(WNOWAIT), sends SIGKILL to the held group, and only then
        // reaps the leader. Once termination has been sent this method only observes drainage;
        // it never signals a potentially reused PGID.
        self.terminate()?;
        let deadline = Instant::now()
            .checked_add(PROCESS_TREE_DRAIN_GRACE)
            .unwrap_or_else(Instant::now);
        loop {
            #[cfg(target_os = "macos")]
            let macos_observation =
                macos_process_group_observation(self.process_group).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("Darwin process-group drainage inspection failed: {error}"),
                    )
                })?;
            #[cfg(target_os = "macos")]
            if macos_observation == MacosProcessGroupObservation::ZombieOnly {
                self.quiesced = true;
                return Ok(());
            }
            let probe = unsafe { libc::kill(-self.process_group, 0) };
            if probe != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    self.quiesced = true;
                    return Ok(());
                }
                #[cfg(target_os = "macos")]
                if error.raw_os_error() == Some(libc::EPERM) {
                    // A member can transition between the read-only inspection and this probe.
                    // Recheck, but never treat an unobservable group as drained. The held leader
                    // reserves the PGID throughout this bounded observation loop.
                    let observation = macos_process_group_observation(self.process_group)
                        .map_err(|inspection_error| {
                            io::Error::new(
                                inspection_error.kind(),
                                format!(
                                    "Darwin process-group drainage inspection failed after the group probe was denied: {inspection_error}"
                                ),
                            )
                        })?;
                    if observation == MacosProcessGroupObservation::ZombieOnly {
                        self.quiesced = true;
                        return Ok(());
                    }
                    if Instant::now() >= deadline {
                        return if observation ==
                            MacosProcessGroupObservation::TemporarilyUnobservable
                        {
                            Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "Darwin process group remained unobservable during drainage",
                            ))
                        } else {
                            Err(error)
                        };
                    }
                    thread::sleep(PROCESS_POLL_INTERVAL);
                    continue;
                }
                return Err(error);
            }
            #[cfg(target_os = "linux")]
            if linux_process_group_has_only_zombies(self.process_group)? {
                self.quiesced = true;
                return Ok(());
            }
            if Instant::now() >= deadline {
                #[cfg(target_os = "macos")]
                if macos_observation == MacosProcessGroupObservation::TemporarilyUnobservable {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Darwin process group remained unobservable during drainage",
                    ));
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "internal worker process group did not terminate",
                ));
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacosProcessGroupObservation {
    ZombieOnly,
    Live,
    TemporarilyUnobservable,
}

#[cfg(target_os = "macos")]
fn macos_process_group_observation(
    process_group: libc::pid_t,
) -> io::Result<MacosProcessGroupObservation> {
    macos_process_group_observation_from_result(macos_process_group_has_only_zombies(process_group))
}

#[cfg(target_os = "macos")]
fn macos_process_group_observation_from_result(
    result: io::Result<bool>,
) -> io::Result<MacosProcessGroupObservation> {
    match result {
        Ok(true) => Ok(MacosProcessGroupObservation::ZombieOnly),
        Ok(false) => Ok(MacosProcessGroupObservation::Live),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            Ok(MacosProcessGroupObservation::TemporarilyUnobservable)
        },
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn linux_process_group_has_only_zombies(process_group: libc::pid_t) -> io::Result<bool> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(process_id) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let Some((state, observed_group)) = linux_process_state_and_group(process_id)? else {
            continue;
        };
        if observed_group == process_group && !matches!(state, 'Z' | 'X') {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(target_os = "macos")]
fn macos_process_group_has_only_zombies(process_group: libc::pid_t) -> io::Result<bool> {
    for process_id in macos_process_group_members(process_group)? {
        let Some((state, observed_group)) = macos_process_state_and_group(process_id)? else {
            continue;
        };
        // A PID can disappear and be reused between the group snapshot and its status query.
        // Ignore only a process that is no longer in the held group; any live member still in the
        // group keeps the containment active.
        if observed_group == process_group && state != libc::SZOMB {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct MacosProcBsdShortInfo {
    pbsi_pid: u32,
    _pbsi_ppid: u32,
    pbsi_pgid: u32,
    pbsi_status: u32,
    _pbsi_comm: [libc::c_char; libc::MAXCOMLEN],
    _pbsi_flags: u32,
    _pbsi_uid: libc::uid_t,
    _pbsi_gid: libc::gid_t,
    _pbsi_ruid: libc::uid_t,
    _pbsi_rgid: libc::gid_t,
    _pbsi_svuid: libc::uid_t,
    _pbsi_svgid: libc::gid_t,
    _pbsi_rfu: u32,
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<MacosProcBsdShortInfo>() == 64);

#[cfg(target_os = "macos")]
// `PROC_PIDT_SHORTBSDINFO` from Apple's public proc_info.h. libc 0.2.186 does not yet expose it.
const MACOS_PROC_PIDT_SHORTBSDINFO: libc::c_int = 13;

#[cfg(target_os = "macos")]
fn macos_process_group_members(process_group: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    const LIST_ATTEMPTS: usize = 4;
    const LIST_HEADROOM: usize = 8;
    const MAX_PROCESS_GROUP_MEMBERS: usize = 4096;

    for _ in 0..LIST_ATTEMPTS {
        // SAFETY: __error returns this thread's errno location.
        unsafe { *libc::__error() = 0 };
        // SAFETY: a null buffer with zero length asks libproc for a process-list sizing bound.
        let observed = unsafe { libc::proc_listpgrppids(process_group, std::ptr::null_mut(), 0) };
        if observed < 0 {
            return Err(io::Error::last_os_error());
        }
        if observed == 0 {
            // SAFETY: __error returns this thread's errno location.
            let error = unsafe { *libc::__error() };
            return if matches!(error, 0 | libc::ESRCH) {
                Ok(Vec::new())
            } else {
                Err(io::Error::from_raw_os_error(error))
            };
        }

        // Darwin's null-buffer query reports a global process-list bound, not the selected
        // group's exact size. Cap that sizing hint; the filled-call count below still rejects an
        // actually full/ambiguous group instead of treating it as drained.
        let capacity = usize::try_from(observed)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "libproc returned an invalid process group size",
                )
            })?
            .saturating_add(LIST_HEADROOM)
            .min(MAX_PROCESS_GROUP_MEMBERS);
        let buffer_bytes = capacity
            .checked_mul(std::mem::size_of::<libc::pid_t>())
            .and_then(|bytes| libc::c_int::try_from(bytes).ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "internal worker process group buffer is too large",
                )
            })?;
        let mut members = vec![0; capacity];

        // SAFETY: members is writable for buffer_bytes and libproc returns at most that many PIDs.
        unsafe { *libc::__error() = 0 };
        let returned = unsafe {
            libc::proc_listpgrppids(process_group, members.as_mut_ptr().cast(), buffer_bytes)
        };
        if returned < 0 {
            return Err(io::Error::last_os_error());
        }
        if returned == 0 {
            // The group may disappear between the sizing and filling calls.
            // SAFETY: __error returns this thread's errno location.
            let error = unsafe { *libc::__error() };
            return if matches!(error, 0 | libc::ESRCH) {
                Ok(Vec::new())
            } else {
                Err(io::Error::from_raw_os_error(error))
            };
        }
        let returned = usize::try_from(returned).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "libproc returned an invalid process group size",
            )
        })?;
        if returned > capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "libproc exceeded the process group buffer",
            ));
        }
        if returned == capacity {
            // A full buffer is ambiguous: the group could have grown while it was inspected.
            continue;
        }
        members.truncate(returned);
        if members.iter().any(|process_id| *process_id <= 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "libproc returned an invalid process group member",
            ));
        }
        return Ok(members);
    }

    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "internal worker process group changed during bounded inspection",
    ))
}

#[cfg(target_os = "macos")]
fn macos_process_state_and_group(
    process_id: libc::pid_t,
) -> io::Result<Option<(u32, libc::pid_t)>> {
    let mut information = unsafe { std::mem::zeroed::<MacosProcBsdShortInfo>() };
    let information_bytes = libc::c_int::try_from(std::mem::size_of::<MacosProcBsdShortInfo>())
        .expect("proc_bsdshortinfo size fits c_int");
    // SAFETY: __error returns this thread's errno location.
    unsafe { *libc::__error() = 0 };
    // SAFETY: information is writable for information_bytes and has the requested layout.
    let returned = unsafe {
        libc::proc_pidinfo(
            process_id,
            // The short BSD flavor exposes exactly the PID, process-group, and state needed for
            // containment without PROC_PIDTBSDINFO's same-user policy. That fuller policy can
            // return EPERM while an already-killed group member is transitioning into a zombie.
            MACOS_PROC_PIDT_SHORTBSDINFO,
            // Darwin searches the zombie process list for BSD-info flavors only when arg is
            // nonzero. The inspected process-group snapshot can contain unreaped zombies after
            // the worker group has been terminated.
            1,
            (&raw mut information).cast(),
            information_bytes,
        )
    };
    if returned == 0 {
        // proc_pidinfo can lose a process between the group snapshot and this query. Tolerate that
        // race only when a fresh existence probe proves the PID is gone; every other libproc
        // failure remains fail-closed.
        let status_error = unsafe { *libc::__error() };
        let probe = unsafe { libc::kill(process_id, 0) };
        if probe != 0 {
            let probe_error = io::Error::last_os_error();
            if probe_error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(probe_error);
        }
        return Err(if status_error == 0 {
            io::Error::other("libproc returned no process status without an error")
        } else {
            io::Error::from_raw_os_error(status_error)
        });
    }
    if returned != information_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "libproc returned a partial process status",
        ));
    }
    let observed_id = libc::pid_t::try_from(information.pbsi_pid).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "libproc returned a process ID outside pid_t",
        )
    })?;
    if observed_id != process_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "libproc returned status for a different process",
        ));
    }
    let process_group = libc::pid_t::try_from(information.pbsi_pgid).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "libproc returned a process group outside pid_t",
        )
    })?;
    Ok(Some((information.pbsi_status, process_group)))
}

#[cfg(target_os = "linux")]
fn linux_process_state_and_group(
    process_id: libc::pid_t,
) -> io::Result<Option<(char, libc::pid_t)>> {
    let path = PathBuf::from("/proc")
        .join(process_id.to_string())
        .join("stat");
    let stat = match std::fs::read_to_string(path) {
        Ok(stat) => stat,
        Err(error) if linux_process_stat_error_is_disappearance(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let close = stat.rfind(')').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Linux process stat has no command terminator",
        )
    })?;
    let mut fields = stat[close + 1..].split_whitespace();
    let state = fields
        .next()
        .and_then(|value| value.chars().next())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Linux process stat has no state",
            )
        })?;
    let _parent = fields.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Linux process stat has no parent",
        )
    })?;
    let process_group = fields
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Linux process stat has no process group",
            )
        })?
        .parse::<libc::pid_t>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some((state, process_group)))
}

#[cfg(target_os = "linux")]
fn linux_process_stat_error_is_disappearance(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(unix)]
impl Drop for ChildContainment {
    fn drop(&mut self) {
        let _ = self.quiesce();
    }
}

#[cfg(windows)]
struct ChildContainment {
    job: std::os::windows::io::OwnedHandle,
    primary_thread: Option<std::os::windows::io::OwnedHandle>,
    quiesced: bool,
}

#[cfg(windows)]
impl ChildContainment {
    fn configure(command: &mut ProcessCommand) {
        use std::os::windows::process::CommandExt;

        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

        // `ChildExt::main_thread_handle` is still nightly-only. Keep the standard library's
        // quoting, environment, and pipe setup, then recover the only thread belonging to the
        // still-suspended process after it has entered the job.
        command.creation_flags(CREATE_SUSPENDED);
    }

    fn bind(child: &Child) -> io::Result<Self> {
        use std::mem::{size_of, zeroed};
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(job.cast()) };
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job.as_raw_handle().cast(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&information).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        let assigned = unsafe {
            AssignProcessToJobObject(job.as_raw_handle().cast(), child.as_raw_handle().cast())
        };
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }
        let primary_thread = open_only_process_thread(child.id())?;
        Ok(Self {
            job,
            primary_thread: Some(primary_thread),
            quiesced: false,
        })
    }

    fn resume(&mut self) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::System::Threading::ResumeThread;

        let primary_thread = self.primary_thread.take().ok_or_else(|| {
            io::Error::other("internal worker primary thread was already resumed")
        })?;
        let previous_suspend_count = unsafe { ResumeThread(primary_thread.as_raw_handle().cast()) };
        if previous_suspend_count == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        if previous_suspend_count != 1 {
            return Err(io::Error::other(format!(
                "internal worker primary thread had unexpected suspend count {previous_suspend_count}"
            )));
        }
        Ok(())
    }

    fn active_processes(&self) -> io::Result<u32> {
        use std::mem::{size_of, zeroed};
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
            QueryInformationJobObject,
        };

        let mut information: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        let queried = unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle().cast(),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut information).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(information.ActiveProcesses)
    }

    fn quiesce(&mut self) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        if self.quiesced {
            return Ok(());
        }
        if unsafe { TerminateJobObject(self.job.as_raw_handle().cast(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now()
            .checked_add(PROCESS_TREE_DRAIN_GRACE)
            .unwrap_or_else(Instant::now);
        loop {
            if self.active_processes()? == 0 {
                self.quiesced = true;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "internal worker process tree did not terminate",
                ));
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
    }
}

#[cfg(windows)]
impl Drop for ChildContainment {
    fn drop(&mut self) {
        let _ = self.quiesce();
    }
}

#[cfg(windows)]
fn open_only_process_thread(process_id: u32) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, THREAD_SUSPEND_RESUME};

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.cast()) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    if unsafe { Thread32First(snapshot.as_raw_handle().cast(), &mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut thread_id = None;
    loop {
        if entry.th32OwnerProcessID == process_id {
            if thread_id.replace(entry.th32ThreadID).is_some() {
                return Err(io::Error::other(
                    "suspended internal worker unexpectedly has multiple threads",
                ));
            }
        }
        if unsafe { Thread32Next(snapshot.as_raw_handle().cast(), &mut entry) } == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(io::Error::from_raw_os_error(error as i32));
        }
    }

    let thread_id = thread_id.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "suspended internal worker primary thread was not found",
        )
    })?;
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(thread.cast()) })
}

struct StagingGuard {
    container: PathBuf,
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        cleanup_empty_private_container(&self.container);
    }
}

fn cleanup_empty_private_container(container: &Path) {
    for attempt in 0..PRIVATE_CONTAINER_CLEANUP_ATTEMPTS {
        match remove_empty_private_container(container) {
            Ok(true) | Ok(false) => return,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(_) if attempt + 1 < PRIVATE_CONTAINER_CLEANUP_ATTEMPTS => {
                thread::sleep(PRIVATE_CONTAINER_CLEANUP_RETRY_DELAY);
            },
            Err(_) => return,
        }
    }
}

enum PlanAndPromoteError {
    BeforeVisibility(RenderError),
    AfterVisibility(RenderError),
}

impl PlanAndPromoteError {
    fn into_error(self) -> RenderError {
        match self {
            Self::BeforeVisibility(error) | Self::AfterVisibility(error) => error,
        }
    }
}

pub(crate) fn is_worker_process() -> bool {
    WORKER_CONTEXT.get().is_some()
}

pub(crate) fn worker_publication_paths() -> Option<&'static WorkerPublicationPaths> {
    WORKER_CONTEXT.get().map(|context| &context.paths)
}

#[derive(Debug, Eq, PartialEq)]
enum WorkerEntryDecision {
    PublicCli,
    RejectInternal,
    Run { marker: String, parent_pid: u32 },
}

fn classify_worker_entry(
    marker: Option<String>,
    parent_pid: Option<String>,
    parent_is_same_executable: impl FnOnce(u32) -> bool,
) -> WorkerEntryDecision {
    let Some(marker) = marker else {
        return WorkerEntryDecision::PublicCli;
    };
    if !valid_nonce(&marker) {
        return WorkerEntryDecision::PublicCli;
    }
    let Some(parent_pid) = parent_pid.and_then(|value| canonical_parent_pid(&value)) else {
        return WorkerEntryDecision::RejectInternal;
    };
    if !parent_is_same_executable(parent_pid) {
        return WorkerEntryDecision::RejectInternal;
    }
    WorkerEntryDecision::Run { marker, parent_pid }
}

/// Intercept an internal worker before ordinary public CLI parsing and output handling.
pub(crate) fn run_worker_from_environment() {
    match classify_worker_entry(
        std::env::var(WORKER_MARKER_ENV).ok(),
        std::env::var(WORKER_PARENT_PID_ENV).ok(),
        worker_parent_is_same_executable,
    ) {
        WorkerEntryDecision::PublicCli => {},
        WorkerEntryDecision::RejectInternal => terminate_worker_process(2),
        WorkerEntryDecision::Run { marker, parent_pid } => {
            let exit_code = run_worker(marker, parent_pid);
            terminate_worker_process(exit_code);
        },
    }
}

fn run_worker(marker: String, prechecked_parent_pid: u32) -> u8 {
    let manifest = match read_worker_manifest() {
        Ok(manifest)
            if manifest.schema == WORKER_MANIFEST_SCHEMA &&
                manifest.version == PROTOCOL_VERSION &&
                manifest.nonce == marker &&
                manifest.parent_pid == prechecked_parent_pid &&
                valid_nonce(&manifest.nonce) =>
        {
            manifest
        },
        _ => return 2,
    };
    let paths = match worker_paths_from_environment() {
        Some(paths) => paths,
        None => return 2,
    };
    if !worker_paths_match_private_container(&manifest.nonce, &paths) ||
        !worker_parent_is_same_executable(manifest.parent_pid) ||
        manifest.paths_sha256 != worker_paths_sha256(&manifest.nonce, &paths)
    {
        return 2;
    }
    if WORKER_CONTEXT
        .set(WorkerContext {
            paths,
            nonce: manifest.nonce.clone(),
            expected_render_id: manifest.render_id.clone(),
        })
        .is_err()
    {
        return 2;
    }
    let command = match parse_args(std::env::args_os().skip(1).collect()) {
        Ok(command) => command,
        Err(_) => return 2,
    };
    let request = match worker_request_for_manifest(manifest.controlled, command) {
        Some(request) => request,
        None => return 2,
    };
    let identity = match supervisor_render_identity(&request, manifest.controlled) {
        Ok(identity) if identity.render_id == manifest.render_id => identity,
        _ => return 2,
    };
    if WORKER_IDENTITY.set(identity).is_err() {
        return 2;
    }
    let result = if manifest.controlled {
        render_controlled_document_session_in_process(request)
    } else {
        render_document_session_in_process(request)
    };
    let (result, exit_code) = match result {
        Ok(outcome) => {
            let Some(value) = outcome.summary.get("internal_deferred_capture").cloned() else {
                return 2;
            };
            let Ok(deferred) = serde_json::from_value::<DeferredCapturedPublication>(value) else {
                return 2;
            };
            if deferred.render_id != manifest.render_id {
                return 2;
            }
            (WorkerResult::Captured { deferred }, 0)
        },
        Err(error) => {
            let exit_code = error.exit_code;
            let evidence = worker_failure_evidence(&error);
            (
                WorkerResult::Failed {
                    error: WireRenderError::from_error(error),
                    evidence,
                },
                exit_code,
            )
        },
    };
    if write_worker_frame(&manifest.nonce, result).is_err() {
        return 2;
    }
    exit_code
}

fn worker_request_for_manifest(controlled: bool, command: Command) -> Option<RenderRequest> {
    match (controlled, command) {
        (false, Command::Render(request)) |
        (true, Command::Render(request) | Command::RenderControlled(request)) => Some(request),
        _ => None,
    }
}

pub(crate) fn finish_captured_worker(deferred: DeferredCapturedPublication) -> ! {
    let Some(context) = WORKER_CONTEXT.get() else {
        terminate_worker_process(2);
    };
    if deferred.validate(&context.expected_render_id).is_err() ||
        write_worker_frame(&context.nonce, WorkerResult::Captured { deferred }).is_err()
    {
        terminate_worker_process(2);
    }
    terminate_worker_process(0)
}

pub(crate) fn finish_failed_worker(error: RenderError) -> ! {
    let Some(context) = WORKER_CONTEXT.get() else {
        terminate_worker_process(2);
    };
    let Some(_) = WORKER_IDENTITY.get() else {
        terminate_worker_process(2);
    };
    let exit_code = error.exit_code;
    if !matches!(exit_code, 1 | 2) {
        terminate_worker_process(2);
    }
    let evidence = worker_failure_evidence(&error);
    let result = WorkerResult::Failed {
        error: WireRenderError::from_error(error),
        evidence,
    };
    if write_worker_frame(&context.nonce, result).is_err() {
        terminate_worker_process(2);
    }
    terminate_worker_process(exit_code)
}

fn write_worker_frame(nonce: &str, result: WorkerResult) -> io::Result<()> {
    let frame = WorkerFrame {
        schema: WORKER_FRAME_SCHEMA.into(),
        version: PROTOCOL_VERSION,
        nonce: nonce.into(),
        result,
    };
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &frame).map_err(io::Error::other)?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

#[cfg(unix)]
fn terminate_worker_process(exit_code: u8) -> ! {
    // The authenticated worker has already flushed its only accepted frame. `_exit` releases the
    // process without running native teardown that may retain detached Servo/runtime threads.
    unsafe { libc::_exit(i32::from(exit_code)) }
}

#[cfg(windows)]
fn terminate_worker_process(exit_code: u8) -> ! {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

    // ExitProcess runs DLL detach callbacks and can deadlock after terminating a thread that held a
    // native-runtime lock. The authenticated frame is already flushed, so terminate the worker
    // without running process detach. The parent Job remains the process-tree authority.
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), u32::from(exit_code));
    }
    loop {
        std::hint::spin_loop();
    }
}

#[cfg(not(any(unix, windows)))]
fn terminate_worker_process(exit_code: u8) -> ! {
    std::process::exit(i32::from(exit_code))
}

fn worker_failure_evidence(error: &RenderError) -> FailureEvidenceDisposition {
    if error.exit_code != 1 {
        return FailureEvidenceDisposition::None;
    }
    let Some(paths) = WORKER_CONTEXT.get().map(|context| &context.paths) else {
        return FailureEvidenceDisposition::None;
    };
    let required_files = [
        "console.jsonl",
        "failure.json",
        "resources.jsonl",
        "session-state.jsonl",
    ];
    let has_required_files = required_files.iter().all(|name| {
        std::fs::symlink_metadata(paths.staging_artifacts.join(name))
            .is_ok_and(|metadata| metadata.is_file())
    });
    let has_resource_directory =
        std::fs::symlink_metadata(paths.staging_artifacts.join("resources"))
            .is_ok_and(|metadata| metadata.is_dir());
    if has_required_files && has_resource_directory {
        FailureEvidenceDisposition::Staged
    } else {
        FailureEvidenceDisposition::None
    }
}

fn captured_input_expectation(identity: &SupervisorRenderIdentity) -> CapturedInputExpectation<'_> {
    CapturedInputExpectation {
        url: identity.expected_input.url.as_str(),
        sha256: &identity.expected_input.sha256,
        resource: &identity.expected_input.content_address,
        bytes: identity.expected_input.bytes,
    }
}

fn failed_artifact_expectation<'a>(
    identity: &'a SupervisorRenderIdentity,
    render_id: &'a str,
    code: &'a str,
    message: &'a str,
    public_output: &'a Path,
) -> FailedArtifactExpectation<'a> {
    FailedArtifactExpectation {
        render_id,
        code,
        message,
        public_output,
        locale: identity.locale,
        timezone: identity.timezone,
        page: &identity.page,
        resource_policy: &identity.resource_policy,
        input: captured_input_expectation(identity),
        allow_host_fonts: identity.allow_host_fonts,
    }
}

fn read_worker_manifest() -> io::Result<WorkerManifest> {
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES ||
        bytes.last() != Some(&b'\n') ||
        bytes[..bytes.len().saturating_sub(1)].contains(&b'\n')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid internal worker manifest framing",
        ));
    }
    let encoded = &bytes[..bytes.len() - 1];
    let manifest: WorkerManifest = serde_json::from_slice(encoded).map_err(io::Error::other)?;
    if serde_json::to_vec(&manifest).map_err(io::Error::other)? != encoded {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "internal worker manifest is not canonically encoded",
        ));
    }
    Ok(manifest)
}

fn worker_paths_from_environment() -> Option<WorkerPublicationPaths> {
    Some(WorkerPublicationPaths {
        staging_container: PathBuf::from(std::env::var_os(WORKER_STAGE_CONTAINER_ENV)?),
        staging_artifacts: PathBuf::from(std::env::var_os(WORKER_STAGE_ARTIFACTS_ENV)?),
        public_artifacts: PathBuf::from(std::env::var_os(WORKER_PUBLIC_ARTIFACTS_ENV)?),
        public_output: PathBuf::from(std::env::var_os(WORKER_PUBLIC_OUTPUT_ENV)?),
    })
}

fn worker_parent_is_same_executable(expected_parent_pid: u32) -> bool {
    let Ok(current_pid) = get_current_pid() else {
        return false;
    };
    let parent_pid = Pid::from_u32(expected_parent_pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[current_pid, parent_pid]),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let Some(current) = system.process(current_pid) else {
        return false;
    };
    if current.parent() != Some(parent_pid) {
        return false;
    }
    let Some(parent_executable) = system.process(parent_pid).and_then(|process| process.exe())
    else {
        return false;
    };
    let Ok(current_executable) = std::env::current_exe() else {
        return false;
    };
    same_file::is_same_file(parent_executable, current_executable).unwrap_or(false)
}

fn worker_paths_match_private_container(nonce: &str, paths: &WorkerPublicationPaths) -> bool {
    let Some(expected_container) = staging_container_leaf(nonce) else {
        return false;
    };
    paths.staging_container.file_name() == Some(expected_container.as_os_str()) &&
        paths.staging_artifacts.parent() == Some(paths.staging_container.as_path()) &&
        paths.staging_artifacts.file_name() == Some(std::ffi::OsStr::new("artifacts")) &&
        !paths.public_artifacts.starts_with(&paths.staging_container) &&
        !paths.public_output.starts_with(&paths.staging_container)
}

fn staging_container_leaf(nonce: &str) -> Option<std::ffi::OsString> {
    let path_nonce = nonce.get(..STAGING_PATH_NONCE_HEX_LEN)?;
    if !path_nonce
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    Some(std::ffi::OsString::from(format!(
        ".pliego-runtime-{path_nonce}"
    )))
}

fn worker_paths_sha256(nonce: &str, paths: &WorkerPublicationPaths) -> String {
    let mut hasher = Sha256::new();
    update_capability_hash(&mut hasher, b"pliego.internal-render-paths.v1");
    update_capability_hash(&mut hasher, nonce.as_bytes());
    for path in [
        &paths.staging_container,
        &paths.staging_artifacts,
        &paths.public_artifacts,
        &paths.public_output,
    ] {
        update_capability_hash(&mut hasher, path.as_os_str().as_encoded_bytes());
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(7 + digest.len() * 2);
    encoded.push_str("sha256:");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn update_capability_hash(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[cfg(unix)]
fn require_waitable_child_processes() -> io::Result<()> {
    let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if !sigchld_disposition_is_waitable(&action) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "document runtime supervision requires the default waitable SIGCHLD disposition",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn sigchld_disposition_is_waitable(action: &libc::sigaction) -> bool {
    action.sa_sigaction == libc::SIG_DFL && action.sa_flags & libc::SA_NOCLDWAIT == 0
}

#[cfg(windows)]
fn require_waitable_child_processes() -> io::Result<()> {
    Ok(())
}

pub(crate) fn render(
    request: RenderRequest,
    controlled: bool,
) -> Result<RenderOutcome, RenderError> {
    let identity = supervisor_render_identity(&request, controlled)?;
    let resource_setup_failed = identity.resolved_resource_policy.setup_failure().is_some();
    let nonce = generate_nonce().map_err(|_| {
        RenderError::without_publication(
            "RUNTIME_TERMINATED",
            "document runtime supervisor could not create a process capability",
            1,
        )
    })?;
    let paths = publication_paths(&request, &nonce, &identity)?;
    let existing_artifacts = bind_existing_artifact_root(&request, &paths, &identity)?;
    let _staging_guard = if existing_artifacts.is_none() {
        create_private_directory(&paths.staging_container)
            .map_err(|error| artifact_creation_error(&request, &paths, &identity, error))?;
        let guard = StagingGuard {
            container: paths.staging_container.clone(),
        };
        validate_requested_artifact_leaf(&request, &paths, &identity)?;
        Some(guard)
    } else {
        None
    };
    preflight_request_paths(&request, &paths, &identity, existing_artifacts.is_some())?;
    if let Some(artifacts) = existing_artifacts {
        return recover_supervised_publication(artifacts, &paths, &identity);
    }
    if !resource_setup_failed {
        preflight_new_publication(&request, &paths, &identity)?;
    }
    let child = run_child(&request, controlled, &nonce, &paths, &identity);
    let child = match child {
        Ok(child) => child,
        Err(_) => return Err(runtime_terminated(&paths, &identity)),
    };
    let frame = match trusted_frame(child, &nonce, &paths) {
        Ok(frame) => frame,
        Err(()) => return Err(runtime_terminated(&paths, &identity)),
    };
    match frame.result {
        WorkerResult::Captured { deferred } => {
            if resource_setup_failed {
                preflight_new_publication(&request, &paths, &identity)?;
            }
            deferred
                .validate(&identity.render_id)
                .map_err(|_| runtime_terminated(&paths, &identity))?;
            validate_staging_closure(&paths, &identity)?;
            let expected = CapturedArtifactExpectation {
                public_artifacts: &paths.public_artifacts,
                public_output: &paths.public_output,
                locale: identity.locale,
                timezone: identity.timezone,
                page: &identity.page,
                resource_policy: &identity.resource_policy,
                input: captured_input_expectation(&identity),
                allow_partial_scene: request.allow_partial_scene,
                allow_host_fonts: request.allow_host_fonts,
            };
            validate_captured_artifact_contract(&paths.staging_artifacts, &deferred, expected)
                .map_err(|_| runtime_terminated(&paths, &identity))?;
            bind_parent_resource_policy(&paths, &identity, true)?;
            validate_staging_closure(&paths, &identity)?;
            validate_captured_artifact_contract(&paths.staging_artifacts, &deferred, expected)
                .map_err(|_| runtime_terminated(&paths, &identity))?;
            require_supervised_finalization_access(&paths, &identity)?;
            preflight_supervised_publication_outcome(&request, &paths, &identity, &deferred)?;
            let prepared_output = request
                .explicit_paths
                .is_some()
                .then(|| prepare_supervised_output(&paths, &identity.render_id))
                .transpose()?;
            plan_and_promote(&paths, &identity).map_err(PlanAndPromoteError::into_error)?;
            finalize_supervised_publication(request, &paths, &identity, deferred, prepared_output)
        },
        WorkerResult::Failed { error, evidence } => {
            let error = error
                .into_trusted_error(&paths, &identity)
                .map_err(|()| runtime_terminated(&paths, &identity))?;
            if error.exit_code == 2 && !matches!(evidence, FailureEvidenceDisposition::None) {
                return Err(runtime_terminated(&paths, &identity));
            }
            if error.exit_code == 1 && matches!(evidence, FailureEvidenceDisposition::Staged) {
                validate_staging_closure(&paths, &identity)?;
                let expected = failed_artifact_expectation(
                    &identity,
                    &identity.render_id,
                    &error.code,
                    &error.message,
                    &paths.public_output,
                );
                validate_failed_artifact_contract(&paths.staging_artifacts, expected)
                    .map_err(|_| runtime_terminated(&paths, &identity))?;
                bind_parent_resource_policy(&paths, &identity, false)?;
                validate_staging_closure(&paths, &identity)?;
                validate_failed_artifact_contract(&paths.staging_artifacts, expected)
                    .map_err(|_| runtime_terminated(&paths, &identity))?;
                let promotion = if resource_setup_failed {
                    promote_failure_without_plan(&paths, &identity)
                } else {
                    plan_and_promote(&paths, &identity)
                };
                match promotion {
                    Ok(()) => {},
                    Err(PlanAndPromoteError::BeforeVisibility(_)) => return Err(error),
                    Err(PlanAndPromoteError::AfterVisibility(error)) => return Err(error),
                }
            }
            Err(error)
        },
    }
}

fn promote_failure_without_plan(
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<(), PlanAndPromoteError> {
    validate_staging_closure(paths, identity).map_err(PlanAndPromoteError::BeforeVisibility)?;
    promote_staged_artifacts(
        &paths.staging_container,
        &paths.staging_artifacts,
        &paths.public_artifacts,
    )
    .map_err(|error| {
        PlanAndPromoteError::AfterVisibility(staged_artifact_error(error, paths, identity))
    })?;
    cleanup_empty_private_container(&paths.staging_container);
    Ok(())
}

fn validate_requested_artifact_leaf(
    request: &RenderRequest,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<(), RenderError> {
    let leaf = paths.public_artifacts.file_name().ok_or_else(|| {
        artifact_creation_error(
            request,
            paths,
            identity,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "session artifact path has no final component",
            ),
        )
    })?;
    let probe_root = paths.staging_container.join("target-name-probe");
    create_private_directory(&probe_root).map_err(|_| runtime_terminated(paths, identity))?;
    let probe = probe_root.join(leaf);
    if let Err(error) = create_private_directory(&probe) {
        let _ = remove_empty_private_container(&probe_root);
        return Err(artifact_creation_error(request, paths, identity, error));
    }
    let resources = probe.join("resources");
    if let Err(error) = create_private_directory(&resources) {
        let _ = remove_empty_private_container(&probe);
        let _ = remove_empty_private_container(&probe_root);
        return Err(artifact_headroom_error(request, paths, identity, error));
    }
    let digest = resources.join("0".repeat(64));
    if let Err(error) = create_private_directory(&digest) {
        let _ = remove_empty_private_container(&resources);
        let _ = remove_empty_private_container(&probe);
        let _ = remove_empty_private_container(&probe_root);
        return Err(artifact_headroom_error(request, paths, identity, error));
    }
    let removed_digest =
        remove_empty_private_container(&digest).map_err(|_| runtime_terminated(paths, identity))?;
    let removed_resources = remove_empty_private_container(&resources)
        .map_err(|_| runtime_terminated(paths, identity))?;
    let removed_probe =
        remove_empty_private_container(&probe).map_err(|_| runtime_terminated(paths, identity))?;
    let removed_root = remove_empty_private_container(&probe_root)
        .map_err(|_| runtime_terminated(paths, identity))?;
    if !removed_digest || !removed_resources || !removed_probe || !removed_root {
        return Err(runtime_terminated(paths, identity));
    }
    Ok(())
}

fn artifact_headroom_error(
    request: &RenderRequest,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
    error: io::Error,
) -> RenderError {
    let (artifacts, output) = requested_publication_error_paths(request, paths);
    RenderError::session(
        artifacts,
        output,
        &identity.render_id,
        "ARTIFACTS_CREATE_FAILED",
        format!(
            "cannot prepare the bounded session artifact tree at {}: {error}",
            artifacts.display()
        ),
    )
}

fn artifact_creation_error(
    request: &RenderRequest,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
    error: io::Error,
) -> RenderError {
    let error = public_artifact_creation_error_detail(paths, error);
    if let Some(explicit) = &request.explicit_paths {
        return RenderError::session(
            &explicit.artifacts,
            &explicit.output,
            &identity.render_id,
            "ARTIFACTS_CREATE_FAILED",
            format!(
                "cannot create exclusive artifact directory {}: {error}",
                explicit.artifacts.display()
            ),
        );
    }
    RenderError::session(
        &paths.public_artifacts,
        &paths.public_output,
        &identity.render_id,
        "ARTIFACTS_CREATE_FAILED",
        format!("cannot create session artifacts: {error}"),
    )
}

fn public_artifact_creation_error_detail(
    paths: &WorkerPublicationPaths,
    error: io::Error,
) -> String {
    let message = error.to_string();
    let mut private_fragments = Vec::new();
    for path in [&paths.staging_container, &paths.staging_artifacts] {
        append_private_path_fragment(&mut private_fragments, &path.to_string_lossy());
        if let Some(leaf) = path.file_name() {
            let leaf = leaf.to_string_lossy();
            if private_leaf_token(&leaf) {
                private_fragments.push(leaf.into_owned());
            }
        }
    }
    if private_fragments
        .iter()
        .any(|fragment| string_contains_private_fragment(&message, fragment))
    {
        "private artifact staging setup was rejected".to_owned()
    } else {
        message
    }
}

fn bind_existing_artifact_root(
    request: &RenderRequest,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<Option<SessionArtifacts>, RenderError> {
    match std::fs::symlink_metadata(&paths.public_artifacts) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(artifact_creation_error(request, paths, identity, error)),
        Ok(_) => {},
    }
    SessionArtifacts::open_for_publication_recovery(
        &paths.public_artifacts,
        identity.render_id.clone(),
    )
    .map(Some)
    .map_err(|error| {
        let (artifacts, output) = requested_publication_error_paths(request, paths);
        RenderError::session(
            artifacts,
            output,
            &identity.render_id,
            "PUBLICATION_RECOVERY_FAILED",
            format!("existing artifact root cannot be opened for publication recovery: {error}"),
        )
    })
}

fn requested_publication_error_paths<'a>(
    request: &'a RenderRequest,
    paths: &'a WorkerPublicationPaths,
) -> (&'a Path, &'a Path) {
    request
        .explicit_paths
        .as_ref()
        .map(|explicit| (explicit.artifacts.as_path(), explicit.output.as_path()))
        .unwrap_or((
            paths.public_artifacts.as_path(),
            paths.public_output.as_path(),
        ))
}

fn publication_paths(
    request: &RenderRequest,
    nonce: &str,
    identity: &SupervisorRenderIdentity,
) -> Result<WorkerPublicationPaths, RenderError> {
    let (public_artifacts, public_output) = if let Some(explicit) = &request.explicit_paths {
        let public_artifacts =
            validated_publication_target(&explicit.artifacts).map_err(|error| {
                RenderError::session(
                    &explicit.artifacts,
                    &explicit.output,
                    &identity.render_id,
                    "ARTIFACTS_CREATE_FAILED",
                    format!(
                        "cannot create exclusive artifact directory {}: {error}",
                        explicit.artifacts.display()
                    ),
                )
            })?;
        (public_artifacts, explicit.output.clone())
    } else {
        let temporary_root = std::env::temp_dir().canonicalize().map_err(|error| {
            RenderError::request(
                "ARTIFACTS_CREATE_FAILED",
                format!("cannot resolve the system temporary directory: {error}"),
            )
        })?;
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let base = temporary_root.join(format!("pliego-session-{}-{unique}", std::process::id()));
        let public_artifacts = select_shorthand_publication_root(&base).map_err(|error| {
            RenderError::session(
                &base,
                &base.join("document.pdf"),
                &identity.render_id,
                "ARTIFACTS_CREATE_FAILED",
                format!("cannot create session artifacts: {error}"),
            )
        })?;
        let public_output = public_artifacts.join("document.pdf");
        (public_artifacts, public_output)
    };
    let parent = public_artifacts.parent().ok_or_else(|| {
        RenderError::session(
            &public_artifacts,
            &public_output,
            &identity.render_id,
            "ARTIFACTS_CREATE_FAILED",
            "artifact directory has no parent",
        )
    })?;
    let staging_container = parent.join(staging_container_leaf(nonce).ok_or_else(|| {
        RenderError::without_publication(
            "RUNTIME_TERMINATED",
            "document runtime supervisor received an invalid process capability",
            1,
        )
    })?);
    let staging_artifacts = staging_container.join("artifacts");
    Ok(WorkerPublicationPaths {
        staging_container,
        staging_artifacts,
        public_artifacts,
        public_output,
    })
}

fn select_shorthand_publication_root(base: &Path) -> io::Result<PathBuf> {
    let file_name = base.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "session artifact path has no final component",
        )
    })?;
    for attempt in 0..SESSION_CREATE_ATTEMPTS {
        let candidate = if attempt == 0 {
            base.to_owned()
        } else {
            let mut retry_name = file_name.to_os_string();
            retry_name.push(format!("-{attempt}"));
            base.with_file_name(retry_name)
        };
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => {},
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "all {SESSION_CREATE_ATTEMPTS} session artifact IDs already exist for {}",
            base.display()
        ),
    ))
}

fn preflight_request_paths(
    request: &RenderRequest,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
    artifacts_exist: bool,
) -> Result<(), RenderError> {
    if request.explicit_paths.is_some() {
        let overlap = if artifacts_exist {
            output_overlaps_artifacts(&paths.public_output, &paths.public_artifacts)
        } else {
            output_overlaps_uncreated_artifacts(
                &paths.public_output,
                &paths.public_artifacts,
                &paths.staging_container,
            )
        };
        match overlap {
            Ok(false) => {},
            Ok(true) => {
                return Err(RenderError::session(
                    &paths.public_artifacts,
                    &paths.public_output,
                    &identity.render_id,
                    "OUTPUT_ARTIFACTS_OVERLAP",
                    "requested output must be outside the artifact directory",
                ));
            },
            Err(error) => {
                return Err(RenderError::session(
                    &paths.public_artifacts,
                    &paths.public_output,
                    &identity.render_id,
                    "OUTPUT_PATH_CHECK_FAILED",
                    format!("cannot compare output and artifact paths: {error}"),
                ));
            },
        }
    }
    Ok(())
}

fn preflight_new_publication(
    request: &RenderRequest,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<(), RenderError> {
    if request.explicit_paths.is_some() {
        let physical_output = std::path::absolute(&paths.public_output).map_err(|error| {
            RenderError::session(
                &paths.public_artifacts,
                &paths.public_output,
                &identity.render_id,
                "OUTPUT_PATH_CHECK_FAILED",
                format!(
                    "cannot check requested output {}: {error}",
                    paths.public_output.display()
                ),
            )
        })?;
        match std::fs::symlink_metadata(physical_output) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Ok(_) => {
                return Err(RenderError::session(
                    &paths.public_artifacts,
                    &paths.public_output,
                    &identity.render_id,
                    "OUTPUT_ALREADY_EXISTS",
                    format!(
                        "requested output already exists: {}",
                        paths.public_output.display()
                    ),
                ));
            },
            Err(error) => {
                return Err(RenderError::session(
                    &paths.public_artifacts,
                    &paths.public_output,
                    &identity.render_id,
                    "OUTPUT_PATH_CHECK_FAILED",
                    format!(
                        "cannot check requested output {}: {error}",
                        paths.public_output.display()
                    ),
                ));
            },
        }
    }
    preflight_publication_request(&paths.public_artifacts, &paths.public_output).map_err(|error| {
        RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "PUBLICATION_TRANSACTION_FAILED",
            format!("cannot begin publication transaction: {error}"),
        )
    })
}

fn prepare_supervised_output(
    paths: &WorkerPublicationPaths,
    render_id: &str,
) -> Result<PreparedDocumentPdf, RenderError> {
    let artifacts = SessionArtifacts::open_staged_for_publication(
        &paths.staging_artifacts,
        &paths.public_artifacts,
        render_id,
    )
    .map_err(|_| runtime_terminated_for_render_id(paths, render_id))?;
    artifacts
        .prepare_document_pdf(&paths.public_output)
        .map_err(|error| {
            let error_message = error.to_string();
            let contains_private_path = match private_path_fragments(paths) {
                Some(fragments) => fragments
                    .iter()
                    .any(|fragment| string_contains_private_fragment(&error_message, fragment)),
                None => true,
            };
            if contains_private_path {
                return runtime_terminated_for_render_id(paths, render_id);
            }
            let code = if error.kind() == io::ErrorKind::AlreadyExists {
                "OUTPUT_ALREADY_EXISTS"
            } else {
                "OUTPUT_PUBLISH_FAILED"
            };
            RenderError::session(
                &paths.public_artifacts,
                &paths.public_output,
                render_id,
                code,
                format!(
                    "cannot prepare requested output {}: {error}",
                    paths.public_output.display()
                ),
            )
        })
}

fn run_child(
    request: &RenderRequest,
    controlled: bool,
    nonce: &str,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> io::Result<ChildResult> {
    // Unix waitid(WNOWAIT) can reserve the worker PID/PGID only while SIGCHLD remains waitable.
    // Reject inherited auto-reap/custom-handler policy before a process group exists.
    require_waitable_child_processes()?;
    let manifest = WorkerManifest {
        schema: WORKER_MANIFEST_SCHEMA.into(),
        version: PROTOCOL_VERSION,
        nonce: nonce.into(),
        parent_pid: std::process::id(),
        paths_sha256: worker_paths_sha256(nonce, paths),
        controlled,
        render_id: identity.render_id.clone(),
    };
    let mut manifest_bytes = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
    manifest_bytes.push(b'\n');
    let executable = std::env::current_exe()?;
    let mut command = ProcessCommand::new(executable);
    command
        .args(std::env::args_os().skip(1))
        .env(WORKER_MARKER_ENV, nonce)
        .env(WORKER_PARENT_PID_ENV, manifest.parent_pid.to_string())
        .env(WORKER_STAGE_CONTAINER_ENV, &paths.staging_container)
        .env(WORKER_STAGE_ARTIFACTS_ENV, &paths.staging_artifacts)
        .env(WORKER_PUBLIC_ARTIFACTS_ENV, &paths.public_artifacts)
        .env(WORKER_PUBLIC_OUTPUT_ENV, &paths.public_output)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    // Headless Mesa otherwise probes unavailable DRI/Zink devices and writes native warnings to
    // the worker's authenticated stderr channel. Software GL keeps that channel pristine.
    command.env("LIBGL_ALWAYS_SOFTWARE", "1");
    ChildContainment::configure(&mut command);
    let mut child = command.spawn()?;
    let mut containment = match ChildContainment::bind(&child) {
        Ok(containment) => containment,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        },
    };
    let stdout_pipe = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_and_reap(&mut child, &mut containment);
            return Err(io::Error::other(
                "internal worker stdout pipe is unavailable",
            ));
        },
    };
    let stderr_pipe = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_and_reap(&mut child, &mut containment);
            return Err(io::Error::other(
                "internal worker stderr pipe is unavailable",
            ));
        },
    };
    let stdout = spawn_bounded_reader(stdout_pipe);
    let stderr = spawn_bounded_reader(stderr_pipe);
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("internal worker stdin pipe is unavailable"))
        .and_then(|mut stdin| stdin.write_all(&manifest_bytes));
    if let Err(error) = write_result {
        terminate_and_reap(&mut child, &mut containment);
        return Err(error);
    }
    if let Err(error) = containment.resume() {
        terminate_and_reap(&mut child, &mut containment);
        return Err(error);
    }
    let deadline = Instant::now()
        .checked_add(
            request
                .runtime_policy
                .host_wall_duration()
                .saturating_add(PROCESS_TEARDOWN_GRACE),
        )
        .unwrap_or_else(Instant::now);
    let (status, timed_out) = wait_for_child(&mut child, &mut containment, deadline)?;
    containment.quiesce()?;
    let pipe_deadline = Instant::now()
        .checked_add(PIPE_DRAIN_GRACE)
        .unwrap_or_else(Instant::now);
    let stdout = receive_reader(stdout, pipe_deadline)?;
    let stderr = receive_reader(stderr, pipe_deadline)?;
    Ok(ChildResult {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

#[cfg(unix)]
fn terminate_and_reap(child: &mut Child, containment: &mut ChildContainment) {
    if containment
        .terminate()
        .and_then(|()| containment.quiesce())
        .is_err()
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(windows)]
fn terminate_and_reap(child: &mut Child, containment: &mut ChildContainment) {
    let _ = containment.quiesce();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn wait_for_child(
    child: &mut Child,
    containment: &mut ChildContainment,
    deadline: Instant,
) -> io::Result<(ExitStatus, bool)> {
    loop {
        match unix_child_exited_without_reaping(child) {
            Ok(true) => {
                if let Err(error) = containment.terminate() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
                if let Err(error) = containment.quiesce() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
                return child.wait().map(|status| (status, false));
            },
            Ok(false) => {},
            Err(error) => {
                terminate_and_reap(child, containment);
                return Err(error);
            },
        }
        if Instant::now() >= deadline {
            if let Err(error) = containment.terminate() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            if let Err(error) = containment.quiesce() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            let _ = child.kill();
            return child.wait().map(|status| (status, true));
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn unix_child_exited_without_reaping(child: &Child) -> io::Result<bool> {
    let process_id = libc::id_t::try_from(child.id()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "internal worker process ID is outside id_t",
        )
    })?;
    let mut information = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            process_id,
            &mut information,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { information.si_pid() } != 0)
}

#[cfg(windows)]
fn wait_for_child(
    child: &mut Child,
    _containment: &mut ChildContainment,
    deadline: Instant,
) -> io::Result<(ExitStatus, bool)> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok((status, false)),
            Ok(None) => {},
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            },
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait().map(|status| (status, true));
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

fn spawn_bounded_reader<R>(mut reader: R) -> Receiver<io::Result<BoundedOutput>>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            let mut overflowed = false;
            let mut chunk = [0_u8; 8192];
            loop {
                let read = reader.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                let remaining = MAX_FRAME_BYTES.saturating_sub(bytes.len());
                let retained = remaining.min(read);
                bytes.extend_from_slice(&chunk[..retained]);
                overflowed |= retained != read;
            }
            Ok(BoundedOutput { bytes, overflowed })
        })();
        let _ = sender.send(result);
    });
    receiver
}

fn receive_reader(
    reader: Receiver<io::Result<BoundedOutput>>,
    deadline: Instant,
) -> io::Result<BoundedOutput> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    match reader.recv_timeout(remaining) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "internal worker output pipe remained open after process exit",
        )),
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "internal worker output reader terminated without a result",
        )),
    }
}

fn trusted_frame(
    child: ChildResult,
    nonce: &str,
    paths: &WorkerPublicationPaths,
) -> Result<WorkerFrame, ()> {
    if child.timed_out ||
        child.stdout.overflowed ||
        child.stderr.overflowed ||
        !child.stderr.bytes.is_empty() ||
        child.stdout.bytes.last() != Some(&b'\n') ||
        child.stdout.bytes[..child.stdout.bytes.len().saturating_sub(1)].contains(&b'\n')
    {
        return Err(());
    }
    let encoded = &child.stdout.bytes[..child.stdout.bytes.len().saturating_sub(1)];
    if encoded.first() != Some(&b'{') ||
        encoded.last() != Some(&b'}') ||
        encoded.contains(&b'\r') ||
        !worker_frame_has_exact_top_level_shape(encoded)
    {
        return Err(());
    }
    let frame: WorkerFrame = serde_json::from_slice(encoded).map_err(|_| ())?;
    if frame.schema != WORKER_FRAME_SCHEMA ||
        frame.version != PROTOCOL_VERSION ||
        frame.nonce != nonce
    {
        return Err(());
    }
    if worker_result_contains_private_path(&frame.result, paths) {
        return Err(());
    }
    let expected_exit = match &frame.result {
        WorkerResult::Captured { .. } => 0,
        WorkerResult::Failed { error, .. } => error.exit_code,
    };
    if child.status.code() != Some(i32::from(expected_exit)) {
        return Err(());
    }
    Ok(frame)
}

fn worker_frame_has_exact_top_level_shape(encoded: &[u8]) -> bool {
    const CAPTURED_KEYS: &[&str] = &["schema", "version", "nonce", "status", "deferred"];
    const FAILED_KEYS: &[&str] = &["schema", "version", "nonce", "status", "error", "evidence"];
    let Ok(serde_json::Value::Object(object)) = serde_json::from_slice(encoded) else {
        return false;
    };
    let expected = match object.get("status").and_then(serde_json::Value::as_str) {
        Some("captured") => CAPTURED_KEYS,
        Some("failed") => FAILED_KEYS,
        _ => return false,
    };
    object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
}

fn worker_result_contains_private_path(
    result: &WorkerResult,
    paths: &WorkerPublicationPaths,
) -> bool {
    let Ok(value) = serde_json::to_value(result) else {
        return true;
    };
    json_contains_private_path(&value, paths)
}

fn json_contains_private_path(value: &serde_json::Value, paths: &WorkerPublicationPaths) -> bool {
    let Some(fragments) = private_path_fragments(paths) else {
        return true;
    };
    json_contains_private_fragment(value, &fragments)
}

fn json_contains_private_fragment(value: &serde_json::Value, fragments: &[String]) -> bool {
    match value {
        serde_json::Value::String(value) => fragments
            .iter()
            .any(|fragment| string_contains_private_fragment(value, fragment)),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| json_contains_private_fragment(value, fragments)),
        serde_json::Value::Object(values) => values
            .values()
            .any(|value| json_contains_private_fragment(value, fragments)),
        _ => false,
    }
}

fn private_path_fragments(paths: &WorkerPublicationPaths) -> Option<Vec<String>> {
    let mut fragments = Vec::new();
    for (path, may_be_absent) in [
        (&paths.staging_container, false),
        (&paths.staging_artifacts, true),
    ] {
        append_private_path_fragment(&mut fragments, &path.to_string_lossy());
        if let Some(leaf) = path.file_name() {
            let leaf = leaf.to_string_lossy();
            if private_leaf_token(&leaf) {
                fragments.push(leaf.into_owned());
            }
        }
        #[cfg(windows)]
        match windows_short_path_aliases(path) {
            Ok(aliases) => {
                for alias in aliases {
                    append_private_path_fragment(&mut fragments, &alias.to_string_lossy());
                }
            },
            Err(error)
                if (may_be_absent || cfg!(test)) && error.kind() == io::ErrorKind::NotFound => {},
            Err(_) => return None,
        }
    }
    fragments.sort();
    fragments.dedup();
    Some(fragments)
}

fn append_private_path_fragment(fragments: &mut Vec<String>, value: &str) {
    fragments.push(value.to_owned());
    fragments.push(value.replace('\\', "/"));
    fragments.push(value.replace('/', "\\"));
}

fn private_leaf_token(value: &str) -> bool {
    let Some(nonce) = value.strip_prefix(".pliego-runtime-") else {
        return false;
    };
    matches!(nonce.len(), STAGING_PATH_NONCE_HEX_LEN | 64) &&
        nonce
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn string_contains_private_fragment(value: &str, private_fragment: &str) -> bool {
    if cfg!(windows) {
        value
            .to_ascii_lowercase()
            .contains(&private_fragment.to_ascii_lowercase())
    } else {
        value.contains(private_fragment)
    }
}

fn plan_and_promote(
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<(), PlanAndPromoteError> {
    validate_staging_closure(paths, identity).map_err(PlanAndPromoteError::BeforeVisibility)?;
    let artifacts = SessionArtifacts::open_staged_for_publication(
        &paths.staging_artifacts,
        &paths.public_artifacts,
        &identity.render_id,
    )
    .map_err(|_| PlanAndPromoteError::BeforeVisibility(runtime_terminated(paths, identity)))?;
    let journal = artifacts
        .begin_publication(&paths.public_output, &identity.request_fingerprint)
        .map_err(|_| PlanAndPromoteError::BeforeVisibility(runtime_terminated(paths, identity)))?;
    if !matches!(
        journal
            .recover()
            .map_err(
                |_| PlanAndPromoteError::BeforeVisibility(runtime_terminated(paths, identity))
            )?,
        PublicationRecoveryState::Planned
    ) {
        return Err(PlanAndPromoteError::BeforeVisibility(runtime_terminated(
            paths, identity,
        )));
    }
    drop(journal);
    drop(artifacts);
    validate_staging_closure(paths, identity).map_err(PlanAndPromoteError::BeforeVisibility)?;
    promote_staged_artifacts(
        &paths.staging_container,
        &paths.staging_artifacts,
        &paths.public_artifacts,
    )
    // The primitive attempts an identity-bound rollback after a post-rename validation failure,
    // but an error cannot prove absence if that rollback itself failed. Classify every promotion
    // error conservatively as potentially visible.
    .map_err(|error| {
        PlanAndPromoteError::AfterVisibility(staged_artifact_error(error, paths, identity))
    })?;
    require_promoted_plan(paths, identity).map_err(PlanAndPromoteError::AfterVisibility)?;
    // Publication is already atomically visible and bound at this point. Container cleanup is
    // hygiene only; never rewrite an accepted result into RUNTIME_TERMINATED after visibility.
    cleanup_empty_private_container(&paths.staging_container);
    Ok(())
}

fn validate_staging_closure(
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<(), RenderError> {
    validate_staged_artifacts(
        &paths.staging_artifacts,
        &[&paths.staging_container, &paths.staging_artifacts],
    )
    .map_err(|error| staged_artifact_error(error, paths, identity))
}

fn require_promoted_plan(
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<(), RenderError> {
    let artifacts = SessionArtifacts::open_for_publication_recovery(
        &paths.public_artifacts,
        &identity.render_id,
    )
    .map_err(|_| runtime_terminated(paths, identity))?;
    let journal = artifacts
        .resume_publication(&paths.public_output, &identity.request_fingerprint)
        .map_err(|_| runtime_terminated(paths, identity))?;
    if !matches!(
        journal
            .recover()
            .map_err(|_| runtime_terminated(paths, identity))?,
        PublicationRecoveryState::Planned
    ) {
        return Err(runtime_terminated(paths, identity));
    }
    Ok(())
}

fn runtime_terminated(
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> RenderError {
    runtime_terminated_for_render_id(paths, &identity.render_id)
}

fn bind_parent_resource_policy(
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
    required: bool,
) -> Result<(), RenderError> {
    let environment_path = paths.staging_artifacts.join("environment.json");
    match std::fs::symlink_metadata(&environment_path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound && !required => return Ok(()),
        Ok(metadata) if metadata.is_file() => {},
        _ => return Err(runtime_terminated(paths, identity)),
    }
    let artifacts = SessionArtifacts::open_staged_for_publication(
        &paths.staging_artifacts,
        &paths.public_artifacts,
        &identity.render_id,
    )
    .map_err(|_| runtime_terminated(paths, identity))?;
    let (sha256, bytes) = artifacts
        .artifact_identity("environment.json")
        .map_err(|_| runtime_terminated(paths, identity))?;
    let mut environment = artifacts
        .read_json_artifact("environment.json", &sha256, bytes)
        .map_err(|_| runtime_terminated(paths, identity))?;
    let Some(environment) = environment.as_object_mut() else {
        return Err(runtime_terminated(paths, identity));
    };
    environment.insert("resource_policy".into(), identity.resource_policy.clone());
    artifacts
        .write_environment(&serde_json::Value::Object(environment.clone()))
        .map_err(|_| runtime_terminated(paths, identity))
}

fn require_supervised_finalization_access(
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> Result<(), RenderError> {
    let artifacts = SessionArtifacts::open_staged_for_publication(
        &paths.staging_artifacts,
        &paths.public_artifacts,
        &identity.render_id,
    )
    .map_err(|_| runtime_terminated(paths, identity))?;
    artifacts
        .require_session_state_append_access()
        .map_err(|_| runtime_terminated(paths, identity))
}

fn runtime_terminated_for_render_id(
    paths: &WorkerPublicationPaths,
    render_id: &str,
) -> RenderError {
    RenderError::session(
        &paths.public_artifacts,
        &paths.public_output,
        render_id,
        "RUNTIME_TERMINATED",
        "document runtime terminated before returning a trusted result",
    )
}

fn staged_artifact_error(
    error: io::Error,
    paths: &WorkerPublicationPaths,
    identity: &SupervisorRenderIdentity,
) -> RenderError {
    if error
        .get_ref()
        .is_some_and(|source| source.is::<StagedArtifactLimitExceeded>())
    {
        return RenderError::session(
            &paths.public_artifacts,
            &paths.public_output,
            &identity.render_id,
            "ARTIFACT_LIMIT_EXCEEDED",
            format!("document runtime artifact closure was rejected: {error}"),
        );
    }
    runtime_terminated(paths, identity)
}

fn generate_nonce() -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(io::Error::other)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut nonce = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        nonce.push(HEX[(byte >> 4) as usize] as char);
        nonce.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(nonce)
}

fn valid_nonce(nonce: &str) -> bool {
    nonce.len() == 64 &&
        nonce
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn canonical_parent_pid(value: &str) -> Option<u32> {
    let process_id = value.parse::<u32>().ok()?;
    (process_id != 0 && process_id.to_string() == value).then_some(process_id)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;
    use std::process::ExitStatus;

    use super::*;
    #[cfg(windows)]
    use crate::raw_absolute_path;

    const NONCE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn paths() -> WorkerPublicationPaths {
        let private = format!(r"C:\private\.pliego-runtime-{}", "a".repeat(32));
        WorkerPublicationPaths {
            staging_container: PathBuf::from(&private),
            staging_artifacts: PathBuf::from(private).join("artifacts"),
            public_artifacts: PathBuf::from(r"C:\public\artifacts"),
            public_output: PathBuf::from(r"C:\public\invoice.pdf"),
        }
    }

    #[test]
    fn private_directory_setup_errors_never_expose_the_staging_path() {
        let paths = paths();
        let private_error = io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "private directory security validation failed: {}",
                paths.staging_container.display()
            ),
        );
        let detail = public_artifact_creation_error_detail(&paths, private_error);
        assert_eq!(detail, "private artifact staging setup was rejected");
        assert!(!detail.contains(".pliego-runtime-"));
        assert!(!detail.contains(&paths.staging_container.to_string_lossy().into_owned()));
    }

    fn deferred() -> DeferredCapturedPublication {
        let hash = format!("sha256:{}", "1".repeat(64));
        DeferredCapturedPublication {
            schema: "pliego.deferred-captured-publication".into(),
            version: 1,
            render_id: hash.clone(),
            readiness_sha256: hash.clone(),
            readiness_bytes: 2,
            resolved_input_hash: hash.clone(),
            controlled_runtime_ms: 1.0,
            scene_capture_ms: 1.0,
            scene_schema: "pliego.document-scene".into(),
            scene_version: 1,
            scene_hash: hash,
            page_count: 1,
            preview_count: 1,
            capture_status: "complete".into(),
            capture_code: None,
            preview_status: "rendered".into(),
            unsupported_event_count: 0,
            text_mapping_gap_count: 0,
            pdf_status: "rendered".into(),
            pdf_structure_status: "rendered".into(),
            scene_setup_ms: 1.0,
            preview_ms: 1.0,
            pdf_ms: 1.0,
            rendered_bytes: 8,
        }
    }

    fn frame(result: WorkerResult) -> WorkerFrame {
        WorkerFrame {
            schema: WORKER_FRAME_SCHEMA.into(),
            version: PROTOCOL_VERSION,
            nonce: NONCE.into(),
            result,
        }
    }

    fn child_result(frame: &WorkerFrame, exit_code: u8) -> ChildResult {
        let mut bytes = serde_json::to_vec(frame).unwrap();
        bytes.push(b'\n');
        ChildResult {
            status: exit_status(exit_code),
            stdout: BoundedOutput {
                bytes,
                overflowed: false,
            },
            stderr: BoundedOutput {
                bytes: Vec::new(),
                overflowed: false,
            },
            timed_out: false,
        }
    }

    fn preflight_error(output: PathBuf, artifacts: PathBuf) -> (RenderError, PathBuf) {
        let sandbox = artifacts.parent().unwrap().to_owned();
        let input = sandbox.join("input.html");
        std::fs::write(&input, b"<!doctype html><title>preflight</title>").unwrap();
        let input_argument = input
            .strip_prefix(std::env::current_dir().unwrap())
            .expect("preflight fixture must stay beneath the working directory")
            .to_owned();
        let command = parse_args(vec![
            OsString::from("render"),
            input_argument.into_os_string(),
            OsString::from("--output"),
            output.into_os_string(),
            OsString::from("--artifacts"),
            artifacts.clone().into_os_string(),
        ])
        .unwrap();
        let Command::Render(request) = command else {
            panic!("preflight fixture did not parse as render")
        };
        let identity = supervisor_render_identity(&request, false).unwrap();
        let paths = publication_paths(&request, NONCE, &identity).unwrap();
        create_private_directory(&paths.staging_container).unwrap();
        let error = preflight_request_paths(&request, &paths, &identity, false)
            .and_then(|()| preflight_new_publication(&request, &paths, &identity))
            .unwrap_err();
        std::fs::remove_dir(&paths.staging_container).unwrap();
        assert!(!paths.public_artifacts.exists());
        assert!(!paths.staging_container.exists());
        (error, sandbox)
    }

    #[test]
    fn preflight_preserves_missing_output_parent_as_publication_transaction_failure() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-preflight-missing-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let (error, sandbox) = preflight_error(
            sandbox.join("missing-output-parent/invoice.pdf"),
            sandbox.join("artifacts"),
        );
        assert_eq!(error.code, "PUBLICATION_TRANSACTION_FAILED");
        assert!(
            error
                .message
                .contains("cannot begin publication transaction")
        );
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn preflight_preserves_nested_output_as_typed_overlap_without_publication() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-preflight-overlap-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let artifacts = sandbox.join("artifacts");
        let (error, sandbox) = preflight_error(artifacts.join("nested/invoice.pdf"), artifacts);
        assert_eq!(error.code, "OUTPUT_ARTIFACTS_OVERLAP");
        assert_eq!(
            error.message,
            "requested output must be outside the artifact directory"
        );
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn preflight_preserves_existing_output_without_publication() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-preflight-existing-output-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let output = sandbox.join("invoice.pdf");
        let original = b"caller-owned-output";
        std::fs::write(&output, original).unwrap();
        let (error, sandbox) = preflight_error(output.clone(), sandbox.join("artifacts"));
        assert_eq!(error.code, "OUTPUT_ALREADY_EXISTS");
        assert_eq!(std::fs::read(output).unwrap(), original);
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn output_reservation_exhaustion_fails_before_publication() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "pliego-supervisor-output-reservation-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let staging_container = sandbox.join(staging_container_leaf(NONCE).unwrap());
        create_private_directory(&staging_container).unwrap();
        let staging_artifacts = staging_container.join("artifacts");
        let public_artifacts = sandbox.join("public-artifacts");
        let public_output = sandbox.join("invoice.pdf");
        let render_id = format!("sha256:{}", "a".repeat(64));
        let artifacts = SessionArtifacts::create_staged_with_render_id(
            &staging_artifacts,
            &public_artifacts,
            render_id.clone(),
        )
        .unwrap();
        artifacts
            .write_document_pdf(b"%PDF-1.7\nfixture\n")
            .unwrap();
        drop(artifacts);
        for attempt in 0..32 {
            std::fs::write(
                sandbox.join(format!(
                    ".invoice.pdf.pliego-{}-{attempt}.tmp",
                    std::process::id()
                )),
                b"caller-owned",
            )
            .unwrap();
        }
        let paths = WorkerPublicationPaths {
            staging_container,
            staging_artifacts,
            public_artifacts: public_artifacts.clone(),
            public_output: public_output.clone(),
        };
        let error = prepare_supervised_output(&paths, &render_id).unwrap_err();
        assert_eq!(error.code, "OUTPUT_ALREADY_EXISTS");
        assert!(
            error
                .message
                .contains("all temporary output names already exist")
        );
        assert!(!public_artifacts.exists());
        assert!(!public_output.exists());
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn invalid_existing_root_precedes_nested_output_overlap() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-invalid-root-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let input = sandbox.join("input.html");
        std::fs::write(&input, b"<!doctype html><title>invalid root</title>").unwrap();
        let artifacts = sandbox.join("artifacts");
        std::fs::write(&artifacts, b"not a directory").unwrap();
        let output = artifacts.join("nested/invoice.pdf");
        let input_argument = input
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .to_owned();
        let Command::Render(request) = parse_args(vec![
            OsString::from("render"),
            input_argument.into_os_string(),
            OsString::from("--output"),
            output.into_os_string(),
            OsString::from("--artifacts"),
            artifacts.into_os_string(),
        ])
        .unwrap() else {
            panic!("invalid-root fixture did not parse as render")
        };
        let identity = supervisor_render_identity(&request, false).unwrap();
        let paths = publication_paths(&request, NONCE, &identity).unwrap();
        let error = bind_existing_artifact_root(&request, &paths, &identity).unwrap_err();
        assert_eq!(error.code, "PUBLICATION_RECOVERY_FAILED");
        assert!(
            error
                .message
                .contains("existing artifact root cannot be opened for publication recovery")
        );
        assert!(
            output_overlaps_uncreated_artifacts(
                &paths.public_output,
                &paths.public_artifacts,
                &sandbox,
            )
            .unwrap()
        );
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn shorthand_publication_root_retries_occupied_names() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-shorthand-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let base = sandbox.join("session");
        std::fs::create_dir(&base).unwrap();
        std::fs::create_dir(sandbox.join("session-1")).unwrap();
        assert_eq!(
            select_shorthand_publication_root(&base).unwrap(),
            sandbox.join("session-2")
        );
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn regular_file_artifact_parent_preserves_direct_creation_error() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-file-parent-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let input = sandbox.join("input.html");
        std::fs::write(&input, b"<!doctype html><title>file parent</title>").unwrap();
        let parent_file = sandbox.join("parent-file");
        std::fs::write(&parent_file, b"file").unwrap();
        let artifacts = parent_file.join("artifacts");
        let output = sandbox.join("invoice.pdf");
        let input_argument = input
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .to_owned();
        let Command::Render(request) = parse_args(vec![
            OsString::from("render"),
            input_argument.into_os_string(),
            OsString::from("--output"),
            output.into_os_string(),
            OsString::from("--artifacts"),
            artifacts.clone().into_os_string(),
        ])
        .unwrap() else {
            panic!("file-parent fixture did not parse as render")
        };
        let identity = supervisor_render_identity(&request, false).unwrap();
        let paths = publication_paths(&request, NONCE, &identity).unwrap();
        let direct_error =
            SessionArtifacts::create_with_render_id(&artifacts, identity.render_id.clone())
                .unwrap_err();
        let supervisor_error = create_private_directory(&paths.staging_container).unwrap_err();
        let supervisor_error =
            artifact_creation_error(&request, &paths, &identity, supervisor_error);
        assert_eq!(
            supervisor_error.message,
            format!(
                "cannot create exclusive artifact directory {}: {direct_error}",
                artifacts.display()
            )
        );
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn target_name_probe_preserves_overlong_leaf_creation_failure() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-long-leaf-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let input = sandbox.join("input.html");
        std::fs::write(&input, b"<!doctype html><title>long leaf</title>").unwrap();
        let artifacts = sandbox.join("a".repeat(256));
        let output = sandbox.join("invoice.pdf");
        let input_argument = input
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .to_owned();
        let Command::Render(request) = parse_args(vec![
            OsString::from("render"),
            input_argument.into_os_string(),
            OsString::from("--output"),
            output.into_os_string(),
            OsString::from("--artifacts"),
            artifacts.clone().into_os_string(),
        ])
        .unwrap() else {
            panic!("long-leaf fixture did not parse as render")
        };
        let identity = supervisor_render_identity(&request, false).unwrap();
        let paths = publication_paths(&request, NONCE, &identity).unwrap();
        create_private_directory(&paths.staging_container).unwrap();
        let _guard = StagingGuard {
            container: paths.staging_container.clone(),
        };
        let direct_error =
            SessionArtifacts::create_with_render_id(&artifacts, identity.render_id.clone())
                .unwrap_err();
        let error = validate_requested_artifact_leaf(&request, &paths, &identity).unwrap_err();
        assert_eq!(error.code, "ARTIFACTS_CREATE_FAILED");
        assert_eq!(
            error.message,
            format!(
                "cannot create exclusive artifact directory {}: {direct_error}",
                artifacts.display()
            )
        );
        assert!(!artifacts.exists());
        std::fs::remove_file(input).unwrap();
        drop(_guard);
        std::fs::remove_dir(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn render_cleans_private_container_after_leaf_preflight_failure() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-leaf-cleanup-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let input = sandbox.join("input.html");
        std::fs::write(&input, b"<!doctype html><title>leaf cleanup</title>").unwrap();
        let input_argument = PathBuf::from(sandbox.file_name().unwrap()).join("input.html");
        let artifacts = sandbox.join("a".repeat(256));
        let output = sandbox.join("invoice.pdf");
        let Command::Render(request) = parse_args(vec![
            OsString::from("render"),
            input_argument.into_os_string(),
            OsString::from("--output"),
            output.clone().into_os_string(),
            OsString::from("--artifacts"),
            artifacts.clone().into_os_string(),
        ])
        .unwrap() else {
            panic!("leaf-cleanup fixture did not parse as render")
        };
        let error = render(request, false).unwrap_err();
        assert_eq!(error.code, "ARTIFACTS_CREATE_FAILED");
        assert!(!artifacts.exists());
        assert!(!output.exists());
        assert!(std::fs::read_dir(&sandbox).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pliego-runtime-")
        }));
        std::fs::remove_file(input).unwrap();
        std::fs::remove_dir(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn staging_guard_retries_transient_private_container_cleanup_errors() {
        use std::os::unix::fs::PermissionsExt;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "pliego-supervisor-cleanup-retry-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(staging_container_leaf(NONCE).unwrap());
        create_private_directory(&container).unwrap();

        // Model a transient identity-bound cleanup rejection without putting forensic evidence
        // inside the container. The guard must never recurse; it may retry only after errors.
        std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o755)).unwrap();
        let repaired_container = container.clone();
        let repair = std::thread::spawn(move || {
            std::thread::sleep(PRIVATE_CONTAINER_CLEANUP_RETRY_DELAY.saturating_mul(3));
            std::fs::set_permissions(repaired_container, std::fs::Permissions::from_mode(0o700))
                .unwrap();
        });

        drop(StagingGuard {
            container: container.clone(),
        });
        repair.join().unwrap();

        assert!(!container.exists());
        std::fs::remove_dir(sandbox).unwrap();
    }

    #[test]
    fn staging_guard_preserves_nonempty_private_failure_evidence() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "pliego-supervisor-cleanup-evidence-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let container = sandbox.join(staging_container_leaf(NONCE).unwrap());
        create_private_directory(&container).unwrap();
        let evidence = container.join("failure-evidence");
        std::fs::write(&evidence, b"retain me").unwrap();

        drop(StagingGuard {
            container: container.clone(),
        });

        assert_eq!(std::fs::read(&evidence).unwrap(), b"retain me");
        std::fs::remove_file(evidence).unwrap();
        assert!(remove_empty_private_container(&container).unwrap());
        std::fs::remove_dir(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn near_path_limit_is_rejected_by_the_documented_headroom_boundary() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "pliego-supervisor-path-headroom-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let encoded = CString::new(sandbox.as_os_str().as_bytes()).unwrap();
        let path_max = unsafe { libc::pathconf(encoded.as_ptr(), libc::_PC_PATH_MAX) };
        if path_max < 512 {
            std::fs::remove_dir(&sandbox).unwrap();
            return;
        }
        let path_max = usize::try_from(path_max).unwrap();
        let public_suffix = Path::new("a").join("resources").join("0".repeat(64));
        let target_parent_len = path_max
            .checked_sub(public_suffix.as_os_str().as_bytes().len() + 17)
            .unwrap();
        let mut parent = sandbox.clone();
        while parent.as_os_str().as_bytes().len() < target_parent_len {
            let remaining = target_parent_len - parent.as_os_str().as_bytes().len() - 1;
            if remaining == 0 {
                break;
            }
            let component = "p".repeat(remaining.min(200));
            parent.push(component);
            std::fs::create_dir(&parent).unwrap();
        }

        let input = PathBuf::from(format!(
            "pliego-supervisor-path-headroom-input-{}-{unique}.html",
            std::process::id()
        ));
        std::fs::write(&input, b"<!doctype html><title>path headroom</title>").unwrap();
        let artifacts = parent.join("a");
        let output = sandbox.join("invoice.pdf");
        assert!(
            artifacts
                .join("resources")
                .join("0".repeat(64))
                .as_os_str()
                .as_bytes()
                .len() <
                path_max
        );
        let Command::Render(request) = parse_args(vec![
            OsString::from("render"),
            input.clone().into_os_string(),
            OsString::from("--output"),
            output.clone().into_os_string(),
            OsString::from("--artifacts"),
            artifacts.clone().into_os_string(),
        ])
        .unwrap() else {
            panic!("path-headroom fixture did not parse as render")
        };
        let identity = supervisor_render_identity(&request, false).unwrap();
        let paths = publication_paths(&request, NONCE, &identity).unwrap();
        assert!(
            paths
                .staging_container
                .join("target-name-probe")
                .join("a")
                .join("resources")
                .join("0".repeat(64))
                .as_os_str()
                .as_bytes()
                .len() >=
                path_max
        );
        create_private_directory(&paths.staging_container).unwrap();
        let guard = StagingGuard {
            container: paths.staging_container.clone(),
        };
        let error = validate_requested_artifact_leaf(&request, &paths, &identity).unwrap_err();
        assert_eq!(error.code, "ARTIFACTS_CREATE_FAILED");
        assert!(
            error
                .message
                .starts_with("cannot prepare the bounded session artifact tree at ")
        );
        assert!(!artifacts.exists());
        assert!(!output.exists());
        drop(guard);
        assert!(std::fs::read_dir(&parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pliego-runtime-")
        }));
        std::fs::remove_file(input).unwrap();
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_output_parent_matches_only_scaffolded_artifact_directories() {
        use std::os::unix::fs::symlink;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-dangling-output-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let artifacts = sandbox.join("artifacts");
        let root_link = sandbox.join("root-link");
        let resources_parent_link = sandbox.join("resources-parent-link");
        let nested_link = sandbox.join("nested-link");
        let erased_missing_link = sandbox.join("erased-missing-link");
        let escaped_reentry_link = sandbox.join("escaped-reentry-link");
        let escaped_outside_link = sandbox.join("escaped-outside-link");
        let outside = sandbox.join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&artifacts, &root_link).unwrap();
        symlink(artifacts.join("resources/.."), &resources_parent_link).unwrap();
        symlink(artifacts.join("not-created"), &nested_link).unwrap();
        symlink(artifacts.join("missing/.."), &erased_missing_link).unwrap();
        symlink(
            artifacts
                .join("../..")
                .join(sandbox.file_name().unwrap())
                .join("artifacts"),
            &escaped_reentry_link,
        )
        .unwrap();
        symlink(artifacts.join("../outside"), &escaped_outside_link).unwrap();
        assert!(
            output_overlaps_uncreated_artifacts(
                &root_link.join("invoice.pdf"),
                &artifacts,
                &sandbox,
            )
            .unwrap()
        );
        assert!(
            output_overlaps_uncreated_artifacts(
                &resources_parent_link.join("invoice.pdf"),
                &artifacts,
                &sandbox,
            )
            .unwrap()
        );
        assert!(
            !output_overlaps_uncreated_artifacts(
                &nested_link.join("invoice.pdf"),
                &artifacts,
                &sandbox,
            )
            .unwrap()
        );
        assert!(
            !output_overlaps_uncreated_artifacts(
                &erased_missing_link.join("invoice.pdf"),
                &artifacts,
                &sandbox,
            )
            .unwrap()
        );
        assert!(
            output_overlaps_uncreated_artifacts(
                &escaped_reentry_link.join("invoice.pdf"),
                &artifacts,
                &sandbox,
            )
            .unwrap()
        );
        assert!(
            output_overlaps_uncreated_artifacts(
                &escaped_outside_link.join("invoice.pdf"),
                &artifacts,
                &sandbox,
            )
            .unwrap(),
            "an unresolved alias that escapes the future scaffold must fail closed",
        );
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn future_artifact_lookup_uses_the_actual_filesystem_name_policy() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-name-policy-{}-{unique}",
            std::process::id()
        ));
        let probe = sandbox.join("probe");
        std::fs::create_dir_all(&probe).unwrap();
        let artifacts = sandbox.join("\u{c9}vidence");
        let alternate = sandbox.join("e\u{301}VIDENCE");
        std::fs::create_dir(&artifacts).unwrap();
        let expected_overlap = alternate.canonicalize().is_ok();
        std::fs::remove_dir(&artifacts).unwrap();

        assert_eq!(
            output_overlaps_uncreated_artifacts(
                &alternate.join("invoice.pdf"),
                &artifacts,
                &probe,
            )
            .unwrap(),
            expected_overlap,
        );
        assert_eq!(
            output_overlaps_uncreated_artifacts(&alternate, &artifacts, &probe).unwrap(),
            expected_overlap,
        );
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn drive_relative_output_uses_standard_windows_absolutization() {
        let drive_relative = Path::new(r"C:pliego-output\invoice.pdf");
        assert_eq!(
            raw_absolute_path(drive_relative).unwrap(),
            std::path::absolute(drive_relative).unwrap(),
        );
    }

    #[cfg(windows)]
    #[test]
    fn future_artifact_root_uses_windows_name_identity_and_rejects_dos_alias_shape() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-windows-root-name-{}-{unique}",
            std::process::id()
        ));
        let probe = sandbox.join("probe");
        std::fs::create_dir_all(&probe).unwrap();
        let existing = sandbox.join("existing");
        std::fs::create_dir(&existing).unwrap();
        let artifacts = sandbox.join("Artifacts");
        let alternate = sandbox.join("ARTIFACTS");
        std::fs::create_dir(&artifacts).unwrap();
        let expected_overlap = same_file::is_same_file(&artifacts, &alternate).unwrap_or(false);
        std::fs::remove_dir(&artifacts).unwrap();

        assert_eq!(
            output_overlaps_uncreated_artifacts(&alternate, &artifacts, &probe).unwrap(),
            expected_overlap,
        );
        assert_eq!(
            output_overlaps_uncreated_artifacts(
                &existing.join("..").join("ARTIFACTS"),
                &artifacts,
                &probe,
            )
            .unwrap(),
            expected_overlap,
        );
        assert!(
            output_overlaps_uncreated_artifacts(&sandbox.join("ARTIFA~1"), &artifacts, &probe,)
                .unwrap(),
            "prospective destination-dependent DOS aliases must fail closed",
        );
        assert!(
            output_overlaps_uncreated_artifacts(&sandbox.join("A~BCDE~2"), &artifacts, &probe,)
                .unwrap(),
            "DOS aliases may retain an earlier literal tilde",
        );
        assert!(
            output_overlaps_uncreated_artifacts(&sandbox.join("ARTIFA~1."), &artifacts, &probe,)
                .unwrap(),
            "Win32-normalized DOS alias spellings must fail closed",
        );
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn explicit_artifact_parent_failure_preserves_api1_error_and_requested_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let input_name = format!("pliego-supervisor-artifact-parent-input-{unique}.html");
        std::fs::write(
            &input_name,
            b"<!doctype html><title>artifact parent</title>",
        )
        .unwrap();
        let artifacts = PathBuf::from(format!(
            "pliego-supervisor-missing-artifact-parent-{unique}/artifacts"
        ));
        let missing_parent = artifacts.parent().unwrap().to_owned();
        let output = PathBuf::from(format!("pliego-supervisor-output-{unique}.pdf"));
        let command = parse_args(vec![
            OsString::from("render"),
            OsString::from(&input_name),
            OsString::from("--output"),
            output.clone().into_os_string(),
            OsString::from("--artifacts"),
            artifacts.clone().into_os_string(),
        ])
        .unwrap();
        let Command::Render(request) = command else {
            panic!("artifact-parent fixture did not parse as render")
        };
        let identity = supervisor_render_identity(&request, false).unwrap();
        let direct_error =
            SessionArtifacts::create_with_render_id(&artifacts, identity.render_id.clone())
                .unwrap_err();
        let error = match publication_paths(&request, NONCE, &identity) {
            Err(error) => error,
            Ok(_) => panic!("missing artifact parent unexpectedly passed preflight"),
        };
        assert_eq!(error.code, "ARTIFACTS_CREATE_FAILED");
        assert_eq!(
            error.message,
            format!(
                "cannot create exclusive artifact directory {}: {direct_error}",
                artifacts.display()
            )
        );
        assert_eq!(error.artifacts, Some(artifacts));
        assert_eq!(error.document_pdf, Some(output));
        assert_eq!(
            error.render_id.as_deref(),
            Some(identity.render_id.as_str())
        );
        assert!(!missing_parent.exists());
        std::fs::remove_file(input_name).unwrap();
    }

    #[cfg(unix)]
    fn exit_status(code: u8) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        ExitStatus::from_raw(i32::from(code) << 8)
    }

    #[cfg(windows)]
    fn exit_status(code: u8) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;

        ExitStatus::from_raw(u32::from(code))
    }

    #[test]
    fn trusted_frame_requires_status_to_match_result_in_both_directions() {
        let captured = frame(WorkerResult::Captured {
            deferred: deferred(),
        });
        let captured_child = child_result(&captured, 0);
        let encoded =
            &captured_child.stdout.bytes[..captured_child.stdout.bytes.len().saturating_sub(1)];
        let decoded: WorkerFrame = serde_json::from_slice(encoded).unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
        assert_eq!(decoded.schema, WORKER_FRAME_SCHEMA);
        assert_eq!(decoded.version, PROTOCOL_VERSION);
        assert_eq!(decoded.nonce, NONCE);
        assert!(!worker_result_contains_private_path(
            &decoded.result,
            &paths()
        ));
        assert_eq!(captured_child.status.code(), Some(0));
        assert!(trusted_frame(captured_child, NONCE, &paths()).is_ok());
        assert!(trusted_frame(child_result(&captured, 1), NONCE, &paths()).is_err());

        let failed = frame(WorkerResult::Failed {
            error: WireRenderError {
                code: "SETTLEMENT_FAILED".into(),
                message: "settlement failed".into(),
                exit_code: 1,
                artifacts: Some(paths().public_artifacts.to_string_lossy().into_owned()),
                document_pdf: Some(paths().public_output.to_string_lossy().into_owned()),
                render_id: Some(deferred().render_id),
                warnings: Vec::new(),
            },
            evidence: FailureEvidenceDisposition::Staged,
        });
        assert!(trusted_frame(child_result(&failed, 1), NONCE, &paths()).is_ok());
        assert!(trusted_frame(child_result(&failed, 0), NONCE, &paths()).is_err());
    }

    #[test]
    fn captured_frame_timings_share_the_artifact_json_number_parser() {
        let mut deferred = deferred();
        deferred.controlled_runtime_ms = 96.000_007_629_394_53;
        let captured = frame(WorkerResult::Captured { deferred });
        let encoded = serde_json::to_value(&captured).unwrap();
        assert!(encoded["deferred"]["controlled_runtime_ms"].is_number());
        assert!(trusted_frame(child_result(&captured, 0), NONCE, &paths()).is_ok());
        let encoded = serde_json::to_vec(&captured).unwrap();
        let decoded: WorkerFrame = serde_json::from_slice(&encoded).unwrap();
        let WorkerResult::Captured { deferred } = decoded.result else {
            unreachable!()
        };
        let mut environment: serde_json::Value = serde_json::from_str(
            r#"{"phase_timings_ms":{"controlled_runtime":96.00000762939453}}"#,
        )
        .unwrap();
        let first_environment_timing = environment["phase_timings_ms"]["controlled_runtime"]
            .as_f64()
            .unwrap();
        assert_eq!(deferred.controlled_runtime_ms, first_environment_timing);
        environment.as_object_mut().unwrap().insert(
            "resource_policy".into(),
            serde_json::json!({ "mode": "parent-bound" }),
        );
        let rewritten = serde_json::to_vec(&environment).unwrap();
        let reparsed: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(
            deferred.controlled_runtime_ms,
            reparsed["phase_timings_ms"]["controlled_runtime"]
                .as_f64()
                .unwrap()
        );
    }

    #[test]
    fn trusted_failed_frame_preserves_warning_order() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::current_dir().unwrap().join(format!(
            "pliego-supervisor-warning-frame-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let input = sandbox.join("input.html");
        std::fs::write(&input, b"<!doctype html><title>warnings</title>").unwrap();
        let artifacts = sandbox.join("artifacts");
        let output = sandbox.join("document.pdf");
        let input_argument = input
            .strip_prefix(std::env::current_dir().unwrap())
            .unwrap()
            .to_owned();
        let Command::Render(request) = parse_args(vec![
            OsString::from("render"),
            input_argument.into_os_string(),
            OsString::from("--output"),
            output.into_os_string(),
            OsString::from("--artifacts"),
            artifacts.into_os_string(),
        ])
        .unwrap() else {
            panic!("warning-frame fixture did not parse as render")
        };
        let identity = supervisor_render_identity(&request, false).unwrap();
        let paths = publication_paths(&request, NONCE, &identity).unwrap();
        let failed = frame(WorkerResult::Failed {
            error: WireRenderError {
                code: "PUBLICATION_FAILED".into(),
                message: "publication failed".into(),
                exit_code: 1,
                artifacts: Some(paths.public_artifacts.to_string_lossy().into_owned()),
                document_pdf: Some(paths.public_output.to_string_lossy().into_owned()),
                render_id: Some(identity.render_id.clone()),
                warnings: vec!["first warning".into(), "second warning".into()],
            },
            evidence: FailureEvidenceDisposition::Staged,
        });
        let trusted = trusted_frame(child_result(&failed, 1), NONCE, &paths).unwrap();
        let WorkerResult::Failed { error, .. } = trusted.result else {
            panic!("trusted failed frame changed result kind")
        };
        let error = error.into_trusted_error(&paths, &identity).unwrap();
        assert_eq!(error.warnings, ["first warning", "second warning"]);
        std::fs::remove_file(input).unwrap();
        std::fs::remove_dir(sandbox).unwrap();
    }

    #[test]
    fn trusted_frame_requires_canonical_single_line_encoding() {
        let frame = frame(WorkerResult::Captured {
            deferred: deferred(),
        });
        let mut child = child_result(&frame, 0);
        child.stdout.bytes.insert(0, b' ');
        assert!(trusted_frame(child, NONCE, &paths()).is_err());

        let mut value = serde_json::to_value(&frame).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), serde_json::Value::Bool(true));
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        let child = ChildResult {
            status: exit_status(0),
            stdout: BoundedOutput {
                bytes,
                overflowed: false,
            },
            stderr: BoundedOutput {
                bytes: Vec::new(),
                overflowed: false,
            },
            timed_out: false,
        };
        assert!(trusted_frame(child, NONCE, &paths()).is_err());
    }

    #[test]
    fn trusted_frame_rejects_decoded_private_path_and_stderr() {
        let private_leaf = paths()
            .staging_container
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let failed_frame = frame(WorkerResult::Failed {
            error: WireRenderError {
                code: "SETTLEMENT_FAILED".into(),
                message: format!("worker failed beneath {private_leaf}"),
                exit_code: 1,
                artifacts: Some(paths().public_artifacts.to_string_lossy().into_owned()),
                document_pdf: Some(paths().public_output.to_string_lossy().into_owned()),
                render_id: Some(deferred().render_id),
                warnings: Vec::new(),
            },
            evidence: FailureEvidenceDisposition::Staged,
        });
        assert!(trusted_frame(child_result(&failed_frame, 1), NONCE, &paths()).is_err());

        let captured_frame = frame(WorkerResult::Captured {
            deferred: deferred(),
        });
        let mut child = child_result(&captured_frame, 0);
        child.stderr.bytes = b"unexpected log\n".to_vec();
        assert!(trusted_frame(child, NONCE, &paths()).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_permission_denied_group_observation_is_never_drained() {
        assert_eq!(
            macos_process_group_observation_from_result(Ok(true)).unwrap(),
            MacosProcessGroupObservation::ZombieOnly
        );
        assert_eq!(
            macos_process_group_observation_from_result(Ok(false)).unwrap(),
            MacosProcessGroupObservation::Live
        );
        assert_eq!(
            macos_process_group_observation_from_result(Err(io::Error::from(
                io::ErrorKind::PermissionDenied,
            )))
            .unwrap(),
            MacosProcessGroupObservation::TemporarilyUnobservable
        );
        assert_eq!(
            macos_process_group_observation_from_result(Err(io::Error::from(
                io::ErrorKind::InvalidData,
            )))
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_termination_accepts_an_already_zombie_only_group() {
        let mut command = ProcessCommand::new("/bin/sh");
        command
            .arg("-c")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ChildContainment::configure(&mut command);
        let mut child = command.spawn().unwrap();
        let mut containment = ChildContainment::bind(&child).unwrap();
        let child_id = libc::pid_t::try_from(child.id()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !unix_child_exited_without_reaping(&child).unwrap() {
            assert!(
                Instant::now() < deadline,
                "zombie-only process-group fixture did not become waitable"
            );
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        let (state, observed_group) = macos_process_state_and_group(child_id)
            .unwrap()
            .expect("waitable process remains observable before it is reaped");
        assert_eq!(state, libc::SZOMB);
        assert_eq!(observed_group, containment.process_group);

        containment.quiesce().unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_process_group_distinguishes_live_members_from_zombies() {
        let mut command = ProcessCommand::new("/bin/sh");
        command
            .arg("-c")
            .arg("trap '' HUP; sleep 60 & wait")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ChildContainment::configure(&mut command);
        let mut child = command.spawn().unwrap();
        let mut containment = ChildContainment::bind(&child).unwrap();
        let child_id = libc::pid_t::try_from(child.id()).unwrap();

        assert!(!macos_process_group_has_only_zombies(containment.process_group).unwrap());
        containment.terminate().unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !unix_child_exited_without_reaping(&child).unwrap() {
            assert!(
                Instant::now() < deadline,
                "process-group fixture did not become waitable"
            );
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        let (state, observed_group) = macos_process_state_and_group(child_id)
            .unwrap()
            .expect("waitable process remains observable before it is reaped");
        assert_eq!(state, libc::SZOMB);
        assert_eq!(observed_group, containment.process_group);
        loop {
            if macos_process_group_has_only_zombies(containment.process_group).unwrap() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminated process-group members did not become zombies"
            );
            thread::sleep(PROCESS_POLL_INTERVAL);
        }

        containment.quiesce().unwrap();
        let _ = child.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_process_group_quiesces_a_descendant_after_the_worker_exits() {
        let mut command = ProcessCommand::new("/bin/sh");
        command
            .arg("-c")
            .arg(r#"trap '' HUP; sleep 60 & printf '%s\n' "$!"; exit 0"#)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        ChildContainment::configure(&mut command);
        let mut child = command.spawn().unwrap();
        let mut containment = ChildContainment::bind(&child).unwrap();
        let mut descendant = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut descendant)
            .unwrap();
        let descendant = descendant.trim().parse::<libc::pid_t>().unwrap();
        assert_eq!(
            unsafe { libc::getpgid(descendant) },
            containment.process_group
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !unix_child_exited_without_reaping(&child).unwrap() {
            assert!(
                Instant::now() < deadline,
                "process-group fixture did not exit its leader"
            );
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
        assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);

        containment.quiesce().unwrap();
        assert!(child.wait().unwrap().success());
        #[cfg(target_os = "linux")]
        assert!(
            linux_process_state_and_group(descendant)
                .unwrap()
                .is_none_or(|(state, _)| matches!(state, 'Z' | 'X'))
        );
        #[cfg(target_os = "macos")]
        assert!(
            macos_process_state_and_group(descendant)
                .unwrap()
                .is_none_or(|(state, group)| {
                    state == libc::SZOMB && group == containment.process_group
                })
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            assert_eq!(unsafe { libc::kill(descendant, 0) }, -1);
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_stat_read_esrch_is_process_disappearance() {
        let mut child = ProcessCommand::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let process_id = libc::pid_t::try_from(child.id()).unwrap();
        let mut stat = match std::fs::File::open(format!("/proc/{process_id}/stat")) {
            Ok(stat) => stat,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("cannot open live Linux process stat: {error}");
            },
        };

        child.kill().unwrap();
        child.wait().unwrap();

        let mut contents = String::new();
        let error = stat.read_to_string(&mut contents).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
        assert!(linux_process_stat_error_is_disappearance(&error));
    }

    #[cfg(unix)]
    #[test]
    fn unix_supervisor_rejects_auto_reap_or_custom_sigchld_policy() {
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = libc::SIG_DFL;
        assert!(sigchld_disposition_is_waitable(&action));

        action.sa_flags |= libc::SA_NOCLDWAIT;
        assert!(!sigchld_disposition_is_waitable(&action));
        action.sa_flags &= !libc::SA_NOCLDWAIT;
        action.sa_sigaction = libc::SIG_IGN;
        assert!(!sigchld_disposition_is_waitable(&action));
    }

    #[cfg(windows)]
    #[test]
    fn windows_child_is_job_bound_before_primary_thread_resumes() {
        let command_interpreter = std::env::var_os("ComSpec")
            .unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows\System32\cmd.exe"));
        let mut command = ProcessCommand::new(command_interpreter);
        command
            .args(["/D", "/C", "exit", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ChildContainment::configure(&mut command);

        let mut child = command.spawn().expect("spawn suspended child");
        let mut containment = ChildContainment::bind(&child).expect("bind child to job");
        assert_eq!(containment.active_processes().unwrap(), 1);
        assert_eq!(child.try_wait().unwrap(), None);

        // `resume` succeeds only when ResumeThread reports the CREATE_SUSPENDED count of one.
        containment
            .resume()
            .expect("resume job-bound primary thread");
        assert!(child.wait().unwrap().success());
        containment.quiesce().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_job_quiesces_a_descendant_after_the_worker_exits() {
        let powershell = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let mut command = ProcessCommand::new(powershell);
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                r#"$null = Start-Process -FilePath "$env:SystemRoot\System32\ping.exe" -ArgumentList @("-n","60","127.0.0.1") -WindowStyle Hidden -PassThru; exit 0"#,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ChildContainment::configure(&mut command);

        let mut child = command.spawn().expect("spawn suspended child");
        let mut containment = ChildContainment::bind(&child).expect("bind child to job");
        containment
            .resume()
            .expect("resume job-bound primary thread");
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("job-bound descendant fixture did not exit its group leader");
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        };
        assert!(status.success());
        assert!(containment.active_processes().unwrap() >= 1);

        containment.quiesce().unwrap();
        assert_eq!(containment.active_processes().unwrap(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn private_container_token_is_case_insensitive_on_windows() {
        let token = paths()
            .staging_container
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_uppercase();
        assert!(json_contains_private_path(
            &serde_json::Value::String(format!("leaked token: {token}")),
            &paths(),
        ));
    }

    #[cfg(windows)]
    #[test]
    fn private_container_short_alias_is_rejected_when_available() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::temp_dir().join(format!(
            "pliego-supervisor-frame-alias-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let staging_container = sandbox.join(format!(
            ".pliego-runtime-{unique:032x}{:032x}",
            std::process::id()
        ));
        create_private_directory(&staging_container).unwrap();
        let staging_artifacts = staging_container.join("artifacts");
        std::fs::create_dir(&staging_artifacts).unwrap();
        let paths = WorkerPublicationPaths {
            staging_container: staging_container.clone(),
            staging_artifacts,
            public_artifacts: sandbox.join("public-artifacts"),
            public_output: sandbox.join("invoice.pdf"),
        };
        let aliases = windows_short_path_aliases(&staging_container).unwrap();
        let Some(alias) = aliases.first() else {
            eprintln!("SKIP: volume assigned no distinct private 8.3 alias");
            std::fs::remove_dir_all(sandbox).unwrap();
            return;
        };
        assert!(json_contains_private_path(
            &serde_json::Value::String(alias.to_string_lossy().into_owned()),
            &paths,
        ));
        std::fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn absent_staged_artifact_root_keeps_typed_failure_frame_scannable() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::temp_dir().join(format!(
            "pliego-supervisor-absent-stage-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&sandbox).unwrap();
        let staging_container = sandbox.join(staging_container_leaf(NONCE).unwrap());
        create_private_directory(&staging_container).unwrap();
        let paths = WorkerPublicationPaths {
            staging_artifacts: staging_container.join("artifacts"),
            staging_container: staging_container.clone(),
            public_artifacts: sandbox.join("public-artifacts"),
            public_output: sandbox.join("invoice.pdf"),
        };
        assert!(!paths.staging_artifacts.exists());
        assert!(private_path_fragments(&paths).is_some());
        assert!(!json_contains_private_path(
            &serde_json::json!({"message": "typed bootstrap failure"}),
            &paths,
        ));
        assert!(remove_empty_private_container(&staging_container).unwrap());
        std::fs::remove_dir(sandbox).unwrap();
    }

    #[test]
    fn nonce_is_exact_lowercase_hex() {
        assert!(valid_nonce(NONCE));
        assert!(!valid_nonce(&NONCE.to_uppercase()));
        assert!(!valid_nonce("short"));
    }

    #[test]
    fn parent_pid_capability_is_canonical_nonzero_decimal() {
        assert_eq!(canonical_parent_pid("42"), Some(42));
        assert_eq!(canonical_parent_pid("0"), None);
        assert_eq!(canonical_parent_pid("042"), None);
        assert_eq!(canonical_parent_pid(" 42"), None);
        assert_eq!(canonical_parent_pid("-42"), None);
    }

    #[test]
    fn valid_worker_marker_never_falls_through_when_parent_verification_fails() {
        assert_eq!(
            classify_worker_entry(None, None, |_| panic!("public CLI has no parent check")),
            WorkerEntryDecision::PublicCli
        );
        assert_eq!(
            classify_worker_entry(Some("not-a-capability".into()), None, |_| panic!(
                "invalid marker has no parent check"
            )),
            WorkerEntryDecision::PublicCli
        );
        assert_eq!(
            classify_worker_entry(Some(NONCE.into()), None, |_| true),
            WorkerEntryDecision::RejectInternal
        );
        assert_eq!(
            classify_worker_entry(Some(NONCE.into()), Some("42".into()), |_| false),
            WorkerEntryDecision::RejectInternal
        );
        assert_eq!(
            classify_worker_entry(Some(NONCE.into()), Some("42".into()), |_| true),
            WorkerEntryDecision::Run {
                marker: NONCE.into(),
                parent_pid: 42,
            }
        );
    }

    #[test]
    fn authenticated_worker_command_pairings_preserve_stable_and_alias_routes() {
        let command = |name: &str| {
            parse_args(vec![
                OsString::from(name),
                OsString::from("input.html"),
                OsString::from("--output"),
                OsString::from("document.pdf"),
                OsString::from("--artifacts"),
                OsString::from("artifacts"),
            ])
            .unwrap()
        };

        assert!(worker_request_for_manifest(false, command("render")).is_some());
        assert!(worker_request_for_manifest(true, command("render")).is_some());
        assert!(worker_request_for_manifest(true, command("render-controlled")).is_some());
        assert!(worker_request_for_manifest(false, command("render-controlled")).is_none());
    }
}
