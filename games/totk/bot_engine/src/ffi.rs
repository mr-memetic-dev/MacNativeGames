//! Lock-free `#[repr(C)]` FFI types and depth/actor geometry helpers for `bot_ffi.h`.

use core::fmt::Write as FmtWrite;

pub const BOT_ABI_VERSION: u32 = 1;

pub const BOT_STATE_WORLD_READY: u32 = 1 << 0;
pub const BOT_STATE_DEPTH_VALID: u32 = 1 << 1;
pub const BOT_STATE_PLAYER_VALID: u32 = 1 << 2;
pub const BOT_STATE_ACTORS_VALID: u32 = 1 << 3;

pub const BOT_BTN_A: u64 = 1 << 0;
pub const BOT_BTN_B: u64 = 1 << 1;
pub const BOT_BTN_X: u64 = 1 << 2;
pub const BOT_BTN_Y: u64 = 1 << 3;
pub const BOT_BTN_LSTICK: u64 = 1 << 4;
pub const BOT_BTN_RSTICK: u64 = 1 << 5;
pub const BOT_BTN_L: u64 = 1 << 6;
pub const BOT_BTN_R: u64 = 1 << 7;
pub const BOT_BTN_ZL: u64 = 1 << 8;
pub const BOT_BTN_ZR: u64 = 1 << 9;
pub const BOT_BTN_PLUS: u64 = 1 << 10;
pub const BOT_BTN_MINUS: u64 = 1 << 11;
pub const BOT_BTN_DLEFT: u64 = 1 << 12;
pub const BOT_BTN_DUP: u64 = 1 << 13;
pub const BOT_BTN_DRIGHT: u64 = 1 << 14;
pub const BOT_BTN_DDOWN: u64 = 1 << 15;

pub const BOT_DEPTH_GRID_W: usize = 8;
pub const BOT_DEPTH_GRID_H: usize = 8;
pub const BOT_DEPTH_GRID_CELLS: usize = BOT_DEPTH_GRID_W * BOT_DEPTH_GRID_H;
pub const BOT_ACTOR_SLOTS: usize = 8;
pub const BOT_ASHOT_PATH_MAX: usize = 256;

