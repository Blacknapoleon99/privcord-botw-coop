# PrivCord × Breath of the Wild co-op

Two PCs, one Hyrule. This repository shows how **PrivCord** (a private, Discord-like app by Blackz, built with Tauri,
Rust and React) turned the community *BOTW Multiplayer* mod into a two-click co-op experience. It also covers the bug
that took three sessions to find and the co-op features we built on top.

**➜ [Open the interactive whiteboard](https://blacknapoleon99.github.io/privcord-botw-coop/)**. It has the
architecture drawboard, the real-time sync path, the long bug with an animated before/after, working vs non-working
code, the original mod vs PrivCord, lessons and the roadmap.

> This is a showcase. It contains only code and writing of our own. It contains **no** Nintendo game files, **no** part
> of the BOTW Multiplayer mod, and nothing that lets anyone play without their own legally dumped copy of the game.
> See [NOTICE.md](NOTICE.md).

## What we built

| | |
|---|---|
| **One-click install** | Game bundle, mod, Cemu 1.26.2, graphic packs, settings and keyboard profile. Every package is SHA-256 pinned, every change is backed up, and a re-run is a sub-second no-op. |
| **Tunnel instead of Hamachi** | PrivCord's peer-to-peer tunnel carries the game. The mod's server only listens on `127.0.0.1`, and only room members can reach it. |
| **The framing fix** | The mod's server corrupts any message that arrives in pieces. The tunnel re-glues each message to exactly 7,168 / 6,144 bytes. This fixed the "ghost Link" bug. |
| **Smooth remote player** | A jitter buffer next to the game: frozen frames on the other player went from 14% to 0%. |
| **Co-op difficulty** | Enemy health ×1.5–3, generated on each PC from that PC's own game dump. Only the 4-byte `Life` value changes, so file sizes stay valid. |
| **Downed & revive** | Our own PowerPC patch holds Link at ¼ heart. A friend holds **G** for 12 s to revive. It's game over only when everyone is down. |
| **Shared enemies (beta)** | An enemy near two players is run by the nearest one. Everyone else sees a puppet copy (its AI paused by our patch) that follows the owner's position, animation and death. |
| **Party & modes** | Party list with hearts and distance, Go to / Bring teleports, Co-op / Death Swap / Hunter vs Speedrunner, all through the stock server's config file and console. |
| **Auto graphics** | GPU and screen detection picks Max, High or Balanced and a 60/120/144 fps limit. |

## What's in here

```
crate/                 Rust crate (compiles on its own; 51 unit tests)
  src/framing.rs         tunnel message framing: the fix for the long bug
  src/botw_smooth.rs     jitter buffer for the remote player
  src/botw_smooth_sidecar.rs  live runner: reads/writes Cemu memory (NtWriteVirtualMemory)
  src/botw_downed.rs     downed/revive rules, messages, relay
  src/botw_downed_sidecar.rs  live runner for the downed patch
  src/botw_enemies.rs    shared enemies: wire format, ownership, adaptive playback
  src/botw_enemies_sidecar.rs  live runner for shared enemies
  src/botw_difficulty.rs co-op difficulty pack generator (hand-written Yaz0/SARC/AAMP/BYML)
  src/botw_graphics.rs   GPU/screen detection and Cemu presets
  src/botwm.rs           BOTWM server control: settings, console parser, party feed
  examples/              terminal probes used for two-PC testing without the app
patches/               our own Cemu graphic-pack patches (PowerPC assembly, v208)
  PrivCordDowned/        downed instead of dead, input freeze while downed
  PrivCordEnemies/       per-enemy AI skip table for shared enemies
tools/                 analysis scripts (logging proxy, RPX disassembler, motion analysis)
docs/                  the whiteboard (GitHub Pages) and the write-ups
```

```bash
cd crate
cargo test --lib
```

Some tests need real files and are `#[ignore]`d: a game dump, a running Cemu, or a community graphic pack.

## Write-ups

- [How it works](docs/how-it-works.md): architecture, the real-time sync path, ports.
- [The long bug](docs/the-long-bug.md): ghost Links, and how a hardware breakpoint led to the tunnel.
- [Original mod vs PrivCord](docs/vs-original-mod.md): what we changed around the mod, and why it's better.
- [Lessons](docs/lessons.md): what got us stuck, how we got unstuck, and what we'd do differently.
- [Roadmap](docs/roadmap.md): what's next.

## Credits

*BOTW Multiplayer* is by its original team, around PointCrow with MadMilkman as lead developer. PrivCord integration and
co-op features are made by **Blackz**. Cemu is by the Cemu team. Graphic packs are by the Cemu graphic-pack community.
