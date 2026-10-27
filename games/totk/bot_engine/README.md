# Breath of the Wild 1.9.0 (`GAME_BOTW190`) — FastHands / Bot Connector (`bot_engine`)

Zero-stall C ↔ Rust telemetry, sensor, and controller-injection connector for **The Legend of Zelda: Breath of the Wild 1.9.0** (`GAME_BOTW190`) on macOS (`totk-native-botw190` (`make -C games/totk bot-engine runtime-botw190`)).

---

## 1. Architecture & Zero-Stall Design

The connector spans three components linked into `build/runtime-botw190`:

1. **Metal G-Buffer Depth Sensor (`games/totk/third_party/d2r_gfx/gfx/nvn/nvn_mtl.m`)**:
   - `BOTW_DEPTH_NOTE(pd)` identifies the 16:9 3D scene G-buffer depth pass (`MTLPixelFormatDepth32Float` / `MTLPixelFormatDepth32Float_Stencil8`, width ≥ 640, matching `totk_taa.m`).
   - At command-buffer commit time (`botw_wb_stage_encode` → `botw_depth_stage_encode`), a single `MTLBlitCommandEncoder` copies an `8×8` (`64` floats) grid of `1×1` depth texels into an 8-slot `MTLResourceStorageModeShared` ring buffer (`g_bot_depth_ring[8]`) tagged with the submission fence (`g_fence + 1`).
   - `mtl_bot_depth_grid(float out_grid[64], uint32_t *out_wb_staged)` reads the newest ring slot whose fence has already completed on the GPU (`fence <= g_done`) inside an `@autoreleasepool` with **0.0 ms CPU/GPU wait**.

2. **Guest Memory World & Actor Survey (`games/totk/runtime/world_survey.c`)**:
   - Invoked once per presented frame (`runtime_survey_tick`) when `TOTK_SURVEY=1` or `TOTK_BOT_SMOKE=1` is enabled.
   - Uses crash-safe `mach_vm_read_overwrite` (`read_mem_190`) to read:
     - **VFR singleton** (`main + 0x1e93890`): frame ratio (`+0x3c`) and delta time (`*(+0x138)`).
     - **Camera singleton** (`main + 0x1e39d10`): active view (`+0x28`), projection (`+0x38`, `near` at `+0x98`, `far` at `+0x9c`, `fovy` at `+0xa0`, `aspect` at `+0xb0`), eye position (`+0x38`), look-at (`+0x44`), and up vector (`+0x50`).
     - **`uking::PlayerMgr::sInstance`** (`main + 0x1e13ed0`): resolves Link's `Player` actor pointer (`*(+0x60)`) and reads world position from `Actor::mMtx` (`sead::Matrix34f` at `BaseProc + 0x398`).
     - **`ksys::act::BaseProcMgr::sInstance`** (`main + 0x1e937f8`): traverses the active actor red-black tree (`root = *(bpm + 0x98)`, where each `node` is embedded at `BaseProc + 0x118` with duplicate-name siblings chained at `node + 0x48`, up to 512 nodes), extracts each actor's ASCII name at `node + 0x30` / `BaseProc + 0x08` (hashed with FNV-1a 32-bit, or `node + 0x20` CRC32 fallback), `BaseProc` state/flags (`+0x24` / `+0x28`), world position (`mMtx` translation at `BaseProc + 0x398`), and linear velocity (`BaseProc + 0x400`), and populates the 8 nearest actors (`SchematicState.actors[0..8]`) sorted by ascending 3D distance `dist_m` from Link.
   - Calls weak-imported `bot_on_frame_tick(&st, &cmd)` (`games/totk/runtime/bot_ffi.h`) and applies the returned `BotFrameCommand` (`runtime_input_pad_synthetic`, `runtime_frame_step_pin`, `dm_gfx_ashot`, `runtime_front_request_close`).

