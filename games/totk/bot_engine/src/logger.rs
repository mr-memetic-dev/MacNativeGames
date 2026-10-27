//! Off-thread bounded telemetry logger for `bot_engine` so `bot_on_frame_tick` never performs
//! blocking disk or stderr I/O on the Metal render/present thread.

use crate::ffi::{BotClearanceSummary, BotVec3, BOT_ASHOT_PATH_MAX};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

#[cfg(unix)]
extern "C" {
    fn geteuid() -> u32;
}

#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x0100;
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
const O_NOFOLLOW: i32 = 0x0100;

/// Returns the process effective user ID on Unix.
#[inline]
pub fn current_euid() -> u32 {
    #[cfg(unix)]
    unsafe {
        geteuid()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Sanitizes an `f32` for RFC 8259 JSON emission, replacing `NaN` or `±Inf` with `0.0`.
#[inline]
pub fn json_f32(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Ensures `dir` exists, is not a symlink (`lstat`), is a directory owned by `geteuid()`,
/// and enforces mode `0700` (no group/world permissions).
pub fn ensure_secure_dir(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path is empty",
        ));
    }

    #[cfg(unix)]
    {
        let euid = current_euid();
        match std::fs::symlink_metadata(dir) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "refusing symlink directory",
                    ));
                }
                if !meta.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "path exists and is not a directory",
                    ));
                }
                if meta.uid() != euid {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "directory uid does not match effective uid",
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)?;
            }
            Err(e) => return Err(e),
        }

        let meta = std::fs::symlink_metadata(dir)?;
        if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != euid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "directory failed post-creation lstat/uid verification",
            ));
        }

        std::fs::set_permissions(dir, Permissions::from_mode(0o700))?;
        let meta = std::fs::symlink_metadata(dir)?;
        if meta.file_type().is_symlink()
            || !meta.is_dir()
            || meta.uid() != euid
            || (meta.mode() & 0o077) != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "directory failed 0700 permission verification",
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Validates the parent directory via [`ensure_secure_dir`] and opens `trace_path` with
/// `O_NOFOLLOW` and mode `0600`, verifying regular-file type and `uid == geteuid()`.
pub fn open_secure_trace_file(trace_path: &Path) -> io::Result<File> {
    let parent = trace_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trace_path has no parent directory",
            )
        })?;
    ensure_secure_dir(parent)?;

    #[cfg(unix)]
    {
        let euid = current_euid();
        match std::fs::symlink_metadata(trace_path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "refusing symlink trace file",
                    ));
                }
                if !meta.file_type().is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "trace path exists and is not a regular file",
                    ));
                }
                if meta.uid() != euid {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "trace file uid does not match effective uid",
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(O_NOFOLLOW)
            .open(trace_path)?;

        let fmeta = file.metadata()?;
        if !fmeta.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "opened trace descriptor is not a regular file",
            ));
        }
        if fmeta.uid() != euid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "opened trace file uid does not match effective uid",
            ));
        }
        file.set_permissions(Permissions::from_mode(0o600))?;
        let fmeta = file.metadata()?;
        if (fmeta.mode() & 0o077) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trace file failed 0600 permission verification",
            ));
        }
        Ok(file)
    }

    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(trace_path)
    }
}

