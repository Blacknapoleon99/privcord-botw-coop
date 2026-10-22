# Tools

Small analysis scripts used during development. None of them contain game data; they read files or processes on your
own machine.

| Script | What it does |
|---|---|
| `proxy.py` | Logging TCP proxy (127.0.0.1:5051 → 5050) that dumps both directions. This is the tool that showed the server never set `Updated`. |
| `parse-proxy.py` | Summarises a proxy log: message sizes, and float fields that look like positions. |
| `rpxdis.py` | Decompresses a Wii U RPX in memory and disassembles address ranges (needs `capstone`). |
| `rpx-imports.py` | Lists RPX import stubs by name (e.g. finding the single `VPADRead` call). |
| `analyze-motion.py` | Compares the remote puppet's motion on one PC with the player's true motion on the other (stalls, step spread). |
| `verify_difficulty_pack.py` | Independent check of a generated co-op difficulty pack with `oead`: sizes unchanged, only `Life` differs. |
