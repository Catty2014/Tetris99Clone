# Tetris99Clone — Tetris 99 Replica (Arena Module for AX1)

English | [简体中文](README.md)

A deterministic Tetris-99-style match engine + headless host + Bevy human client + built-in bot ladder.
One set of physics serves both the **human client** and **program players**: the human client is just another source of input events, going through exactly the same validation and physics path as subprocess programs and built-in bots.

## Notice

- This project is almost entirely created by Generative AI / Large Language Model technology. We make no guarantees about its usability or correctness. The project is provided **"as is"**, with the hope that it "might be useful", and **without any warranty**.
- The Tetris 99 game mechanics were chosen as the base to maximize familiarity for both models and humans. This project does **not intend to, and does not**, fully replicate the game features and mechanics of Tetris 99; without hindering the models' problem-solving, we have made changes to the original rules.
- This project was built using only the Tetris Guideline and interpretive texts about Tetris 99. **No decompilation or reverse engineering of the Tetris 99 game program was performed.**
- The copyright of Tetris 99 is owned by **Nintendo Co., Ltd.**

## Components

```
t99-core      Deterministic kernel: integer physics / SRS / 7-bag / garbage / targeting /
              badges / ranking (zero dependencies)
t99-proto     Wire protocol v8 (single-line JSON) + adapters (stdio/WS) +
              snapshot_from_frame (wire → memory reconstruction)
t99-bots      Built-in bots (one file per tier): null / basic / advanced / expert (in-process,
              same view, same rate limits; basic/advanced source is public, expert is a
              closed-source built-in behind the `expert` feature)
t99-bot-ext   Public-tier external programs: t99-bot-basic / t99-bot-advanced (reference
              implementations / modifiable sparring partners; same decision core as
              internal; shipped in dist)
t99-bot-m     Hidden-tier external programs Master/GrandMaster (excluded from the public
              package; participants only know they exist)
t99-config    ruleset.toml / server.toml / client.toml parsing (CLI > env > file > defaults)
t99-headless  Headless host: run (matches, supports ext:label=path external seats) /
              replay (bit-exact verification) / calibrate (tier matrix)
t99-server    Single-match authoritative server: ID+Key admission, event-stream broadcast,
              spectator seats, lobby mode
t99-client    Bevy 0.16 human client (1 local player + 3 bots, HUD / ghost / garbage table;
              not shipped in dist)
examples/     pybot.py: Python subprocess example (cross-language protocol reference)
utils/        time.py: Arena Utils session wall-clock query
Makefile      make check (tests + smoke) / make dist (static musl distribution → dist/)
```

Official competition lineup: **internal** basic/advanced/expert + **external** master/grandmaster.
The external basic/advanced are reference implementations and sparring partners — equivalent to
internal by construction, without a 1:1 assertion.

## Quick start

```bash
cargo build --release
make check                                # full workspace tests + external smoke match
make dist                                 # static musl distribution → dist/ (exclusion
                                          # assertions + standalone source build check)

# 4-player bot match, 15-minute cap, ~7s wall time
./target/release/t99-headless run --players 4 --bots basic,advanced,expert,advanced \
    --seed 31415 --transcript /tmp/m.jsonl

# Bit-exact replay verification (seed + event stream → same hash)
./target/release/t99-headless replay --transcript /tmp/m.jsonl

# Tier calibration matrix (N games per pair, verifies strict tier ordering)
./target/release/t99-headless calibrate --tiers null,basic,advanced,expert --games 12

# External program seats (ext:label=path, same channel as model submissions)
./target/release/t99-headless run --players 4 --seed 7 \
    --bots "ext:basic=./target/release/t99-bot-basic,advanced,expert,basic"

# C/S (incl. B/S): server = one match; clients mirror the event stream locally
# (deterministic, hash-verified). Countdown scales with player count: 4p=5min → 99p=15min.
# --stall-accel 1/0: burn the countdown faster after 60s of zero kills (exhibition only).
./target/release/t99-server --port 8138 --key t99 --players 4 --lobby \
    --bots "ext:basic=./target/release/t99-bot-basic,advanced,expert,basic"
cargo run --release -p t99-client -- --server ws://127.0.0.1:8138 --id p1 --key t99          # player
cargo run --release -p t99-client -- --server ws://127.0.0.1:8138 --id ob --key t99 --observer --spectate 0  # spectator

# Browser client (wasm, WebGL2)
./scripts-build-web.sh && cd crates/t99-client/web && python3 -m http.server 8137

# Subprocess participation (example: Python greedy vs built-in basic)
./target/release/t99-headless run --players 2 --bots basic \
    --candidate "python3 examples/pybot.py" --seed 99
```

Client keys: `A/D` move (held autorepeat, engine-side DAS/ARR) · `S` soft drop ·
`Space` hard drop · `X/J` clockwise · `Z/K` counter-clockwise · `C/Shift` hold ·
`Tab` cycle target modes · **arrow keys for manual target selection**.

## Protocol essentials (v8)

- Single-line JSON: `hello → ready → (frame → input)* → result`; `ready` within 2s, frame
  response budget 16.666ms (enforced at 16ms), 600 consecutive unanswered frames → loss,
  non-zero process exit → loss.
- **Press/release keys**: `left_down/left_up/...` with engine-side DAS/ARR (identical physics
  for everyone); event offsets are integer microseconds `t_us ∈ [0,16666]`, ≤16 events per
  frame, strictly increasing; illegal frames are dropped entirely.
- Five target modes (badges/kos/attackers/random/manual); snapshots carry `phase`, `das`,
  `targeted_by`, `badges_all`, `pending_all` and other offense/defense decision fields.
- **Garbage**: three-stage independent entry (gray queued → yellow timing → red imminent),
  combo blocks entry; **blocking rule** (T99 semantics): with pending garbage, attacks are
  entirely used for blocking; only after the queue empties are the remainder sent to
  opponents; queue cap 12, entering rows push the stack (and the active piece) up, pushing
  the piece fully out of the field is a loss.
- Badges = KO transfer (the killer receives the victim's entire badges + 1), tiers at
  2/6/14/30 badge units.
- **Rhythm phases**: switch at 50%/30%/10% survival ratio, gravity ramps within a phase and
  jumps between phases.
- Rule parameters live in `ruleset.toml` (a ruleset ID ships with the handshake; mismatch is
  rejected).

## Determinism

- Internal time unit is ⅓ns (1 frame = 5×10⁷ ticks, µs/ms all divide evenly), positions in
  integer milli-cells; **zero floating point in the kernel**.
- Per-player splitmix64 streams (derived from seed, pid, purpose); a match is a pure
  function of `(seed, event stream)`.
- `replay` re-runs a transcript and compares the state hash; it must match bit-for-bit
  (verified including subprocess matches).
- Lock timing is accurate to 1ms sub-steps (positions guaranteed by the accumulator) — see
  ADR-008.

## Verified

- `cargo test`: whole workspace green (incl. snapshot↔frame roundtrip, internal/external
  same-match smoke)
- Calibration matrix (12 games per pair): null < basic < advanced < expert **strictly
  ordered, all wins/all losses**
- M/GM redefined via a 5-subagent strategy draft (murder-ball tournament + 1v1 firepower
  differential; see `lab/` and ADR-026)
- 15min × 4p full match in ~7s headless (release); 99p at 59 TPS (phase-split collection)

## License

MIT-0 (see [LICENSE](LICENSE)). Rule decisions and open questions: `docs/rules/adr.md`.

----
README version: 20261101 (the zh/en copies are synced by this number; if they disagree, the
one with the newer version number wins)
