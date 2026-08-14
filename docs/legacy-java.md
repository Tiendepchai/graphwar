# Legacy Java Logic Summary

## Status and scope

Graphwar began as a GPL-3.0-or-later Java desktop game with separate match, lobby, and room services. That implementation, its build files, and its unused resources were removed after the Rust/WASM browser release became the sole supported runtime. This document preserves behavior and migration context, not source code. The old Java client and servers are unsupported and cannot interoperate with the current release.

## Desktop client

The Swing client used a fixed 800×600 window and a 770×450 Cartesian battlefield. Screens covered the main menu, lobby, room setup, and active match. The client collected expressions, displayed players and timers, previewed or animated trajectories, and rendered terrain, soldiers, current-turn markers, explosions, deaths, and function paths.

Rendering used cached Java2D images and layered sprite resources. Team Two mirrored the horizontal firing direction. The soldier helmet was composed by applying a player-coloured tint through a 20×20 alpha mask, then drawing the helmet line art above the body. The browser retains this visual rule with deterministic team colours and nearest-neighbour sprites.

## Match server (`GraphServer`)

A match accepted TCP clients, assigned the first client as leader, and grouped each client's local players into two teams. Before a match, the leader selected the mode while players changed teams, soldier counts, and readiness. Starting required a valid two-team roster and used a short countdown.

During play, the server distributed terrain and soldier positions, selected turns, enforced a 60-second turn window, relayed fired functions, advanced past dead soldiers, and ended the match when one team remained. Disconnects removed the departing client's players and transferred leadership when needed.

The original authority boundary was weaker than the current server: parts of trajectory, hit, death, and turn handling depended on client messages. The Rust server intentionally makes those outcomes authoritative.

## Lobby and room services

`GlobalServer` tracked connected lobby players and advertised public rooms. It handled join, quit, player lists, room lists, room creation, room status, and room closure.

`RoomServer` managed standalone public match rooms. It periodically inspected them, restarted inactive rooms after use, kept spare empty rooms available, and removed excess empty rooms. The browser release replaces this process topology with one authenticated HTTP/WebSocket server and an in-process room registry backed by PostgreSQL snapshots.

## Gameplay rules

Graphwar turns mathematical expressions into trajectories across destructible terrain:

- **Function:** translate `y = f(x)` so the curve passes through the firing soldier.
- **First-order ODE:** solve `y' = f(x, y)` from the soldier position.
- **Second-order ODE:** solve `y'' = f(x, y, y')` from the soldier position and firing angle.
- **Teams:** Team Two uses the mirrored logical direction.
- **Terrain and hits:** trajectories stop at battlefield bounds, terrain, or a soldier; explosions remove terrain and can kill nearby soldiers.
- **Turns:** teams alternate, dead soldiers are skipped, and elimination determines the winner.

The Java parser supported arithmetic, powers, common unary functions, and the variables required by each mode. Function trajectories used fixed-step evaluation; ODE modes used Runge–Kutta integration with adaptive limits. Terrain was raster-oriented and randomly generated. The Rust game core keeps the player-facing modes while adding strict parsing, finite-value and complexity limits, seeded generation, geometric segment collision, deterministic simulation, and explicit server validation.

The Java computer player evolved expression populations with unchanged, mutated, and crossover candidates. Candidate fitness rewarded enemy hits, penalized ally hits, and otherwise favoured proximity. The Rust server preserves the genetic-search shape but adds deterministic randomness, bounded expressions, mode validation, and authoritative shot evaluation.

## Protocol and persistence

The Java services used newline-delimited TCP messages. Each message began with a numeric operation code; fields were joined with `&`. Message families covered connection health, lobby and room lists, player setup, readiness, countdowns, modes, turns, angles, fired expressions, timeouts, and match completion.

There was no durable room or active-match persistence. Connection and game state lived in process memory. The current protocol is versioned tagged JSON over authenticated WebSockets, with UUID identities, structured errors, snapshots, sequence numbers, reconnect synchronization, chat, protected rooms, and session expiry. No compatibility adapter exists for the historical wire format.

## Rust replacement mapping

- `crates/client-wasm` replaces the Swing client, state reducer, input handling, and Java2D renderer.
- `crates/game-core` owns expressions, trajectories, terrain, collision, generation, and shared game models.
- `crates/protocol` defines the versioned browser/server JSON contract.
- `crates/server` replaces the match, lobby, room, authentication, bot, and persistence services.
- `assets/web` and the three retained `assets/rsc/soldiers` sprites form the browser distribution.
- `deploy` contains the supported production image and Compose stack.

Exact Java quirks were not compatibility requirements. Parser accidents, client-authoritative outcomes, unseeded randomness, raster collision differences, raw TCP, fixed Swing layout, and the old room-process scaler were deliberately not retained.

## Removal boundary

Removed material included all Java source, Java-only build entrypoints, generated JAR/class outputs, and legacy UI, audio, animation, explosion, mask, and duplicate resource files. The repository retains `COPYING`, this behavior summary, and only the three soldier sprites used by the browser runtime. Git history remains the source for exact historical implementation details.
