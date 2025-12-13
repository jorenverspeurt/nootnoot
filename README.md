# nootnoot — Multi-Host Ping Monitor with Optional Web Dashboard

`nootnoot` is a lightweight, efficient Rust daemon for monitoring the reachability and latency of multiple hosts.
It is designed for **network monitoring**, and can run either:

- as a **systemd service**, or
- interactively from the **command line**.

The tool can adapt its ping rate based on host availability, it logs reachability changes and latency summaries, and can optionally expose a simple **web dashboard** with recent status and latency history.

`nootnoot` is licensed under the **BSD-3-Clause** license (see the **LICENSE** file).

FYI: this project is developed with the help of AI-based tools.

---

## Features

### ✔ Monitor multiple hosts
Each host has configurable:
- Address
- Ping interval when reachable (`up_interval_ms`)
- Ping interval when unreachable (`down_interval_ms`)
- Optional per-host detailed log file

### ✔ Efficient & robust
- Async Rust using Tokio
- Low CPU and memory usage
- Safe handling of shutdown (SIGTERM, Ctrl-C)

### ✔ Reachability & latency logging
- Logs UP/DOWN events
- Logs periodic latency summaries (mean + stddev)
- Optional detailed per-ping logging

### ✔ Optional web dashboard
- Shows 3 most recent reachability changes per host
- Shows last 3 hours of latency samples
- Auto-refresh HTML UI
- JSON API at `/api/state`

### ✔ Flexible configuration
- CLI arguments
- Config file (`./nootnoot.toml`, `~/.config/nootnoot.toml`, or `/etc/nootnoot.toml`)
- CLI overrides config

### ✔ systemd integration
- Uses `DynamicUser=yes`
- Uses `LogsDirectory=nootnoot` (systemd creates `/var/log/nootnoot/` automatically)
- No persistent service users, no manual directory management

---

## Installation

You can install `nootnoot` in several ways:

---

### 1. Install via Makefile (system-wide)

```bash
git clone https://github.com/yourname/nootnoot.git
cd nootnoot
make install
```

Uninstall:

```bash
make uninstall
```

---

### 2. Install via PKGBUILD (Arch Linux)

```bash
cd packaging/arch
makepkg -si
```

Enable the systemd service:

```bash
sudo systemctl enable --now nootnoot.service
```

---

### 3. Install manually (cargo)

```bash
git clone https://github.com/yourname/nootnoot.git
cd nootnoot
cargo build --release
sudo cp target/release/nootnoot /usr/local/bin/
```

---

## Usage (Command Line)

```bash
nootnoot   --host "router,192.168.0.1,1000,5000"   --host "server,10.0.0.10,1000,3000"   --web   --web-bind 0.0.0.0:8080
```

### Key CLI Options

| Option | Description |
|--------|-------------|
| `--config <path>` | Path to configuration file |
| `--host "name,address,up_ms,down_ms"` | Add a host (repeatable) |
| `--log-file <path>` | Override log output |
| `--web` | Enable web dashboard |
| `--web-bind <host:port>` | Bind address for dashboard |
| `--summary-interval <seconds>` | Summary logging interval |

---

## Configuration

Example `/etc/nootnoot.toml`:

```toml
# Interval in seconds for summary logging
summary_interval_secs = 600
# Path to the main log file
log_file = "/var/log/nootnoot/nootnoot.log"

[web]
# Enable the web server on the following bind address
bind = "0.0.0.0:8080"

[[hosts]]
name = "router"
address = "192.168.1.1"
up_interval_ms = 10_000
down_interval_ms = 1_000
detailed_log = "/var/log/nootnoot/router-detail.log"

[[hosts]]
name = "server"
address = "example.org"
up_interval_ms = 60_000
down_interval_ms = 3_000
# no detailed log for this one
```

---

## systemd Usage

```bash
sudo systemctl enable --now nootnoot.service
journalctl -u nootnoot -f
```

The service uses:

```ini
DynamicUser=yes
LogsDirectory=nootnoot
```

---

## Web Dashboard

- Dashboard: `http://<bind-address>/`
- JSON API: `http://<bind-address>/api/state`

---

## Development

```bash
cargo build
cargo run -- --config ./nootnoot.toml
cargo test
```

---

## License

This project is licensed under the **BSD 3-Clause License**.
See the **LICENSE** file for the full text.
