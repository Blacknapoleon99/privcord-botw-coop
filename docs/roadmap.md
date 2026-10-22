# Roadmap

The rule for everything new: it travels on PrivCord's own side channel (a sidecar plus our own patches), never on the
mod's fixed wire format.

## Now (days)

- One combined build of PrivCord with co-op, gameplay and shared enemies, then a full two-player session inside the app.
- Verify the input freeze while downed, max graphics in game, and smoothing seen from the friend's side.
- Smoothing inside the game: a per-frame patch reads our smoothed position. This removes the last spike every ~2 s.
- Shared enemies P3: hits on a puppet copy go to the owner's real enemy.
- An enemy damage multiplier next to the health one.
- Auto-install the .NET 6 Desktop Runtime, the last manual step for friends.

## Next (1–5 weeks)

| Idea | Effort | Why |
|---|---|---|
| Proximity voice | 3–5 days | Positions are already in the party feed; no game risk |
| Host world controls (time, weather, "everyone to me", go to shrine) | 2–3 days | Stock console commands |
| Save snapshots before every session | 2 days | A safety net for anything riskier |
| Session recap (shrines, towers, koroks) | 2–3 days | Fun and cheap |
| Item trading | 3–5 weeks | A mailbox patch calls the game's own add/remove item functions, with a ledger |
| Own save per player, drop in/out | 1–2 weeks | Needs a clear definition of "co-op progress" first |

## Later (months)

- **Replace the closed DLL**, in 10–15 weeks: an out-of-process sidecar, our own patch packs and a small Rust server.
  There would be no injection and no framing workaround, and newer Cemu versions would become possible. Start only if
  one of these happens:
  - a feature needs a new field on the mod's wire;
  - the DLL stops working or can't be obtained;
  - we want an installer with no third-party downloads;
  - a remaining puppet bug can only be fixed inside the DLL.
- Spectate a friend, emotes, shared horses, co-op puzzles, a shared stash and wallet.
