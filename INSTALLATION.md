# Installation

One-time setup for building and running this project (hexmerge — raylib-rs +
SpacetimeDB gamejam entry, browser-only). Rust/Cargo is assumed to already
be installed.

This project only ships a web client: `raylib-rs` is built through
`raylib-sys` for the `wasm32-unknown-emscripten` target via emsdk (see
below), so no host GL/X11/Wayland dev packages are needed.

## SpacetimeDB CLI

```sh
curl -sSf https://install.spacetimedb.com | sh
```

## Web build (emsdk)

Only needed to build/run the browser client (`./build-web.sh`).

```sh
git clone https://github.com/emscripten-core/emsdk.git
cd emsdk
./emsdk install latest
./emsdk activate latest      # writes .emscripten file
source ./emsdk_env.sh        # activates PATH for the current terminal

rustup target add wasm32-unknown-emscripten
```

`build-web.sh` sources `emsdk_env.sh` for you on every run (assumes
`../emsdk` relative to the repo root; override with `EMSDK_DIR=...`), so you
only need to do the clone/install/activate once.

## Next steps

See [WORK.md](WORK.md) to run the project.