3. **Rust Policy & Telemetry Engine (`games/totk/bot_engine/src/lib.rs`, `build/libbotw_bot.a`)**:
   - Linearizes the `8×8` hardware depth grid (`z ∈ [0, 1]`, `0` = near, `1` = far) into meters via `linearize_depth_m(z, near, far)` and computes [`TerrainClearance`] (`front_clear_m`, `left_clear_m`, `right_clear_m`, `ground_ahead_m`, `cliff_drop_ahead`, `nearest_entity_dist_m`, `nearest_entity_hash`).
   - Runs `DeterministicSmokePolicy` (`WaitWorld` → `Settle` → `Walk` → `Pan` → `Action` → `Done`) with depth-aware obstacle/cliff steering during `Walk`.
   - Offloads all `[bot-hb]` `stderr` logs and `TOTK_BOT_TRACE` JSONL disk writes to a dedicated background thread (`botw-bot-io`) over a bounded `sync_channel(256)` (`try_send` only) so `bot_on_frame_tick` never blocks or allocates on the render thread.

---

## 2. Environment Variables (Existing & New Capabilities)

| Environment Variable | Type / Example | Description |
| :--- | :--- | :--- |
| `TOTK_SURVEY` | `0` or `1` | Enables `world_survey.c` per-frame telemetry (`[survey]` log every 300 frames) and invokes `bot_on_frame_tick`. Refuses to run if `TOTK_SLOT_OWNER=user`. |
| `TOTK_BOT_SMOKE` | `0` or `1` | Implicitly enables `world_survey.c`, activates `DeterministicSmokePolicy` in `libbotw_bot.a` (`WaitWorld` → `Settle` → `Walk` → `Pan` → `Action` → `Done`), and runs completely in the background by default (`orderOut` + `NSApplicationActivationPolicyAccessory` + `NSActivityUserInitiated` App-Nap guard + present/display-link bypass) without stealing keyboard focus or stalling when occluded/minimized. |
| `TOTK_SHOW_WINDOW` | `0` or `1` | When set to `1` alongside `TOTK_BOT_SMOKE=1`, shows the game window (`orderFrontRegardless`, still without stealing keyboard focus or clearing synthetic inputs on focus loss) while surviving occlusion or manual minimization at full speed. |
| `TOTK_MINIMIZE` | `0` or `1` | Starts the game window minimized in the Dock (`[window miniaturize:nil]`) with `CAMetalDisplayLink`/`nextDrawable` bypassed so offscreen rendering, depth readback, and `ashot` PNG captures run at full speed. |
| `TOTK_HEADLESS` | `0` or `1` | Keeps the window ordered out (`[window orderOut:nil]`) as an accessory background process with `CAMetalDisplayLink`/`nextDrawable` bypassed. |
| `TOTK_BOT_EXIT_ON_DONE` | `0` or `1` | When `1`, `DeterministicSmokePolicy` sets `cmd.request_close = 1` 30 frames after reaching `SmokePhase::Done` for automated headless/windowed test runs. |
| `TOTK_BOT_ASHOT_DIR` | Path (default `$HOME/Library/Logs/MacNativeGames/botw_bot_smoke` on macOS) | Directory where `DeterministicSmokePolicy` saves phase-transition PNG screenshots (`bot_smoke_<phase>_<frame>.png`) via `dm_gfx_ashot`. Validated with `lstat` (non-symlink, owned by `geteuid()`, mode `0700`); `/tmp/...` may still be passed explicitly if it passes validation. |
| `TOTK_BOT_TRACE` | File path (default `<TOTK_BOT_ASHOT_DIR>/bot_trace.jsonl`) | Writes RFC 8259 JSONL telemetry records (`NaN`/`Inf`-sanitized) opened with `O_NOFOLLOW` and mode `0600` inside a validated `0700` parent directory. |
| `TOTK_AUTOLOAD` | `0` or `1` | Automatically advances past the BotW title screen into the most recent save slot (`games/totk/runtime/stubs.c`). |
| `TOTK_SAVE_DIR` | Directory path | Overrides the BotW save directory (`nand/user/save/...`). |
| `TOTK_INPUT_FILE` | File path (e.g. `/tmp/botw_input.cmd`) | Polled live command file supporting `pad <buttons_hex> <lx> <ly> <rx> <ry>`, `padclear`, `ashot <path>`, `survey ...`, and `quit`. |
| `TOTK_ASHOT_EVERY` | Frame count `N` | Automatically captures an async PNG screenshot every `N` presented frames. |
| `TOTK_SLOT_OWNER` | `agent` / `user` | Safety guard: `TOTK_SURVEY` and `TOTK_BOT_SMOKE` abort immediately if `TOTK_SLOT_OWNER=user`. |

---

## 3. C ↔ Rust FFI Contract (`games/totk/runtime/bot_ffi.h`)

