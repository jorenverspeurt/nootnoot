# Noot Noot

Noot Noot is a utility to continuously monitor the availability and latency of hosts by sending periodic ping requests. It provides a web dashboard to visualize the status and average latency of the monitored hosts.

## Features

- Monitor multiple hosts
- Configurable ping frequency and timeout
- Different ping frequencies depending on current availability
- Log ping results to a file
- Web dashboard to visualize host status and latency

## Installation

To install Noot Noot, you need to have [Rust](https://www.rust-lang.org/) installed. Then, you can build the project using Cargo:

```sh
git clone https://github.com/yourusername/nootnoot.git
cd nootnoot
cargo build --release
```

The built binary will be in `target/release`.

To install the binary to your Cargo `bin` directory, use `cargo install`:

```sh
cargo install --path .
```

## Usage

To run Noot Noot, you need to provide a configuration file. You can specify the path to the configuration file using the `-c` or `--config` option. Additionally, you can enable logging to stdout and verbose mode using the `-s` or `--stdout` and `-v` or `--verbose` options, respectively.

```sh
./target/release/nootnoot -c /path/to/config.toml -s -v
```

In verbose mode, the tool continuously logs the results of the pings to stdout, not just when the availability status changes.

### Command Line Options

- `-c, --config <FILE>`: Path to the configuration file (default: `/etc/nootnoot/config.toml`)
- `-s, --stdout`: Also output logs to stdout
- `-v, --verbose`: Print every response, or just the interruptions

## Configuration

The configuration file is written in TOML format. Below is an example configuration file with all available options:

```toml
# Configuration for the nootnoot tool
# -----------------------------------

# Server address to bind the dashboard (optional, default: "0.0.0.0")
address = "0.0.0.0"

# Port to bind the dashboard (optional, default: 8080)
port = 8080

# Path to the log file (optional, default: "./nootnoot.log")
log_path = "./nootnoot.log"

# Size of the log buffer (optional, default: 10)
log_buffer_size = 10

# List of hosts to monitor
# ------------------------
[[hosts]]
# The address of the host to monitor (required)
address = "192.168.1.1"

# Interval at which to ping the host in seconds (optional, default: None)
# Only one of 'frequency', 'avail_frequency', or 'unavail_frequency' should be specified
frequency = 30

# Timeout for each ping in milliseconds (optional, default: 1000)
timeout = 500

# Number of consecutive failures before considering the host unavailable (optional, default: 1)
unavail_threshold = 3

[[hosts]]
address = "8.8.8.8"

# Interval at which to ping the host while it is still available in seconds (optional, default: None)
avail_frequency = 60

# Interval at which to ping the host when it is unavailable in seconds (optional, default: None)
unavail_frequency = 10
```

## Dashboard

Noot Noot provides a web dashboard to visualize the status and statistics of the monitored hosts. The dashboard is accessible at `http://<address>:<port>/dashboard`, where `<address>` and `<port>` are specified in the configuration file.

## License

This project is licensed under the 3-Clause BSD License. See the [LICENSE](LICENSE) file for details.
