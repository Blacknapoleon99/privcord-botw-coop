# Notice

This project is not affiliated with, endorsed by, or sponsored by Nintendo, the BOTW Multiplayer team, or the Cemu
project. *The Legend of Zelda: Breath of the Wild* is a trademark of Nintendo.

What this repository **does not** contain:

- No game files, game data, game assets, screenshots or anything extracted from a game dump.
- No part of the *BOTW Multiplayer* mod: not its client, server, injected DLL, BNP files, backgrounds or source code
  (decompiled or otherwise). Where the write-ups describe how the mod behaves, that comes from observing it
  and is written in our own words.
- No Cemu binaries and no community graphic packs.
- No keys, no download links for any of the above.

What it **does** contain is code and text of our own:

- Rust code that runs alongside the game, reading and writing the emulator's memory from outside.
- Cemu graphic-pack patches in PowerPC assembly. They hold our own instructions only, plus addresses in the game's
  executable (v208).
- Analysis scripts, and the write-ups.

The co-op difficulty generator builds its pack on each player's PC from that player's own dump. Nothing from the game
is distributed.

Copyright © 2026 Blackz. All rights reserved unless a license file says otherwise.