Compile-time assertions in both C (`_Static_assert`) and Rust (`const _: () = assert!(...)`) enforce identical struct sizes and field offsets:

- `sizeof(BotVec3) == 12`
- `sizeof(BotActorSlot) == 36`
- `sizeof(SchematicState) == 656`
- `sizeof(BotFrameCommand) == 296`

### `SchematicState` (`656` bytes, C → Rust)

| Offset | Field | Type | Description |
| :--- | :--- | :--- | :--- |
| `0x000` | `abi_version` | `uint32_t` | Must equal `BOT_ABI_VERSION` (`1`). |
| `0x004` | `state_flags` | `uint32_t` | Bitmask: `BOT_STATE_WORLD_READY` (`1<<0`), `BOT_STATE_DEPTH_VALID` (`1<<1`), `BOT_STATE_PLAYER_VALID` (`1<<2`), `BOT_STATE_ACTORS_VALID` (`1<<3`). |
| `0x008` | `frame_index` | `uint64_t` | Monotonic presented frame counter. |
| `0x010` | `now_ns` | `uint64_t` | Host monotonic timestamp in nanoseconds (`runtime_now_ns()`). |
| `0x018` | `vfr_dt_s` | `float` | Guest VFR frame delta time in seconds (`main + 0x1e93890`). |
| `0x01C` | `this_frame_draws` | `uint32_t` | Metal draw calls submitted during this frame (`>= 400` in 3D world). |
| `0x020` | `wb_stage_count` | `uint32_t` | Cumulative GPU writeback/depth stages encoded. |
| `0x024` | `player_pos` | `BotVec3` | Link world position `(x, y, z)` in meters (`+Y` is up). |
| `0x030` | `player_fwd` | `BotVec3` | Forward unit vector `(x, y, z)`. |
| `0x03C` | `camera_pos` | `BotVec3` | Active camera eye position `(x, y, z)` in meters. |
| `0x048` | `camera_fwd` | `BotVec3` | Active camera forward unit vector `(x, y, z)`. |
| `0x054` | `camera_up` | `BotVec3` | Active camera up vector `(x, y, z)`. |
| `0x060` | `camera_fovy` | `float` | Vertical field of view in radians. |
| `0x064` | `camera_near` | `float` | Camera near clip plane in meters (`proj + 0x98`, default `0.1`). |
| `0x068` | `camera_far` | `float` | Camera far clip plane in meters (`proj + 0x9c`, default `2500.0`). |
| `0x06C` | `actor_count` | `uint32_t` | Total active spatial actors within 500 m of Link in `BaseProcMgr`. |
| `0x070` | `depth_grid[64]` | `float[64]` | `8×8` row-major (`r = 0..7` top-to-bottom, `c = 0..7` left-to-right) normalized hardware depth in `[0.0, 1.0]` from the 3D G-buffer depth pass. |
| `0x170` | `actors[8]` | `BotActorSlot[8]` | Up to 8 nearest active actors sorted by ascending 3D distance `dist_m` from Link. |

### `BotActorSlot` (`36` bytes)

| Offset | Field | Type | Description |
| :--- | :--- | :--- | :--- |
| `0x00` | `name_hash` | `uint32_t` | FNV-1a 32-bit hash of the ASCII actor name at `node + 0x30` (or `node + 0x20` CRC32 fallback). |
| `0x04` | `flags` | `uint32_t` | Packed `BaseProc` state/flags: `((uint32_t)mState << 24) \| (mFlags & 0x00ffffff)`. |
| `0x08` | `pos` | `BotVec3` | World position `(x, y, z)` in meters (`Actor::mMtx` translation at `BaseProc + 0x398`). |
| `0x14` | `vel` | `BotVec3` | World linear velocity `(vx, vy, vz)` in m/s (`BaseProc + 0x400`). |
| `0x20` | `dist_m` | `float` | Euclidean 3D distance in meters from `player_pos`. |

### `BotFrameCommand` (`296` bytes, Rust → C)

