# How it works

## The pieces

```
 MAIN PC (host)                                   FRIEND'S PC
 ┌──────────────────────────────┐                 ┌──────────────────────────────┐
 │ PrivCord app                 │   PrivCord      │ PrivCord app                 │
 │  · starts the mod's server   │   P2P tunnel    │  · binds 127.0.0.1:5050      │
 │  · tunnel host  ◄────────────┼─────────────────┼──► tunnel joiner             │
 │  · sidecars                  │ 5050 (framed)   │  · sidecars                  │
 │                              │ 5051 · 5053     │                              │
 │ BOTW dedicated server        │                 │ BOTWM client → 127.0.0.1:5050│
 │  127.0.0.1:5050              │                 │                              │
 │ Cemu 1.26.2 + mod DLL        │                 │ Cemu 1.26.2 + mod DLL        │
 │  Link + friend's puppet      │                 │  Link + host's puppet        │
 └──────────────────────────────┘                 └──────────────────────────────┘
```

| Port | What |
|---|---|
| TCP 5050 | The mod's dedicated server. This is the only stream that needs [framing](../crate/src/framing.rs). |
| TCP 5051 | PrivCord party feed: members, hearts, distance, teleports. |
| TCP 5053 | Downed/revive and shared-enemies relay. |

On the friend's PC, 5051 and 5053 map to any free local port, so a busy port can't break a join.

## Host and join

- **Host:** PrivCord rewrites the server's `ServerConfig.ini`:
  - `IP=127.0.0.1`, because `localhost` bound the LAN card;
  - `DefaultGamemode=True`, so the server starts without asking a question;
  - the co-op settings.

  It starts the server with an **open stdin pipe**, since a closed stdin crashes it. It waits until binding 5050
  fails, which means the server holds the port. It doesn't connect to check, because connect-and-close crashes the
  server. Then it posts a session card in the chat.
- **Join:** the friend presses Join on the card. The tunnel listens on the friend's own `127.0.0.1:5050`, and the
  mod's client is launched with `ShellExecuteW`. Launching it with NUL std handles makes Cemu abort with 0xc0000409.

## One position update, end to end

1. The mod's DLL reads Link's position from emulated memory (a signature scan).
2. The client sends a 7,168-byte update with the position at offset 0x0d (little-endian floats).
3. The tunnel delivers it to the server as **one whole message**. Before the fix it arrived in pieces.
4. The server stores it and marks the player as updated for everyone else.
5. The other player's own update works as a request, and the server answers with a 213-byte snapshot
   (remote position at offset 0x12, `Updated = 1`).
6. The first time, the DLL spawns a puppet actor (`Jugador<N>`) through the game's own actor creator.
7. Every 16 ms the DLL writes position + velocity × t into the puppet's physics motion state.
8. PrivCord's smoothing sidecar replays the received positions about 45 ms behind, at a steady rate, and writes them
   right after the DLL's write ([botw_smooth.rs](../crate/src/botw_smooth.rs)).

## Sidecars

The sidecars are PrivCord threads that attach to Cemu from outside. Nothing is injected. They find our patch blocks
through Cemu's `log.txt`, then read and write guest memory with `ReadProcessMemory` / `NtWriteVirtualMemory`.
`WriteProcessMemory` costs about 12 ms per call on Cemu's 3.3 GB guest region, so they don't use it.

- **Downed** (10 Hz): watches the [PrivCordDowned](../patches/PrivCordDowned) block, runs the revive rules, reads
  the G key and talks to the relay.
- **Smoothing** (~2 kHz reads): one jitter buffer per remote player.
- **Shared enemies** (16 ms tick, 4 ms writes): ownership by proximity, puppet playback, and the
  [PrivCordEnemies](../patches/PrivCordEnemies) AI-skip table with leases. If PrivCord dies, every puppet runs its own
  AI again within about a second.

Every patch defaults to vanilla behaviour and only acts once PrivCord arms it.
