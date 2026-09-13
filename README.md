# RustDAoC

RustDAoC is a replacement client for [Dark Age of Camelot](https://darkageofcamelot.com/) written in Rust.

The project started because the original client is showing its age: it still works, but its single-threaded design, old rendering path, and Windows-only assumptions make some kinds of work needlessly difficult. RustDAoC gives us a client we can profile, debug, and extend on current hardware while keeping the game recognizable.

You need your own DAoC installation. This repository does not contain retail assets, and the client will not run without them. You also need a compatible server or emulator.

Expect rough edges. RustDAoC can render the world, talk to a server, process a useful portion of game state, and handle the early login-to-world flow. It is still under active development and is not ready to replace the retail client for everyday play.

## What works

The following areas have working code today:

- World and terrain loading from an existing DAoC install
- Login, realm selection, and character selection
- Character creation protocol round trips
- Player movement, targeting, combat mode, and skill input
- Entity spawning, updates, equipment, and removal
- Inventory, money, merchants, spells, death, and revival packet handling
- GPU-skinned animation
- Basic audio routing
- Early dungeon asset loading and rendering
- Lua addons with typed events and commands

Coverage varies. A decoded packet may only have tests behind it, while another feature may already be wired through the renderer and exercised against a live server. Full class coverage, dungeons, social systems, Keep and War Map behavior, and several UI screens still need work.

Linux is the development platform right now. Windows and macOS builds have not been verified.

## Building and running

Install a Rust toolchain, then point `CAER_CLIENT` at a clean DAoC installation:

```sh
export CAER_CLIENT="/path/to/Dark Age of Camelot"
cargo build --release
./target/release/rustdaoc
```

The client expects the original asset files under `CAER_CLIENT`. Modified or incomplete installs can produce confusing results, so use a separate clean copy if you also run launchers that patch the game directory.

Run `rustdaoc --help` to see the available connection, rendering, and diagnostics options.

## Project direction

Compatibility comes first. Movement, timing, camera behavior, controls, UI, audio, and protocol handling should match what DAoC players and servers expect. Old hardware workarounds can disappear, but changes to the feel of the game need a concrete reason and evidence behind them.

Development is moving through complete player journeys instead of isolated feature counts. The current route starts with a fresh launch and covers login, character creation, character selection, entering the world, movement, focus recovery, and shutdown. Recording that path gives us something useful to replay whenever the renderer, protocol code, or platform layer changes.

The long-term design puts a deterministic state and effect boundary between network/world state and the renderer, UI, audio, addons, and host platform. That work is landing in small pieces so the client stays testable along the way. Server and wider ecosystem development live in the separate CamelotCore project.

## Addons

`caer-script` embeds Lua 5.1 through [`mlua`](https://github.com/mlua-rs/mlua), with LuaJIT compatibility in mind. Addons run in separate sandboxes and have instruction budgets, memory caps, typed events, typed commands, and hot reload.

An example lives at:

```text
crates/caer-script/addons/HelloCAER
```

The CLI has commands for the common addon checks:

```sh
caer addon discover
caer addon check
caer addon test
```

The API is usable, though it will keep changing while the main client systems settle down.

## Protocol and clean-room work

Protocol behavior is derived from packet captures made with a legally obtained client, externally observable behavior, and available server or emulator code such as [Dawn of Light](https://github.com/Dawn-of-Light/DOLSharp).

This project does not use code recovered by decompiling the retail client. Retail assets are also kept out of the repository. Tests and local tools may refer to asset paths or hashes, but contributors must supply the files themselves.

When protocol behavior is uncertain, the code and documentation should say so. A local encoder passing data to a local decoder proves that the two agree with each other. It does not prove that a live DAoC server sends the same bytes.

## Contributing

Bug reports, compatibility notes, tests, tools, and patches are welcome. Please keep retail DAoC assets and code derived from client decompilation out of issues and pull requests.

If you are fixing protocol behavior, include the capture, public implementation, or external observation that led to the change whenever licensing allows it. For rendering bugs, a screenshot and the asset path are usually the quickest way to make the problem reproducible.

## License

RustDAoC is licensed under GPL-3.0-or-later. See [LICENSE](LICENSE).
