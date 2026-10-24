#pragma once
#include <TargetConditionals.h>
#include <stdint.h>

/* C <-> Rust FastHands / smoke-bot FFI seam (macOS only; iOS never links libbotw_bot.a).
 * Keep #[repr(C)] layout identical to `games/totk/bot_engine/src/lib.rs`.
 *
 * Overview:
 *   - Invoked once per presented frame from `runtime_survey_tick` (`games/totk/runtime/world_survey.c`)
 *     when `TOTK_SURVEY=1` or `TOTK_BOT_SMOKE=1` is set on Breath of the Wild 1.9.0.
 *   - Reads guest memory non-blockingly via `mach_vm_read_overwrite` (`VFR`, `Camera`, `PlayerMgr`,
 *     and `BaseProcMgr` active actor RB-tree) and reads the latest completed GPU 8x8 scene depth
 *     stage buffer via `mtl_bot_depth_grid` (zero GPU wait).
 *   - See `games/totk/bot_engine/README.md` for full documentation of all connector env vars,
 *     sensors, and JSONL telemetry formats.
 */
#if TARGET_OS_OSX

enum {
    BOT_ABI_VERSION      = 1,
    BOT_ACTOR_SLOTS      = 8,
    BOT_DEPTH_GRID_W     = 8,
    BOT_DEPTH_GRID_H     = 8,
    BOT_DEPTH_GRID_CELLS = 64,
    BOT_ASHOT_PATH_MAX   = 256,
};

/* Bitflags in `SchematicState.state_flags`. */
enum {
    BOT_STATE_WORLD_READY  = (1u << 0), /* 3D scene pass active (`this_frame_draws >= 400`) */
    BOT_STATE_DEPTH_VALID  = (1u << 1), /* `depth_grid` holds a completed GPU 8x8 G-buffer depth snapshot */
    BOT_STATE_PLAYER_VALID = (1u << 2), /* `PlayerMgr::sInstance` Link actor resolved in guest memory */
    BOT_STATE_ACTORS_VALID = (1u << 3), /* `BaseProcMgr::sInstance` actor tree traversed in guest memory */
};

/* 3D vector in Breath of the Wild world coordinates (meters; +Y is vertical up). */
typedef struct {
    float x, y, z;
} BotVec3;

/* Snapshot of one active nearby actor from `ksys::act::BaseProcMgr::sInstance`
 * (`main + 0x1e937f8`), sorted in ascending order of `dist_m` from Link.
 * Size: 36 bytes. */
typedef struct {
    uint32_t name_hash; /* FNV-1a 32-bit hash of ASCII actor name at `node+0x30` (or `node+0x20` CRC32) */
    uint32_t flags;     /* `BaseProc` state/flags at `+0x24/+0x28` (`(mState << 24) | (mFlags & 0x00ffffff)`) */
    BotVec3  pos;       /* World position (m) from `Actor::mMtx` (`sead::Matrix34f`) at `BaseProc + 0x398` */
    BotVec3  vel;       /* World linear velocity (m/s) at `BaseProc + 0x400` */
    float    dist_m;    /* Euclidean 3D distance from `player_pos` (m) */
} BotActorSlot;

/* Per-frame world, depth-grid, and nearby-entity snapshot passed from C to Rust.
 * Size: 656 bytes. */
typedef struct {
    uint32_t     abi_version;                        /* 0x000: BOT_ABI_VERSION (1) */
    uint32_t     state_flags;                        /* 0x004: Bitmask of `BOT_STATE_*` */
    uint64_t     frame_index;                        /* 0x008: Monotonic presented frame number */
    uint64_t     now_ns;                             /* 0x010: Host monotonic timestamp (ns) */
    float        vfr_dt_s;                           /* 0x018: Frame delta time in seconds (`main+0x1e93890`) */
    uint32_t     this_frame_draws;                   /* 0x01C: Metal draw calls submitted in this frame */
    uint32_t     wb_stage_count;                     /* 0x020: Cumulative async GPU writeback stages completed */
    BotVec3      player_pos;                         /* 0x024: Link world position (m) */
    BotVec3      player_fwd;                         /* 0x030: Link unit forward vector */
    BotVec3      camera_pos;                         /* 0x03C: Active camera eye position (m) */
    BotVec3      camera_fwd;                         /* 0x048: Active camera unit forward vector */
    BotVec3      camera_up;                          /* 0x054: Active camera up vector */
    float        camera_fovy;                        /* 0x060: Vertical field of view (radians) */
    float        camera_near;                        /* 0x064: Camera near clip plane (m, default 0.1) */
    float        camera_far;                         /* 0x068: Camera far clip plane (m, default 2500.0) */
    uint32_t     actor_count;                        /* 0x06C: Total active spatial actors within 500m of Link */
    float        depth_grid[BOT_DEPTH_GRID_CELLS];   /* 0x070: 8x8 row-major (top-to-bottom, left-to-right) normalized
                                                      *        hardware depth in [0.0, 1.0] from the 3D G-buffer depth pass */
    BotActorSlot actors[BOT_ACTOR_SLOTS];            /* 0x170: Up to 8 nearest active entities around Link */
} SchematicState;

/* Per-frame controller override & host action command returned from Rust to C.
 * Size: 296 bytes. */
typedef struct {
    int32_t  pad_connected;                  /* 1 = override synthetic gamepad via `runtime_input_pad_synthetic` */
    uint8_t  request_close;                  /* 1 = request clean app exit via `runtime_front_request_close` */
    uint8_t  reserved[3];
    uint64_t buttons;                        /* HidNpad button bitmask (A=1, B=2, X=4, Y=8, LStick=16, RStick=32, L=64, R=128, ZL=256, ZR=512, Plus=1024, Minus=2048) */
    int32_t  lx, ly, rx, ry;                 /* Stick axes in [-32767, 32767] */
    uint32_t pin_frame_step_us;              /* Optional frame-step pin in microseconds (0 = unpinned) */
    uint32_t request_ashot_id;               /* Non-zero = capture async PNG screenshot to `ashot_path` */
    char     ashot_path[BOT_ASHOT_PATH_MAX]; /* NUL-terminated output PNG path for `dm_gfx_ashot` */
} BotFrameCommand;

_Static_assert(sizeof(BotVec3) == 12, "BotVec3 ABI size");
_Static_assert(sizeof(BotActorSlot) == 36, "BotActorSlot ABI size");
_Static_assert(sizeof(SchematicState) == 656, "SchematicState ABI size");
_Static_assert(sizeof(BotFrameCommand) == 296, "BotFrameCommand ABI size");

/* Weak import: NULL when build/libbotw_bot.a is absent; never linked into probe-* targets. */
extern void bot_on_frame_tick(const SchematicState *state, BotFrameCommand *cmd)
    __attribute__((weak_import));

#endif /* TARGET_OS_OSX */