| Offset | Field | Type | Description |
| :--- | :--- | :--- | :--- |
| `0x000` | `pad_connected` | `int32_t` | `1` to inject synthetic gamepad state via `runtime_input_pad_synthetic`. |
| `0x004` | `request_close` | `uint8_t` | `1` to request clean window/process close via `runtime_front_request_close`. |
| `0x005` | `reserved[3]` | `uint8_t[3]` | Alignment padding (zeroed). |
| `0x008` | `buttons` | `uint64_t` | HidNpad button bitmask (`BTN_A=1`, `BTN_B=2`, `BTN_X=4`, `BTN_Y=8`, etc.). |
| `0x010` | `lx`, `ly`, `rx`, `ry` | `int32_t` × 4 | Left/right analog stick axes in `[-32767, 32767]`. |
| `0x020` | `pin_frame_step_us` | `uint32_t` | Optional frame-step pin in microseconds (`0` = unpinned). |
| `0x024` | `request_ashot_id` | `uint32_t` | Non-zero triggers an async PNG screenshot to `ashot_path`. |
| `0x028` | `ashot_path[256]` | `char[256]` | NUL-terminated output PNG path for `dm_gfx_ashot`. |

---

## 4. Log & Telemetry Formats

### `[survey]` Log (`games/totk/runtime/world_survey.c`, every 300 frames)

```text
[survey] frame=1500 vfr=0x1105bf640 ratio=2.0000 delta=0.06667 cam=0x11216d6d0 view0=0x11216d6f8 view1=0x11216d7e8 proj=0x11216d708 aspect=1.7778 fovy=0.8727 near=0.10 far=2500.0 pos=-1130.65,239.88,1911.09 at=-1128.43,238.49,1914.61 player=-1129.11,238.03,1913.84 draws=912 depth_valid=1 d_center=0.99124 d_ground=0.96410 wb=1420 actors=24 top0=Obj_TreeApple_A_M_01(4.2m) top1=Item_Fruit_A(4.5m)
```

### `[bot_engine]` Phase Transitions & Summary (`games/totk/bot_engine/src/logger.rs`)

```text
[bot_engine] active: ABI v1 smoke=1 ashot_dir=/tmp/botw_bot_smoke trace=/tmp/botw_bot_smoke/bot_trace.jsonl exit_on_complete=1
[bot_engine] frame=2112 phase=settle_baseline pos=(-940.66,273.96,2330.39) disp=0.00m depth_ok=1 clear_f=204.0m clear_l=8.1m clear_r=0.0m ground=0.0m actors=100 near_act=1.1m(0xf5636347) wb=1026
[bot_engine] frame=2460 SMOKE COMPLETE: start=(-940.66,273.96,2330.39) end=(-941.88,273.40,2337.77) disp_xz=7.48m depth_ok=1 valid_cells=64 clear_f=193.7m actors=103 near_act=1.4m(0xbb0ed8e7) wb=5648
```

### `TOTK_BOT_TRACE` JSONL Schema (`games/totk/bot_engine/src/logger.rs`)

```json
{"frame":2460,"now_ns":3427494687684083,"dt_s":0.00884,"flags":15,"draws":2361,"wb":5648,"phase":"done","player":[-941.884,273.399,2337.767],"cam_pos":[-939.214,275.339,2331.839],"cam_fwd":[-0.3878,0.0000,0.9217],"depth_ok":true,"valid_cells":64,"clear_front_m":193.68,"clear_left_m":150.78,"clear_right_m":4.52,"ground_m":0.00,"actors":103,"nearest_actor_m":1.45,"nearest_actor_hash":"0xbb0ed8e7","buttons":0,"lx":0,"ly":0,"rx":0,"ry":0,"ashot_id":0}
```

---

## 5. Building & Running (Headless Background / Minimized / Windowed)

```bash
# 1. Run unit tests for the Rust bot engine
cargo test --manifest-path games/totk/bot_engine/Cargo.toml

# 2. Build staticlib + Breath of the Wild 1.9.0 runtime (macOS arm64)
make -C games/totk bot-engine runtime-botw190

# 3. Launch a deterministic background smoke + trace run (never steals focus, ~118 FPS, exits automatically)
TOTK_SLOT_OWNER=agent \
TOTK_AUTOLOAD=1 \
TOTK_BOT_SMOKE=1 \
TOTK_PROFILE_DIR=/tmp/botw_bot_profile \
TOTK_BOT_ASHOT_DIR=/tmp/botw_bot_smoke \
TOTK_BOT_TRACE=/tmp/botw_bot_smoke/bot_trace.jsonl \
"/path/to/Breath of the Wild.app/Contents/MacOS/BotW"
```