/// Writes a single RFC 8259 JSONL telemetry record with all float fields sanitized via [`json_f32`].
#[allow(clippy::too_many_arguments)]
pub fn write_frame_telemetry_json<W: Write>(
    w: &mut W,
    frame_index: u64,
    now_ns: u64,
    vfr_dt_s: f32,
    state_flags: u32,
    draws: u32,
    wb_stage_count: u32,
    phase_name: &str,
    player_pos: BotVec3,
    camera_pos: BotVec3,
    camera_fwd: BotVec3,
    clearance: BotClearanceSummary,
    actor_count: u32,
    buttons: u64,
    lx: i32,
    ly: i32,
    rx: i32,
    ry: i32,
    ashot_id: u32,
) -> io::Result<()> {
    writeln!(
        w,
        "{{\"frame\":{},\"now_ns\":{},\"dt_s\":{:.5},\"flags\":{},\"draws\":{},\"wb\":{},\"phase\":\"{}\",\"player\":[{:.3},{:.3},{:.3}],\"cam_pos\":[{:.3},{:.3},{:.3}],\"cam_fwd\":[{:.4},{:.4},{:.4}],\"depth_ok\":{},\"valid_cells\":{},\"clear_front_m\":{:.2},\"clear_left_m\":{:.2},\"clear_right_m\":{:.2},\"ground_m\":{:.2},\"actors\":{},\"nearest_actor_m\":{:.2},\"nearest_actor_hash\":\"0x{:08x}\",\"buttons\":{},\"lx\":{},\"ly\":{},\"rx\":{},\"ry\":{},\"ashot_id\":{}}}",
        frame_index,
        now_ns,
        json_f32(vfr_dt_s),
        state_flags,
        draws,
        wb_stage_count,
        phase_name,
        json_f32(player_pos.x),
        json_f32(player_pos.y),
        json_f32(player_pos.z),
        json_f32(camera_pos.x),
        json_f32(camera_pos.y),
        json_f32(camera_pos.z),
        json_f32(camera_fwd.x),
        json_f32(camera_fwd.y),
        json_f32(camera_fwd.z),
        clearance.depth_valid,
        clearance.valid_cells,
        json_f32(clearance.front_clear_m),
        json_f32(clearance.left_clear_m),
        json_f32(clearance.right_clear_m),
        json_f32(clearance.ground_ahead_m),
        actor_count,
        json_f32(clearance.nearest_actor_dist_m),
        clearance.nearest_actor_hash,
        buttons,
        lx,
        ly,
        rx,
        ry,
        ashot_id
    )
}

#[derive(Clone, Debug)]
pub enum BotAsyncEvent {
    Banner {
        ashot_dir: String,
        trace_path: String,
        exit_on_complete: bool,
    },
    PhaseTransition {
        frame_index: u64,
        phase_name: &'static str,
        player_pos: BotVec3,
        disp_m: f32,
        clearance: BotClearanceSummary,
        actor_count: u32,
        wb_stage_count: u32,
    },
    FrameTelemetry {
        frame_index: u64,
        now_ns: u64,
        vfr_dt_s: f32,
        state_flags: u32,
        draws: u32,
        wb_stage_count: u32,
        phase_name: &'static str,
        player_pos: BotVec3,
        camera_pos: BotVec3,
        camera_fwd: BotVec3,
        clearance: BotClearanceSummary,
        actor_count: u32,
        buttons: u64,
        lx: i32,
        ly: i32,
        rx: i32,
        ry: i32,
        ashot_id: u32,
    },
    AshotQueued {
        frame_index: u64,
        ashot_id: u32,
        path_bytes: [u8; BOT_ASHOT_PATH_MAX],
    },
    Summary {
        frame_index: u64,
        start_pos: BotVec3,
        end_pos: BotVec3,
        disp_m: f32,
        clearance: BotClearanceSummary,
        actor_count: u32,
        wb_stage_count: u32,
    },
    Flush(SyncSender<()>),
}

pub struct BotAsyncLogger {
    tx: Option<SyncSender<BotAsyncEvent>>,
}

impl BotAsyncLogger {
    pub fn spawn(trace_path: PathBuf) -> Self {
        let (tx, rx) = sync_channel::<BotAsyncEvent>(512);
        match thread::Builder::new()
            .name("botw-bot-io".to_string())
            .spawn(move || Self::worker_loop(trace_path, rx))
        {
            Ok(_handle) => Self { tx: Some(tx) },
            Err(_) => Self { tx: None },
        }
    }

    #[inline]
    pub fn send(&self, ev: BotAsyncEvent) {
        if let Some(ref tx) = self.tx {
            let _ = tx.try_send(ev);
        }
    }

    /// Flushes all pending telemetry events to disk with a short bounded timeout (200 ms).
    pub fn flush(&self) {
        if let Some(ref tx) = self.tx {
            let (ack_tx, ack_rx) = sync_channel::<()>(1);
            if tx.try_send(BotAsyncEvent::Flush(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(Duration::from_millis(200));
            }
        }
    }

    fn worker_loop(trace_path: PathBuf, rx: Receiver<BotAsyncEvent>) {
        let mut trace_writer: Option<BufWriter<File>> = match open_secure_trace_file(&trace_path) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                eprintln!(
                    "[bot_engine] WARNING: refusing to open trace file '{}': {}",
                    trace_path.display(),
                    e
                );
                None
            }
        };

