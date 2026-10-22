# Lessons

## Where we got stuck, and how we got unstuck

| Stuck on | Why | Unstuck by |
|---|---|---|
| Ghost Links erased every second | The server dropped the friend's split messages | A hardware write breakpoint → logging proxy → tunnel framing |
| Server dies at start | stdin EOF makes its console loop crash | Keep stdin an open pipe |
| Server unreachable on 127.0.0.1 | `IP=localhost` binds the LAN address | Rewrite the ini |
| Our health check killed the server | Connect-then-close crashes its error handler | Check that binding the port fails instead |
| Cemu 0xc0000409 on Connect | It inherited NUL std handles from our launch | `ShellExecuteW` |
| The DLL can't find the game in memory | It only scans the 8th memory region | Extended Memory pack |
| Multiplayer patches silently ignored | Pack `version = 7`, which Cemu 1.26.2 skips | Our own v6 copy |
| Cemu 2.6 zip won't unpack | One garbled file name | Re-staged without it |
| Choppy remote player | Wi-Fi bunches packets (20 / 40 / 4 ms gaps) | Jitter buffer |
| Memory writes too slow | `WriteProcessMemory` ≈ 12 ms per call | `NtWriteVirtualMemory` |
| Revive killed both players | A stale downed flag after a reload | Only count the flag at exactly ¼ heart |
| Reading game files needed a C++ compiler | The `roead` crate builds C++ | Small hand-written Yaz0 / SARC / AAMP / BYML readers |
| Our patch crashed the game | PowerPC `lwz rX, 0(r0)` means base 0, not register r0 | Proper base registers |

## What we removed or reverted

- Key-file staging, which wasn't needed.
- ZeroTier from the BotW flow; BotW is tunnel-only now.
- Hamachi.
- Cemu 2.6 for multiplayer.
- Our diagnostic trace patch and the experimental puppet-copy patch.
- AI schedule, CharacterController and bone-link edits.
- Graphics packs newer than v6.
- Some bosses from ×2 health: Dark Beast Ganon, weak points, 1-HP actors, dragons and Divine Beast parts.

## What we should have done differently

1. **Capture the network traffic first.** A logging proxy on day one would have shown "0 replies with Updated = 1"
   straight away. We spent about two sessions tracing inside the game instead.
2. **Test the real path early.** Every install and host test ran on one PC, over loopback, where messages never split.
3. **Follow the mod's guide to the letter first, then improve.** Cemu 1.26.2, Extended Memory and FPS++ were all in
   it.
4. **Read the server's receive loop as soon as a second PC is involved.**
5. **Decide the network approach before building one.** We built ZeroTier, then dropped it.
6. **Never poke fragile, closed software.** Watch it from outside instead: whether the port is bound, its log lines.
7. **One save per player from the start.**

What went right: every real fix came from a measurement. The write breakpoint, the 0-of-227,150 count, the arrival
gaps and the 12 ms write cost each pointed straight at a fix.
