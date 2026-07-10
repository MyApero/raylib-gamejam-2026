# Installation

One-time setup for building and running this project (hexmerge — raylib-rs +
SpacetimeDB gamejam entry). Rust/Cargo is assumed to already be installed.

## System dependencies (raylib)

`raylib-rs` builds raylib from source via `raylib-sys`, so the usual raylib
system packages are required:

```sh
# Ubuntu
sudo apt install libasound2-dev libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev libglu1-mesa-dev libxcursor-dev libxinerama-dev libwayland-dev libxkbcommon-dev

# Fedora
sudo dnf5 install alsa-lib-devel mesa-libGL-devel libX11-devel libXrandr-devel libXi-devel libXcursor-devel libXinerama-devel libatomic

# Asahi Linux
sudo dnf5 install libX11-devel libXrandr-devel libXi-devel libXcursor-devel mesa-libGL-devel pulseaudio-libs-devel libdrm-devel libXinerama-devel
```

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
