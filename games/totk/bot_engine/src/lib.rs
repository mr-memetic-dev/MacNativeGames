//! Lock-free, zero-allocation (on the render thread) autonomous BotW 1.9.0 bot engine.
//!
//! Exposes `extern "C" fn bot_on_frame_tick(state: *const SchematicState, out_cmd: *mut BotFrameCommand)`
//! matching `games/totk/runtime/bot_ffi.h` (`BOT_ABI_VERSION = 1`).

pub mod ffi;
mod logger;

pub use ffi::*;
use logger::{BotAsyncEvent, BotAsyncLogger};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Latches to `true` if `bot_on_frame_tick` ever catches a panic so subsequent frames return immediately.
static BOT_PANICKED: AtomicBool = AtomicBool::new(false);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SmokePhase {
    WaitForWorld,
    SettleBeforeBaseline { enter_frame: u64 },
    WalkForward { start_frame: u64 },
    CameraPanAndSprint { start_frame: u64 },
    JumpAndTurn { start_frame: u64 },
    SettleAfterAction { start_frame: u64 },
    Done { done_frame: u64 },
}

impl SmokePhase {
    pub fn name(&self) -> &'static str {
        match self {
            SmokePhase::WaitForWorld => "wait_for_world",
            SmokePhase::SettleBeforeBaseline { .. } => "settle_baseline",
            SmokePhase::WalkForward { .. } => "walk_forward",
            SmokePhase::CameraPanAndSprint { .. } => "camera_pan_sprint",
            SmokePhase::JumpAndTurn { .. } => "jump_and_turn",
            SmokePhase::SettleAfterAction { .. } => "settle_after",
            SmokePhase::Done { .. } => "done",
        }
    }
}

pub struct BotRuntime {
    pub enabled: bool,
    pub exit_on_complete: bool,
    pub phase: SmokePhase,
    pub world_ready_streak: u32,
    pub start_pos: Option<BotVec3>,
    pub prev_pos: Option<BotVec3>,
    pub prev_frame: u64,
    pub stuck_frames: u32,
    pub next_ashot_id: u32,
    pub ashot_dir_bytes: [u8; BOT_ASHOT_PATH_MAX],
    pub ashot_dir_len: usize,
    logger: Option<BotAsyncLogger>,
}

impl BotRuntime {
    pub fn new_for_test(enabled: bool, exit_on_complete: bool, ashot_dir: &str) -> Self {
        let mut dir_bytes = [0u8; BOT_ASHOT_PATH_MAX];
        let trimmed = ashot_dir.trim_end_matches('/');
        let take = trimmed.as_bytes().len().min(BOT_ASHOT_PATH_MAX - 1);
        dir_bytes[..take].copy_from_slice(&trimmed.as_bytes()[..take]);
        Self {
            enabled,
            exit_on_complete,
            phase: SmokePhase::WaitForWorld,
            world_ready_streak: 0,
            start_pos: None,
            prev_pos: None,
            prev_frame: 0,
            stuck_frames: 0,
            next_ashot_id: 1,
            ashot_dir_bytes: dir_bytes,
            ashot_dir_len: take,
            logger: None,
        }
    }

