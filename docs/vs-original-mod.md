# The original mod vs the PrivCord way

*BOTW Multiplayer* 1.0.4 is by the team around PointCrow, with MadMilkman as lead developer. It was taken down after
Nintendo takedowns, so 1.0.4 is final and nobody fixes it any more. **We did not change a single byte of it.**
Everything PrivCord does sits around the mod: how it is installed, launched, connected, configured and extended.

| Area | Original mod, tutorial setup | PrivCord |
|---|---|---|
| Network | Hamachi VPN; port 5050 is reachable by anyone on that VPN network | P2P tunnel; the server listens only on 127.0.0.1, and only room members with the session ticket can reach it |
| Real networks | The server mis-parses messages that arrive in pieces, so a joiner's updates get dropped | The tunnel re-glues every message |
| Mod files | Merge BNPs with BCML (needs Python) | The merged result ships as a Cemu graphic-pack overlay |
| Cemu | Set up by hand; easy to pick a version or pack format that silently fails | Pinned Cemu 1.26.2 with every required pack in the right format; backups of every change |
| Hosting | Run the server, answer its console prompt, keep the window open | Starts unattended, waits until ready, stops with the session |
| Joining | Type the host's IP in the Lobby Browser | Press Join on the session card |
| Movement | Freezes and jumps on Wi-Fi (14% frozen frames) | Jitter buffer (0%) |
| Co-op tools | Console commands only | Party list, Go to / Bring buttons, one-click modes |
| New gameplay | None | Enemy health ×1.5–3, downed & revive, shared enemies (beta), auto graphics |

## What we fixed around the mod

**Bugs in the mod (worked around, since it can't be edited)**
- The server corrupts split messages → tunnel framing.
- The server crashes on stdin EOF → we keep an open pipe.
- The server crashes on connect-then-close → we never probe it.
- `IP=localhost` binds the LAN card → we rewrite it to `127.0.0.1`.
- The DLL only scans the 8th memory region → the Extended Memory pack is always on.
- The DLL's flag scan is sometimes flaky. **Not fixed.**

**Version traps**
- The mod needs Cemu older than 2.0-47 → a separate Cemu 1.26.2.
- The utilities pack's `version = 7` is silently skipped by 1.26.2 → we use our own v6 copy.
- Current community graphics packs are v7 → we pin release Github833.
- The official Cemu 2.6 zip has a garbled file name → we re-staged it without that file.

**Our own bugs**
- Launching the client with NUL std handles → switched to `ShellExecuteW`.
- A readiness probe that connected and killed the server → switched to a bind check.
- A `Cemu.exe` case mismatch.
- A stale downed flag that killed both players.
- A revive hold that reset on one-tick blips.
- An old join that still held port 5050.
- A party-feed reply with no size limit.

**Environment**
- Wi-Fi bunching → jitter buffer.
- Slow `WriteProcessMemory` → `NtWriteVirtualMemory`.
- A disabled Cemu controller.
- A laptop screen that reports only 60 Hz.

## Why this is better than the plain mod

- **It works over a real connection.** With the plain server, a joiner's updates get dropped whenever messages arrive
  in pieces. That is what made the ghost Links.
- **Two clicks instead of a 13-step guide**, with hash-checked packages and backed-up settings.
- **Safer.** The server is loopback-only, behind a tunnel that only room members can use.
- **Smoother**, and **more game**: difficulty, downed & revive, shared enemies, party HUD, teleports, graphics
  presets.
- **Repeatable.** Install is idempotent and covered by end-to-end tests.

## Where we're still limited by the original

- We still depend on its closed DLL and server.
- We're locked to Cemu 1.26.2, game version v208, and Windows.
- Its wire format is fixed, so new data has to travel on our own side channels.
- "Hunter vs Speedrunner" has no rules in 1.0.4.
- We have tested 2 players; the server supports 32.
