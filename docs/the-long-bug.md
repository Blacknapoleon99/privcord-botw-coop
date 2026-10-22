# The long bug: ghost Links

## Symptom

Both PCs connected. Both games said "Player Kev joined". A T-posed Link with the right name tag appeared, and was
erased about a second later. The mod respawned it every 3 seconds, forever. On the laptop, 46 puppets were spawned and
44 erased. The two that survived were frozen, because the mod had stopped tracking them.

## Walking the chain backwards

| Step | What we measured | Tool |
|---|---|---|
| 1 | The game deletes the puppet through its own "fell out of the world" check (20 m drop), then requestDelete → deleteLater → erase | our own PowerPC trace patch: ~35 hooks writing to a ring buffer in a Cemu codecave |
| 2 | The puppet's physics position becomes (0,0,0) about 250 ms after spawning | sampling memory every 4 ms |
| 3 | **Who** writes the zeros: the mod's DLL, from about 233 ms on | debugger attach + a **hardware write breakpoint** on the motion state |
| 4 | The DLL's cached position for the friend never changes from (0,0,0) | walking the DLL's player table |
| 5 | The server never sets the "updated" flag: **0 of 227,150 replies** had it | a logging TCP proxy between client and server ([proxy.py](../tools/proxy.py)) |
| 6 | The server log repeats `[Kev] Retried N times`: it can't read the friend's messages | server log |

## Root cause

The server reads into a 10 KB buffer and, on every read, appends **the whole buffer** to its message. It parses once
at least 6,144 bytes have arrived. On one PC a 7,168-byte message always arrives in one read, so this works. Through a
tunnel the message arrives in pieces. Each piece drags 10 KB of junk in with it, every later message is misaligned
and read as an unknown type, and the friend's updates are silently dropped.

So the friend's position stays zero, the puppet is written to (0,0,0), it falls out of the world and is erased, and
the DLL spawns it again.

## Fix

We couldn't change the server: it's closed and no longer maintained. The PrivCord tunnel now cuts the friend→server
stream into whole messages before the server sees it. A message whose first byte is 1 (a ping) is 6,144 bytes; every
other message is 7,168. See [`crate/src/framing.rs`](../crate/src/framing.rs) and its tests.

## Wrong turns along the way

Each of these was tested live and reverted:

- the create position being ignored;
- stale puppet copies erasing each other;
- the puppet's AI schedule;
- its CharacterController start state;
- bone links;
- single-core vs multi-core recompiler;
- both PCs using the same save.