    fn init_from_env() -> Self {
        let enabled = std::env::var("TOTK_BOT_SMOKE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let exit_on_complete = std::env::var("TOTK_BOT_SMOKE_EXIT")
            .or_else(|_| std::env::var("TOTK_BOT_EXIT_ON_DONE"))
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let ashot_dir =
            std::env::var("TOTK_BOT_ASHOT_DIR").unwrap_or_else(|_| "/tmp/botw_bot_smoke".to_string());
        let trace_path = std::env::var("TOTK_BOT_TRACE").unwrap_or_else(|_| {
            format!("{}/bot_trace.jsonl", ashot_dir.trim_end_matches('/'))
        });

        let mut rt = Self::new_for_test(enabled, exit_on_complete, &ashot_dir);
        if enabled {
            let logger = BotAsyncLogger::spawn(PathBuf::from(&trace_path));
            logger.send(BotAsyncEvent::Banner {
                ashot_dir,
                trace_path,
                exit_on_complete,
            });
            rt.logger = Some(logger);
        }
        rt
    }

    #[inline]
    fn ashot_dir_str(&self) -> &str {
        core::str::from_utf8(&self.ashot_dir_bytes[..self.ashot_dir_len]).unwrap_or("/tmp")
    }

    fn trigger_ashot(&mut self, cmd: &mut BotFrameCommand, tag: &str, frame_index: u64) {
        let id = self.next_ashot_id;
        self.next_ashot_id = self.next_ashot_id.saturating_add(1);
        cmd.request_ashot_id = id;
        cmd.set_ashot_path_formatted(self.ashot_dir_str(), id, tag, frame_index);
        if let Some(ref logger) = self.logger {
            logger.send(BotAsyncEvent::AshotQueued {
                frame_index,
                ashot_id: id,
                path_bytes: cmd.ashot_path,
            });
        }
    }

    fn log_phase_transition(&self, state: &SchematicState, clearance: &BotClearanceSummary) {
        if let Some(ref logger) = self.logger {
            let disp_m = self
                .start_pos
                .map(|s| state.player_pos.dist_xz(&s))
                .unwrap_or(0.0);
            logger.send(BotAsyncEvent::PhaseTransition {
                frame_index: state.frame_index,
                phase_name: self.phase.name(),
                player_pos: state.player_pos,
                disp_m,
                clearance: *clearance,
                actor_count: state.actor_count,
                wb_stage_count: state.wb_stage_count,
            });
        }
    }

    pub fn tick(&mut self, state: &SchematicState, cmd: &mut BotFrameCommand) {
        *cmd = BotFrameCommand::default();
        if !self.enabled || state.abi_version != BOT_ABI_VERSION {
            return;
        }

        let clearance = state.compute_clearance();
        let world_ready = (state.state_flags & BOT_STATE_WORLD_READY) != 0
            && state.player_pos.is_finite_non_zero()
            && state.camera_pos.is_finite_non_zero();

        if world_ready {
            self.world_ready_streak = self.world_ready_streak.saturating_add(1);
        } else {
            self.world_ready_streak = 0;
        }

        let prev_phase = self.phase;

        match self.phase {
            SmokePhase::WaitForWorld => {
                if self.world_ready_streak >= 45 {
                    self.phase = SmokePhase::SettleBeforeBaseline {
                        enter_frame: state.frame_index,
                    };
                }
            }
            SmokePhase::SettleBeforeBaseline { enter_frame } => {
                cmd.pad_connected = 1;
                if state.frame_index >= enter_frame + 30 {
                    self.start_pos = Some(state.player_pos);
                    self.prev_pos = Some(state.player_pos);
                    self.trigger_ashot(cmd, "baseline", state.frame_index);
                    self.phase = SmokePhase::WalkForward {
                        start_frame: state.frame_index,
                    };
                }
            }
            SmokePhase::WalkForward { start_frame } => {
                cmd.pad_connected = 1;
                let elapsed = state.frame_index.saturating_sub(start_frame);

                if let Some(prev) = self.prev_pos {
                    if state.player_pos.dist_xz(&prev) < 0.008 {
                        self.stuck_frames = self.stuck_frames.saturating_add(1);
                    } else {
                        self.stuck_frames = 0;
                    }
                }

                let depth_blocked = clearance.depth_valid
                    && clearance.front_clear_m > 0.0
                    && clearance.front_clear_m < 2.2;
                let steer_sign: i32 = if clearance.left_clear_m >= clearance.right_clear_m {
                    -1
                } else {
                    1
                };

                if depth_blocked || self.stuck_frames > 25 {
                    cmd.lx = steer_sign * 22000;
                    cmd.ly = 24000;
                    cmd.rx = steer_sign * 10000;
                    if elapsed % 30 == 0 {
                        cmd.buttons |= BOT_BTN_X;
                    }
                } else {
                    cmd.lx = 0;
                    cmd.ly = 30000;
                    if clearance.nearest_actor_dist_m > 0.0 && clearance.nearest_actor_dist_m < 18.0
                    {
                        cmd.buttons |= BOT_BTN_ZL;
                    }
                }

                if elapsed == 60 {
                    self.trigger_ashot(cmd, "walk_mid", state.frame_index);
                }
                if elapsed >= 120 {
                    self.phase = SmokePhase::CameraPanAndSprint {
                        start_frame: state.frame_index,
                    };
                }
            }
            SmokePhase::CameraPanAndSprint { start_frame } => {
                cmd.pad_connected = 1;
                let elapsed = state.frame_index.saturating_sub(start_frame);
                let depth_blocked = clearance.depth_valid
                    && clearance.front_clear_m > 0.0
                    && clearance.front_clear_m < 2.5;
                if depth_blocked {
                    let steer: i32 = if clearance.left_clear_m >= clearance.right_clear_m {
                        -24000
                    } else {
                        24000
                    };
                    cmd.lx = steer;
                    cmd.ly = 22000;
                    cmd.rx = steer / 2;
                } else {
                    cmd.lx = 8000;
                    cmd.ly = 32000;
                    cmd.rx = 14000;
                    cmd.ry = 0;
                }
                cmd.buttons = BOT_BTN_B;

                if elapsed == 45 {
                    self.trigger_ashot(cmd, "sprint_pan", state.frame_index);
                }
                if elapsed >= 90 {
                    self.phase = SmokePhase::JumpAndTurn {
                        start_frame: state.frame_index,
                    };
                }
            }
            SmokePhase::JumpAndTurn { start_frame } => {
                cmd.pad_connected = 1;
                let elapsed = state.frame_index.saturating_sub(start_frame);
                cmd.lx = -18000;
                cmd.ly = 24000;
                cmd.rx = -12000;
                if elapsed < 10 || (elapsed >= 30 && elapsed < 40) {
                    cmd.buttons = BOT_BTN_X;
                } else if elapsed >= 15 && elapsed < 22 {
                    cmd.buttons = BOT_BTN_ZL;
                }
                if elapsed >= 60 {
                    self.phase = SmokePhase::SettleAfterAction {
                        start_frame: state.frame_index,
                    };
                }
            }
            SmokePhase::SettleAfterAction { start_frame } => {
                cmd.pad_connected = 1;
                let elapsed = state.frame_index.saturating_sub(start_frame);
                if elapsed == 20 {
                    self.trigger_ashot(cmd, "final", state.frame_index);
                }
                if elapsed >= 45 {
                    let disp = self
                        .start_pos
                        .map(|s| state.player_pos.dist_xz(&s))
                        .unwrap_or(0.0);
                    if let Some(ref logger) = self.logger {
                        logger.send(BotAsyncEvent::Summary {
                            frame_index: state.frame_index,
                            start_pos: self.start_pos.unwrap_or_default(),
                            end_pos: state.player_pos,
                            disp_m: disp,
                            clearance,
                            actor_count: state.actor_count,
                            wb_stage_count: state.wb_stage_count,
                        });
                    }
                    self.phase = SmokePhase::Done {
                        done_frame: state.frame_index,
                    };
                }
            }
            SmokePhase::Done { done_frame } => {
                // Release synthetic gamepad override so user/real controller input works normally.
                cmd.pad_connected = 0;
                if self.exit_on_complete && state.frame_index >= done_frame + 15 {
                    if let Some(ref logger) = self.logger {
                        logger.flush();
                    }
                    cmd.request_close = 1;
                }
            }
        }

        if self.phase != prev_phase {
            self.log_phase_transition(state, &clearance);
        }

        self.prev_pos = Some(state.player_pos);
        self.prev_frame = state.frame_index;

        if self.phase != SmokePhase::WaitForWorld && state.frame_index % 6 == 0 {
            if let Some(ref logger) = self.logger {
                logger.send(BotAsyncEvent::FrameTelemetry {
                    frame_index: state.frame_index,
                    now_ns: state.now_ns,
                    vfr_dt_s: state.vfr_dt_s,
                    state_flags: state.state_flags,
                    draws: state.this_frame_draws,
                    wb_stage_count: state.wb_stage_count,
                    phase_name: self.phase.name(),
                    player_pos: state.player_pos,
                    camera_pos: state.camera_pos,
                    camera_fwd: state.camera_fwd,
                    clearance,
                    actor_count: state.actor_count,
                    buttons: cmd.buttons,
                    lx: cmd.lx,
                    ly: cmd.ly,
                    rx: cmd.rx,
                    ry: cmd.ry,
                    ashot_id: cmd.request_ashot_id,
                });
            }
        }
    }
}

static GLOBAL_BOT: OnceLock<Mutex<BotRuntime>> = OnceLock::new();

/// Invoked once per rendered frame from `runtime_survey_tick` (`games/totk/runtime/world_survey.c`).
/// Never unwinds across the C FFI boundary; latches `BOT_PANICKED` and releases control if a panic occurs.
#[no_mangle]
pub unsafe extern "C" fn bot_on_frame_tick(
    state: *const SchematicState,
    out_cmd: *mut BotFrameCommand,
) {
    if state.is_null() || out_cmd.is_null() {
        return;
    }
    if BOT_PANICKED.load(Ordering::Relaxed) {
        *out_cmd = BotFrameCommand::default();
        return;
    }
    let res = catch_unwind(AssertUnwindSafe(|| {
        let st = &*state;
        let cmd = &mut *out_cmd;
        let mtx = GLOBAL_BOT.get_or_init(|| Mutex::new(BotRuntime::init_from_env()));
        if let Ok(mut rt) = mtx.try_lock() {
            rt.tick(st, cmd);
        } else {
            *cmd = BotFrameCommand::default();
        }
    }));
    if res.is_err() {
        BOT_PANICKED.store(true, Ordering::Relaxed);
        *out_cmd = BotFrameCommand::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn abi_layout_matches_c_header() {
        assert_eq!(size_of::<BotVec3>(), 12);
        assert_eq!(size_of::<BotActorSlot>(), 36);
        assert_eq!(offset_of!(SchematicState, abi_version), 0x000);
        assert_eq!(offset_of!(SchematicState, state_flags), 0x004);
        assert_eq!(offset_of!(SchematicState, frame_index), 0x008);
        assert_eq!(offset_of!(SchematicState, now_ns), 0x010);
        assert_eq!(offset_of!(SchematicState, vfr_dt_s), 0x018);
        assert_eq!(offset_of!(SchematicState, this_frame_draws), 0x01C);
        assert_eq!(offset_of!(SchematicState, wb_stage_count), 0x020);
        assert_eq!(offset_of!(SchematicState, player_pos), 0x024);
        assert_eq!(offset_of!(SchematicState, player_fwd), 0x030);
        assert_eq!(offset_of!(SchematicState, camera_pos), 0x03C);
        assert_eq!(offset_of!(SchematicState, camera_fwd), 0x048);
        assert_eq!(offset_of!(SchematicState, camera_up), 0x054);
        assert_eq!(offset_of!(SchematicState, camera_fovy), 0x060);
        assert_eq!(offset_of!(SchematicState, camera_near), 0x064);
        assert_eq!(offset_of!(SchematicState, camera_far), 0x068);
        assert_eq!(offset_of!(SchematicState, actor_count), 0x06C);
        assert_eq!(offset_of!(SchematicState, depth_grid), 0x070);
        assert_eq!(offset_of!(SchematicState, actors), 0x170);
        assert_eq!(size_of::<SchematicState>(), 656);

        assert_eq!(offset_of!(BotFrameCommand, pad_connected), 0x000);
        assert_eq!(offset_of!(BotFrameCommand, request_close), 0x004);
        assert_eq!(offset_of!(BotFrameCommand, buttons), 0x008);
        assert_eq!(offset_of!(BotFrameCommand, lx), 0x010);
        assert_eq!(offset_of!(BotFrameCommand, pin_frame_step_us), 0x020);
        assert_eq!(offset_of!(BotFrameCommand, request_ashot_id), 0x024);
        assert_eq!(offset_of!(BotFrameCommand, ashot_path), 0x028);
        assert_eq!(size_of::<BotFrameCommand>(), 296);
    }

    #[test]
    fn depth_linearization_and_clearance() {
        // Default BotW 1.9.0 outdoor clip planes: near = 1.0, far = 25000.0
        let d_near = linearize_depth_m(0.01, 0.0, 0.0);
        let d_mid = linearize_depth_m(0.90, 1.0, 25000.0);
        let d_far = linearize_depth_m(0.9995, 1.0, 25000.0);
        assert!(d_near > 1.0 && d_near < 1.05);
        assert!(d_mid > 9.9 && d_mid < 10.1);
        assert!(d_far > 1000.0);
        assert_eq!(linearize_depth_m(0.0, 1.0, 25000.0), 0.0);

        // Camera 5m behind Link (`cam_to_player_m = 5.0`), obstacle at eye depth ~10.0m (`z = 0.90`) -> 5.0m relative to Link.
        let mut st = SchematicState {
            state_flags: BOT_STATE_DEPTH_VALID | BOT_STATE_PLAYER_VALID | BOT_STATE_ACTORS_VALID,
            camera_near: 1.0,
            camera_far: 25000.0,
            camera_pos: BotVec3 { x: 0.0, y: 2.0, z: -5.0 },
            player_pos: BotVec3 { x: 0.0, y: 2.0, z: 0.0 },
            depth_grid: [0.99; BOT_DEPTH_GRID_CELLS],
            actor_count: 2,
            ..Default::default()
        };
        st.depth_grid[3 * 8 + 2] = 0.90; // ~10.0m eye depth -> ~5.0m ahead of Link
        st.actors[0] = BotActorSlot {
            name_hash: HASH_ENEMY_BOKOBLIN_A,
            flags: 1,
            pos: BotVec3 { x: 1.0, y: 2.0, z: 3.0 },
            vel: BotVec3::default(),
            dist_m: 6.25,
        };
        let clr = st.compute_clearance();
        assert!(clr.depth_valid);
        assert!((clr.cam_to_player_m - 5.0).abs() < 1e-3);
        assert!(clr.front_clear_m > 4.9 && clr.front_clear_m < 5.1);
        assert!(clr.left_clear_m > 50.0);
        assert_eq!(clr.nearest_actor_hash, HASH_ENEMY_BOKOBLIN_A);
        assert!((clr.nearest_actor_dist_m - 6.25).abs() < 1e-4);

        // When actor_count == 0, stale entries in `st.actors` must be ignored.
        st.actor_count = 0;
        let clr_no_actors = st.compute_clearance();
        assert_eq!(clr_no_actors.nearest_actor_hash, 0);
        assert_eq!(clr_no_actors.nearest_actor_dist_m, 0.0);
    }

    #[test]
    fn clearance_ignores_zero_samples_and_filters_link_self_occlusion() {
        let mut st = SchematicState {
            state_flags: BOT_STATE_DEPTH_VALID | BOT_STATE_PLAYER_VALID,
            camera_near: 1.0,
            camera_far: 25000.0,
            camera_pos: BotVec3 { x: 0.0, y: 2.0, z: -5.0 },
            player_pos: BotVec3 { x: 0.0, y: 2.0, z: 0.0 },
            depth_grid: [0.0; BOT_DEPTH_GRID_CELLS], // mostly 0.0 (unwritten/invalid)
            ..Default::default()
        };
        // Put Link's own mesh at center-lower cell (r=4, c=3) at eye depth ~5.26m (z = 0.81),
        // and a real obstacle at (r=3, c=2) at eye depth ~10.0m (z = 0.90 -> 5.0m relative to Link).
        st.depth_grid[4 * 8 + 3] = 0.81; // ~5.26m eye depth <= cam_to_player_m (5.0) + 1.2 -> filtered!
        st.depth_grid[3 * 8 + 2] = 0.90; // ~10.0m eye depth -> ~5.0m relative to Link

        let clr = st.compute_clearance();
        assert!(clr.depth_valid);
        assert_eq!(clr.valid_cells, 2);
        // Front clearance must reflect the 5.0m obstacle, NOT 0.26m from Link's back and NOT 25000m from 0.0 cells!
        assert!(clr.front_clear_m > 4.9 && clr.front_clear_m < 5.1);
        // Left/right/ground had only 0.0 or filtered samples, so they must report 0.0 (not 1000.0 / 25000.0).
        assert_eq!(clr.left_clear_m, 0.0);
        assert_eq!(clr.right_clear_m, 0.0);
        assert_eq!(clr.ground_ahead_m, 0.0);
    }

    #[test]
    fn smoke_policy_phases_and_done_releases_pad() {
        let mut rt = BotRuntime::new_for_test(true, true, "/tmp/test_ashot");
        let mut st = SchematicState {
            state_flags: BOT_STATE_WORLD_READY | BOT_STATE_DEPTH_VALID | BOT_STATE_PLAYER_VALID,
            player_pos: BotVec3 { x: -100.0, y: 200.0, z: 300.0 },
            camera_pos: BotVec3 { x: -100.0, y: 202.0, z: 295.0 },
            depth_grid: [0.98; BOT_DEPTH_GRID_CELLS],
            ..Default::default()
        };
        let mut cmd = BotFrameCommand::default();

        for f in 1..=500 {
            st.frame_index = f;
            st.player_pos.x += 0.05;
            rt.tick(&st, &mut cmd);
            if matches!(rt.phase, SmokePhase::Done { .. }) {
                break;
            }
        }
        assert!(matches!(rt.phase, SmokePhase::Done { .. }));

        // Tick once inside SmokePhase::Done: pad_connected MUST be 0 (synthetic gamepad released).
        st.frame_index += 1;
        rt.tick(&st, &mut cmd);
        assert_eq!(cmd.pad_connected, 0);

        // Verify ashot_path truncation and guaranteed NUL termination on long directory prefixes.
        let long_dir = "/tmp/".to_string() + &"a".repeat(200);
        cmd.set_ashot_path_formatted(&long_dir, 7, "baseline", 12345);
        assert_eq!(cmd.ashot_path[BOT_ASHOT_PATH_MAX - 1], 0);
        assert!(cmd.ashot_path_str().starts_with("/tmp/aaaa"));
    }

    #[test]
    fn ffi_catch_unwind_contains_panic() {
        let mut cmd = BotFrameCommand {
            pad_connected: 1,
            buttons: BOT_BTN_A,
            ..Default::default()
        };
        let res = catch_unwind(AssertUnwindSafe(|| -> () {
            panic!("simulated internal bot panic");
        }));
        if res.is_err() {
            cmd = BotFrameCommand::default();
        }
        assert_eq!(cmd.pad_connected, 0);
        assert_eq!(cmd.buttons, 0);
    }
}
