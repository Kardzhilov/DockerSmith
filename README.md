# DockerSmith

A full-screen terminal UI suite for Docker — inspect images and containers, check
for updates without pulling, and reclaim disk space, all from your keyboard.

Built in Rust with [ratatui](https://ratatui.rs) and the
[bollard](https://docs.rs/bollard) Docker Engine API client. Talks directly to the
Docker socket (no shelling out to the `docker` CLI).

## Features

**Core**

- **Images & containers** — browse all images and running/stopped containers with
  size, state, and live CPU/memory for the selected container. The images view also
  shows each image's **version**, **build date**, and **source** (a short origin like
  `docker`, `ghcr`, `lscr`).
- **Update checking** — compares your local image digest against the registry
  manifest (no layers pulled) to flag available updates. Press `⏎` on any row for
  a details view showing the **current vs latest version and build date**, plus a
  changelog link. Results are **remembered across restarts**, so a known-outdated
  image still shows as such the next time you open DockerSmith. Works with Docker Hub,
  GHCR, `lscr.io`, and private/self-hosted registries via the daemon's credentials.
- **Disk usage & prune** — a Space view mirroring `docker system df` with a
  reclaimable breakdown per category, plus one-key pruning (dangling/all images,
  stopped containers, unused volumes, build cache, or everything).

**Extras**

- **Container logs** viewer (`L`).
- **Lifecycle controls** — start/stop (`s`), restart (`R`), remove (`x`).
- **Real-time stats** — CPU% and memory for the selected running container.
- **Changelog viewer** (`w`) — pulls the latest GitHub Releases for GHCR images and
  any image with an `org.opencontainers.image.source` label, rendered as formatted
  **Markdown** (headings, lists, code, links).
- **Defer / ignore** (`d`) — silence an update you don't want for 30 days.
- **Apply updates** (`a`) — on the Containers tab, press `a` (or use the footer /
  command palette) to pull the new image and **recreate the container** in place,
  preserving its volumes, ports, environment, restart policy, and networks. A
  confirmation is always required, and the previous container is restored
  automatically if the new one fails to start.
- **Command palette** (`:` or `Ctrl-P`) — fuzzy-search every action.
- **Scheduled checks + notifications** — periodic background update checks that push
  to an ntfy topic or webhook.
- **Multi-host** — manage remote daemons over `unix://`, `tcp://`, or `ssh://`.
- **Self-update** — `dockersmith self-update` swaps in the latest release binary.
- **Themes** (`T`) — midnight, solar, gruvbox, mono.
- **Mouse-driven** — everything is clickable: tabs, list rows (click to select, click
  again for details), footer shortcuts, the prune menu, command palette, and
  confirmation buttons. The scroll wheel moves the selection and scrolls overlays.

## Install

Requires Rust 1.75+ and access to the Docker socket.

```sh
cargo build --release
./target/release/dockersmith
```

## Usage

```sh
dockersmith                 # launch the full-screen TUI
dockersmith check           # headless: list containers with available updates
dockersmith check --host nas
dockersmith space           # headless: reclaimable disk usage (docker system df)
dockersmith apply <name>    # headless: update a container (pull + recreate)
dockersmith doctor          # verify daemon connectivity
dockersmith self-update     # update the binary to the latest release
```

### Keys

| Key | Action |
| --- | --- |
| `⇥` / `1`–`3` | switch tab (Images · Containers · Space) |
| `↑↓` / `j k` | move selection |
| `r` | refresh |
| `u` / `U` | check update (selected / all) |
| `⏎` | update details (current→latest version/date + changelog) |
| `a` | apply update (pull + recreate container) |
| `s` · `R` · `x` | start/stop · restart · remove container |
| `L` · `w` | logs · changelog |
| `d` | defer update 30 days |
| `p` | prune menu |
| `:` / `Ctrl-P` | command palette |
| `T` · `?` · `q` | theme · help · quit |

Everything is also **mouse-clickable** — tabs, rows, the footer shortcuts, and every
pop-up menu — and the scroll wheel navigates lists and overlays.

## Configuration

Config lives at `~/.config/dockersmith/config.toml` (created on first run):

```toml
theme = "midnight"          # midnight | solar | gruvbox | mono

[notify]
url = "https://ntfy.sh/my-private-topic"   # optional

[schedule]
enabled = false
interval_minutes = 360

# Remote/extra hosts (used by --host and the TUI)
[[hosts]]
name = "nas"
endpoint = "ssh://user@nas"
```

Runtime state (deferred updates, changelog source overrides, and remembered update
check results) is stored separately in `~/.config/dockersmith/state.json`. Cached
results are automatically invalidated when the underlying local image changes.

## How update checking works

For each image, DockerSmith reads the local `RepoDigest` and asks the registry for
the current manifest descriptor digest (`inspect_registry_image`, the Engine API's
distribution inspect). No image layers are transferred. If the digests differ, an
update is available. Registry authentication is handled by the Docker daemon, so any
registry your daemon can pull from — including private ones — works automatically.

For the **details view** (`⏎`), DockerSmith additionally reads the local and remote
image **config blobs** (a few KB each, still no layers) to extract the version label
(`org.opencontainers.image.version` and common fallbacks) and build date, so it can
show `current → latest`. When no version label is published, it falls back to the
image creation dates.

## License

MIT