/// Well-known FNV-1a-32 actor name hashes in BotW 1.9.0.
pub const HASH_GAME_ROM_PLAYER: u32 = 0x0c34_2f65; // "GameRomPlayer"
pub const HASH_PLAYER_STOLE2: u32 = 0x3e62_889b; // "PlayerStole2"
pub const HASH_ENEMY_BOKOBLIN_A: u32 = 0x89f9_be93; // "Enemy_Bokoblin_Junior"

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct BotVec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl BotVec3 {
    #[inline]
    pub fn norm_xz(&self) -> f32 {
        (self.x * self.x + self.z * self.z).sqrt()
    }

    #[inline]
    pub fn dist(&self, other: &Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    #[inline]
    pub fn dist_xz(&self, other: &Self) -> f32 {
        let dx = self.x - other.x;
        let dz = self.z - other.z;
        (dx * dx + dz * dz).sqrt()
    }

    #[inline]
    pub fn is_finite_non_zero(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.z.is_finite()
            && (self.x.abs() > 0.01 || self.y.abs() > 0.01 || self.z.abs() > 0.01)
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct BotActorSlot {
    pub name_hash: u32,
    pub flags: u32,
    pub pos: BotVec3,
    pub vel: BotVec3,
    pub dist_m: f32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct SchematicState {
    pub abi_version: u32,
    pub state_flags: u32,
    pub frame_index: u64,
    pub now_ns: u64,
    pub vfr_dt_s: f32,
    pub this_frame_draws: u32,
    pub wb_stage_count: u32,
    pub player_pos: BotVec3,
    pub player_fwd: BotVec3,
    pub camera_pos: BotVec3,
    pub camera_fwd: BotVec3,
    pub camera_up: BotVec3,
    pub camera_fovy: f32,
    pub camera_near: f32,
    pub camera_far: f32,
    pub actor_count: u32,
    pub depth_grid: [f32; BOT_DEPTH_GRID_CELLS],
    pub actors: [BotActorSlot; BOT_ACTOR_SLOTS],
}

impl Default for SchematicState {
    fn default() -> Self {
        Self {
            abi_version: BOT_ABI_VERSION,
            state_flags: 0,
            frame_index: 0,
            now_ns: 0,
            vfr_dt_s: 0.0,
            this_frame_draws: 0,
            wb_stage_count: 0,
            player_pos: BotVec3::default(),
            player_fwd: BotVec3::default(),
            camera_pos: BotVec3::default(),
            camera_fwd: BotVec3::default(),
            camera_up: BotVec3::default(),
            camera_fovy: 0.0,
            camera_near: 1.0,
            camera_far: 25000.0,
            actor_count: 0,
            depth_grid: [0.0; BOT_DEPTH_GRID_CELLS],
            actors: [BotActorSlot::default(); BOT_ACTOR_SLOTS],
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct BotFrameCommand {
    pub pad_connected: i32,
    pub request_close: u8,
    pub reserved: [u8; 3],
    pub buttons: u64,
    pub lx: i32,
    pub ly: i32,
    pub rx: i32,
    pub ry: i32,
    pub pin_frame_step_us: u32,
    pub request_ashot_id: u32,
    pub ashot_path: [u8; BOT_ASHOT_PATH_MAX],
}

impl Default for BotFrameCommand {
    fn default() -> Self {
        Self {
            pad_connected: 0,
            request_close: 0,
            reserved: [0; 3],
            buttons: 0,
            lx: 0,
            ly: 0,
            rx: 0,
            ry: 0,
            pin_frame_step_us: 0,
            request_ashot_id: 0,
            ashot_path: [0u8; BOT_ASHOT_PATH_MAX],
        }
    }
}

struct SliceCursor<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> FmtWrite for SliceCursor<'a> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let cap = self.buf.len().saturating_sub(1); // keep last byte for NUL
        let rem = cap.saturating_sub(self.pos);
        let take = bytes.len().min(rem);
        if take > 0 {
            self.buf[self.pos..self.pos + take].copy_from_slice(&bytes[..take]);
            self.pos += take;
        }
        Ok(())
    }
}

impl BotFrameCommand {
    /// Zero-allocation formatter that writes `<dir>/bot_<id:02>_<tag>_f<frame>.png\0` directly
    /// into `self.ashot_path` and guarantees NUL termination at or before `BOT_ASHOT_PATH_MAX - 1`.
    pub fn set_ashot_path_formatted(
        &mut self,
        dir_prefix: &str,
        id: u32,
        tag: &str,
        frame_index: u64,
    ) {
        self.ashot_path = [0u8; BOT_ASHOT_PATH_MAX];
        let mut cur = SliceCursor {
            buf: &mut self.ashot_path,
            pos: 0,
        };
        let _ = write!(cur, "{}/bot_{:02}_{}_f{}.png", dir_prefix, id, tag, frame_index);
        let nul_idx = cur.pos.min(BOT_ASHOT_PATH_MAX - 1);
        self.ashot_path[nul_idx] = 0;
    }

    #[inline]
    pub fn ashot_path_str(&self) -> &str {
        let nul = self
            .ashot_path
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(BOT_ASHOT_PATH_MAX);
        core::str::from_utf8(&self.ashot_path[..nul]).unwrap_or("")
    }
}

/// Summary of the 8x8 depth grid and nearby actors used by the tactical navigation policy.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct BotClearanceSummary {
    pub depth_valid: bool,
    pub valid_cells: u32,
    pub cam_to_player_m: f32,
    pub front_clear_m: f32,
    pub left_clear_m: f32,
    pub right_clear_m: f32,
    pub ground_ahead_m: f32,
    pub nearest_actor_dist_m: f32,
    pub nearest_actor_hash: u32,
}

/// Linearizes a raw `[0, 1]` NDC depth sample into eye-space distance in meters.
/// Handles standard `[0, 1]` depth (`z = 0` at `near`, `z = 1` at `far`), clamping to `[near, far]`.
/// Uses BotW 1.9.0 outdoor clip defaults (`near = 1.0`, `far = 25000.0`) when clip planes are invalid.
#[inline]
pub fn linearize_depth_m(raw_z: f32, near_clip: f32, far_clip: f32) -> f32 {
    if !raw_z.is_finite() || raw_z <= 0.0 || raw_z > 1.0 {
        return 0.0;
    }
    let near = if near_clip.is_finite() && near_clip > 0.001 && near_clip <= 100.0 {
        near_clip
    } else {
        1.0
    };
    let far = if far_clip.is_finite() && far_clip > near && far_clip <= 100_000.0 {
        far_clip
    } else {
        25000.0
    };
    let z = raw_z.clamp(0.0, 1.0);
    let denom = far - z * (far - near);
    if denom <= 1e-6 {
        far
    } else {
        ((near * far) / denom).clamp(near, far)
    }
}

impl SchematicState {
    /// Computes front/left/right/ground clearance in meters relative to Link (`eye_depth - cam_to_player_m`)
    /// from the 8x8 depth grid and summarizes the nearest non-player actor.
    ///
    /// - Never substitutes `far` for invalid (`0.0` / `NaN`) depth samples; only valid samples contribute.
    /// - Subtracts the 3rd-person camera-to-player distance (`cam_to_player_m`, clamped to `0..=15 m`) so
    ///   clearance thresholds are relative to Link rather than the camera eye.
    /// - Filters out center-lower self-occlusion cells (`r in 4..=6, c in 3..=4`) whose eye depth is within
    ///   `cam_to_player_m + 1.2 m` so Link's own mesh never masks obstacles or cliffs ahead of him.
    pub fn compute_clearance(&self) -> BotClearanceSummary {
        let mut out = BotClearanceSummary::default();
        if (self.state_flags & BOT_STATE_DEPTH_VALID) != 0 {
            let cam_to_player_m = if (self.state_flags & BOT_STATE_PLAYER_VALID) != 0
                && self.player_pos.is_finite_non_zero()
                && self.camera_pos.is_finite_non_zero()
            {
                self.player_pos.dist(&self.camera_pos).clamp(0.0, 15.0)
            } else {
                0.0
            };
            out.cam_to_player_m = cam_to_player_m;

            let mut front_min: Option<f32> = None;
            let mut left_min: Option<f32> = None;
            let mut right_min: Option<f32> = None;
            let mut ground_min: Option<f32> = None;
            let mut valid = 0u32;

            for r in 0..BOT_DEPTH_GRID_H {
                for c in 0..BOT_DEPTH_GRID_W {
                    let raw = self.depth_grid[r * BOT_DEPTH_GRID_W + c];
                    let eye_d = linearize_depth_m(raw, self.camera_near, self.camera_far);
                    if eye_d <= 0.0 || !eye_d.is_finite() {
                        continue;
                    }
                    valid += 1;

                    // Filter 3rd-person Link self-occlusion in the center-lower cells.
                    if (4..=6).contains(&r)
                        && (3..=4).contains(&c)
                        && cam_to_player_m > 0.5
                        && eye_d <= cam_to_player_m + 1.2
                    {
                        continue;
                    }

                    let rel_d = (eye_d - cam_to_player_m).max(0.0);
                    if (2..=4).contains(&r) && (2..=5).contains(&c) {
                        front_min = Some(front_min.map_or(rel_d, |m| m.min(rel_d)));
                    }
                    if (2..=5).contains(&r) && c <= 1 {
                        left_min = Some(left_min.map_or(rel_d, |m| m.min(rel_d)));
                    }
                    if (2..=5).contains(&r) && c >= 6 {
                        right_min = Some(right_min.map_or(rel_d, |m| m.min(rel_d)));
                    }
                    if r >= 5 && (2..=5).contains(&c) {
                        ground_min = Some(ground_min.map_or(rel_d, |m| m.min(rel_d)));
                    }
                }
            }
            if valid > 0 {
                out.depth_valid = true;
                out.valid_cells = valid;
                out.front_clear_m = front_min.unwrap_or(0.0);
                out.left_clear_m = left_min.unwrap_or(0.0);
                out.right_clear_m = right_min.unwrap_or(0.0);
                out.ground_ahead_m = ground_min.unwrap_or(0.0);
            }
        }

        if (self.state_flags & BOT_STATE_ACTORS_VALID) != 0 && self.actor_count > 0 {
            let slots = (self.actor_count as usize).min(BOT_ACTOR_SLOTS);
            for slot in &self.actors[..slots] {
                if slot.name_hash == 0
                    || slot.name_hash == HASH_GAME_ROM_PLAYER
                    || slot.name_hash == HASH_PLAYER_STOLE2
                    || slot.dist_m <= 0.05
                    || !slot.dist_m.is_finite()
                {
                    continue;
                }
                if out.nearest_actor_dist_m == 0.0 || slot.dist_m < out.nearest_actor_dist_m {
                    out.nearest_actor_dist_m = slot.dist_m;
                    out.nearest_actor_hash = slot.name_hash;
                }
            }
        }
        out
    }
}
