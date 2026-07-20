# Graphwar browser parity checklist

Legacy references remain under `src/` and `rsc/`. The Rust/browser release preserves user-visible rules; confirmed correctness and authority bugs are intentionally not preserved.

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

## Deliberate incompatibilities

- No Java client/server interoperability.
- No raw TCP or numeric `&` wire protocol.
- No image-mask buttons or fixed 800×600 Swing layout.
- No client authority over hits, deaths, turn advancement, or game result.
- Strict conventional parser semantics replace ignored characters and accidental associativity.
