# busywatch

Warns when the system is too busy, at near-zero cost.

Instead of polling, busywatch registers a **PSI trigger** with the kernel
(`/proc/pressure/cpu`) and sleeps in `poll()` until the kernel itself reports
that tasks were stalled on CPU beyond a threshold. While the system is healthy
the process consumes no CPU at all — measured: 0 CPU seconds across a load
storm that fired four warnings.

A trigger event alone is not a warning — compiles and page loads fire triggers
constantly. On each wakeup the sustained average (`avg60`) is checked, and only
when it exceeds `--sustained` (default 20%) does busywatch warn: a desktop
notification plus a log line naming the **top CPU consumers** at that moment
(sampled only when a warning actually fires).

## Build & install

```sh
cargo build --release
install -m755 target/release/busywatch ~/.local/bin/busywatch
sudo setcap cap_sys_resource+ep ~/.local/bin/busywatch   # PSI triggers need this
systemctl --user enable --now busywatch.service          # unit in ./busywatch.service
```

The `setcap` must be repeated after every reinstall of the binary. Without the
capability (or without PSI), busywatch falls back to sampling the pressure file
every `--poll-secs` (default 10 s) — one small file read, still negligible.

## Options

| Flag | Default | Meaning |
|---|---|---|
| `--stall-ms` | 500 | trigger: stall time within the window |
| `--window-ms` | 1000 | trigger: window size |
| `--sustained` | 20 | warn when avg60 CPU pressure ≥ this % |
| `--cooldown` | 300 | seconds between warnings |
| `--top` | 3 | processes named in the warning |
| `--poll-secs` | 10 | fallback sampling interval |
| `--no-notify` | | log only, no desktop notification |

## Origin

Written after WirePlumber silently burned 2.5 CPU-hours digesting a stuck
Bluetooth discovery scan — a warning naming the hot process would have
surfaced that in minutes instead of hours.
