# Graphwar browser parity checklist

Rust/WASM is the sole supported implementation. The browser release preserves user-visible rules; confirmed correctness and authority bugs were intentionally not preserved. See the [legacy Java logic summary](../docs/legacy-java.md) for historical context.

## Modes and expressions

- [ ] Normal function: translate `f(x)` through the current soldier.
- [ ] First-order ODE: solve `y' = f(x,y)` from soldier position.
- [ ] Second-order ODE: solve `y'' = f(x,y,y')` from position and angle.
- [ ] Team two mirrors the logical plane and firing direction.
- [ ] Variables `x`, `y`, `y'`; constants `e`, `pi`.
- [ ] Operators `+`, `-`, `*`, `/`, `^`; unary minus.
- [ ] Functions `sqrt`, `log`, `ln`, `abs`, `sin`/`sen`, `cos`, `tan`/`tg`, `exp`.
- [ ] Decimal commas and implicit multiplication.
- [ ] Unknown text, unbalanced brackets, excessive size/depth, and non-finite results are rejected.

## Lobby and rooms

- [ ] Email/password register, login, logout, session restore.
- [ ] Public room directory, global player list, global chat.
- [ ] Create/join public room.
- [ ] Create/join private room by invite code.
- [ ] Multiple account-owned local player slots.
- [ ] Add/remove AI slots and select AI level.
- [ ] Team assignment, 0–4 soldiers, mode selection, ready/unready.
- [ ] Start countdown cancels when setup becomes invalid.
- [ ] Room chat; return to lobby/room after match.

## Match

- [ ] Seeded terrain and soldier placement.
- [ ] Alternating team turn order; skip dead soldiers.
- [ ] 60-second authoritative turn deadline.
- [ ] Function preview, fire, angle controls in second-order mode.
- [ ] Segment collision with soldiers and terrain.
- [ ] Function path animation, explosion, terrain removal, death animation.
- [ ] Team elimination determines winner server-side.
- [ ] `-skip` requires all participating accounts; display preferences remain local.
- [ ] Reconnect replaces local state from a server snapshot.

## Desktop browser UI

- [ ] Accessible DOM inputs, buttons, room/player lists, alerts, chat logs.
- [ ] Canvas game plane uses 770×450 logical coordinates and device-pixel scaling.
- [ ] Keyboard function submit and angle controls.
- [ ] Chrome, Firefox, Safari, Edge current desktop releases.

## Public beta release evidence

These are release gates, not claims that the checklist above has passed. Record the tested commit/image ID, tool/browser versions, date, result (`pass`, `fail`, `blocked`, `not run`) and evidence location. Keep credentials, session tokens, dumps and player snapshots out of evidence logs. See the [release and rollback runbook](../readme.md#public-beta-release-gates).

- [ ] Rust 1.89.0 native/WASM checks, fmt/clippy and workspace tests pass.
- [ ] Explicit PostgreSQL round-trip/recovery test passes (ordinary ignored-test output is insufficient).
- [ ] Local-image E2E passes against the release image: auth, rooms, editor/fire, bot/practice/rematch, reconnect and desktop/mobile accessibility.
- [ ] Image assets load and decode, including MathLive, WASM and all soldier sprites; missing assets return 404.
- [ ] Two owned-server restarts retain sessions, room data and the expected turn; Resolving recovery advances only once in the database test.
- [ ] Backup is restored into a separate database; migrations, snapshot load, data comparison and synthetic-session reconnect pass.
- [ ] Separately approved staging HTTPS, Secure cookies, WSS and two-browser gameplay pass.
- [ ] Release owner records previous image, verified backup time, rollback compatibility and accepts the release.

| Gate | Commit / image | Environment / versions | Date | Result | Evidence |
|---|---|---|---|---|---|
| Native / MSRV | — | — | — | not run for release | — |
| PostgreSQL | — | — | — | not run for release | — |
| Image / browser / restart / restore | — | — | — | not run for release | — |
| Staging HTTPS / WSS | — | — | — | not run | — |

## Deliberate incompatibilities

- No compatibility with the removed Java client/server or its raw TCP numeric `&` wire protocol.
- No image-mask buttons or fixed 800×600 Swing layout.
- No client authority over hits, deaths, turn advancement, or game result.
- Strict conventional parser semantics replace ignored characters and accidental associativity.