        while let Ok(ev) = rx.recv() {
            match ev {
                BotAsyncEvent::Banner {
                    ashot_dir,
                    trace_path,
                    exit_on_complete,
                } => {
                    eprintln!(
                        "[bot_engine] active: ABI v{} smoke=1 ashot_dir={} trace={} exit_on_complete={}",
                        crate::ffi::BOT_ABI_VERSION,
                        ashot_dir,
                        trace_path,
                        exit_on_complete as u8
                    );
                }
                BotAsyncEvent::PhaseTransition {
                    frame_index,
                    phase_name,
                    player_pos,
                    disp_m,
                    clearance,
                    actor_count,
                    wb_stage_count,
                } => {
                    eprintln!(
                        "[bot_engine] frame={} phase={} pos=({:.2},{:.2},{:.2}) disp={:.2}m depth_ok={} clear_f={:.1}m clear_l={:.1}m clear_r={:.1}m ground={:.1}m actors={} near_act={:.1}m(0x{:08x}) wb={}",
                        frame_index,
                        phase_name,
                        player_pos.x,
                        player_pos.y,
                        player_pos.z,
                        disp_m,
                        clearance.depth_valid as u8,
                        clearance.front_clear_m,
                        clearance.left_clear_m,
                        clearance.right_clear_m,
                        clearance.ground_ahead_m,
                        actor_count,
                        clearance.nearest_actor_dist_m,
                        clearance.nearest_actor_hash,
                        wb_stage_count
                    );
                }
                BotAsyncEvent::FrameTelemetry {
                    frame_index,
                    now_ns,
                    vfr_dt_s,
                    state_flags,
                    draws,
                    wb_stage_count,
                    phase_name,
                    player_pos,
                    camera_pos,
                    camera_fwd,
                    clearance,
                    actor_count,
                    buttons,
                    lx,
                    ly,
                    rx,
                    ry,
                    ashot_id,
                } => {
                    if let Some(w) = trace_writer.as_mut() {
                        let _ = write_frame_telemetry_json(
                            w,
                            frame_index,
                            now_ns,
                            vfr_dt_s,
                            state_flags,
                            draws,
                            wb_stage_count,
                            phase_name,
                            player_pos,
                            camera_pos,
                            camera_fwd,
                            clearance,
                            actor_count,
                            buttons,
                            lx,
                            ly,
                            rx,
                            ry,
                            ashot_id,
                        );
                        if frame_index % 60 == 0 {
                            let _ = w.flush();
                        }
                    }
                }
                BotAsyncEvent::AshotQueued {
                    frame_index,
                    ashot_id,
                    path_bytes,
                } => {
                    let nul = path_bytes
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(BOT_ASHOT_PATH_MAX);
                    let path_str = core::str::from_utf8(&path_bytes[..nul]).unwrap_or("");
                    eprintln!(
                        "[bot_engine] frame={} queued ashot #{} -> {}",
                        frame_index, ashot_id, path_str
                    );
                }
                BotAsyncEvent::Summary {
                    frame_index,
                    start_pos,
                    end_pos,
                    disp_m,
                    clearance,
                    actor_count,
                    wb_stage_count,
                } => {
                    eprintln!(
                        "[bot_engine] SMOKE COMPLETE at frame={}: start=({:.2},{:.2},{:.2}) end=({:.2},{:.2},{:.2}) total_disp={:.2}m depth_ok={} clear_f={:.1}m actors={} wb={}",
                        frame_index,
                        start_pos.x,
                        start_pos.y,
                        start_pos.z,
                        end_pos.x,
                        end_pos.y,
                        end_pos.z,
                        disp_m,
                        clearance.depth_valid as u8,
                        clearance.front_clear_m,
                        actor_count,
                        wb_stage_count
                    );
                    if let Some(w) = trace_writer.as_mut() {
                        let _ = w.flush();
                    }
                }
                BotAsyncEvent::Flush(ack) => {
                    if let Some(w) = trace_writer.as_mut() {
                        let _ = w.flush();
                    }
                    let _ = ack.send(());
                }
            }
        }
    }
}
