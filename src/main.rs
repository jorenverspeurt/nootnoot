use chrono::{self, Duration as ChronoDuration};
use clap::Parser;
use ctrlc;
use ping_rs;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::{fs, io::Write, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use warp::Filter;

#[derive(Parser, Debug)]
#[command(version = "1.0", about = "Monitors host availability and latency")]
struct Args {
    /// Path to the configuration file
    #[arg(short, long, default_value = "/etc/nootnoot/config.toml")]
    config: String,

    /// Also output logs to stdout
    #[arg(short, long)]
    stdout: bool,

    /// Print every response, or just the interruptions
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Deserialize, Debug)]
struct Config {
    hosts: Vec<HostConfig>,
    address: Option<String>,
    port: Option<u16>,
    log_path: Option<String>,
    log_buffer_size: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct HostConfig {
    address: String,
    frequency: Option<u64>,         // Frequency in seconds
    timeout: Option<u64>,           // Timeout in milliseconds
    avail_frequency: Option<u64>,   // Frequency in seconds
    unavail_frequency: Option<u64>, // Frequency in seconds
    unavail_threshold: Option<u64>, // Threshold in number of failures
}

impl HostConfig {
    fn validate(&self) -> Result<(), String> {
        let freq_count = self.frequency.is_some() as u8
            + self.avail_frequency.is_some() as u8
            + self.unavail_frequency.is_some() as u8;
        if freq_count == 3 {
            return Err(format!("Host {}: 'frequency', 'avail_frequency', and 'unavail_frequency' cannot all be specified at the same time", self.address));
        }

        let timeout = self.timeout.unwrap_or(1000);
        let min_frequency = timeout / 1000;
        if self.get_avail_frequency() <= min_frequency
            || self.get_unavail_frequency() <= min_frequency
        {
            return Err(format!(
                "Host {}: All frequencies must be greater than 'timeout' / 1000",
                self.address
            ));
        }

        Ok(())
    }

    fn get_timeout(&self) -> u64 {
        self.timeout.unwrap_or(1000)
    }

    fn get_avail_frequency(&self) -> u64 {
        self.avail_frequency
            .or(self.frequency)
            .expect("Either 'avail_frequency' or 'frequency' must be specified")
    }

    fn get_unavail_frequency(&self) -> u64 {
        self.unavail_frequency
            .or(self.frequency)
            .expect("Either 'unavail_frequency' or 'frequency' must be specified")
    }

    fn get_unavail_threshold(&self) -> u64 {
        self.unavail_threshold.unwrap_or(1)
    }
}

#[derive(Debug, Clone)]
struct HostStats {
    reachable: u64,
    unreachable: u64,
    total_latency: u64,
    count: u64,
    unavail_log: Vec<(chrono::DateTime<chrono::Local>, ChronoDuration)>,
    log_buffer_size: usize,
}

impl HostStats {
    fn new(log_buffer_size: usize) -> Self {
        HostStats {
            reachable: 0,
            unreachable: 0,
            total_latency: 0,
            count: 0,
            unavail_log: Vec::with_capacity(log_buffer_size),
            log_buffer_size,
        }
    }

    fn update(
        &mut self,
        result: Option<Duration>,
        interruption_start: Option<chrono::DateTime<chrono::Local>>,
    ) {
        self.count += 1;
        match result {
            Some(latency) => {
                self.reachable += 1;
                self.total_latency += latency.as_millis() as u64;
                if let Some(start) = interruption_start {
                    let duration = chrono::Local::now() - start;
                    if self.unavail_log.len() == self.log_buffer_size {
                        self.unavail_log.remove(0);
                    }
                    self.unavail_log.push((start, duration));
                }
            }
            None => {
                self.unreachable += 1;
            }
        }
    }

    fn average_latency(&self) -> Option<u64> {
        if self.reachable > 0 {
            Some(self.total_latency / self.reachable)
        } else {
            None
        }
    }

    fn reachable_percentage(&self) -> f64 {
        if self.count > 0 {
            (self.reachable as f64 / self.count as f64) * 100.0
        } else {
            0.0
        }
    }
}

impl Serialize for HostStats {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("HostStats", 6)?;
        state.serialize_field("reachable", &self.reachable)?;
        state.serialize_field("unreachable", &self.unreachable)?;
        state.serialize_field("total_latency", &self.total_latency)?;
        state.serialize_field("count", &self.count)?;
        state.serialize_field(
            "unavail_log",
            &self
                .unavail_log
                .iter()
                .map(|(start, duration)| (start.to_rfc3339(), duration.num_seconds()))
                .collect::<Vec<_>>(),
        )?;
        state.serialize_field("log_buffer_size", &self.log_buffer_size)?;
        state.end()
    }
}

fn read_config(config_path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let config_data = fs::read_to_string(config_path)?;
    let config: Config = toml::from_str(&config_data)?;
    Ok(config)
}

fn write_log(log_path: &Path, message: &str, stdout: bool) {
    if stdout {
        println!("{}", message);
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        writeln!(file, "{}", message).unwrap_or_else(|err| {
            eprintln!("Failed to write log: {}", err);
        });
    } else {
        eprintln!("Failed to open log file: {:?}", log_path);
    }
}

async fn ping_host(address: &str, timeout: u64) -> Option<Duration> {
    let addr = address.parse().ok()?;
    let data = [1, 2, 3, 4];
    let data_arc = Arc::new(&data[..]);
    let options = ping_rs::PingOptions {
        ttl: 128,
        dont_fragment: true,
    };
    let result = ping_rs::send_ping_async(
        &addr,
        Duration::from_millis(timeout),
        data_arc,
        Some(&options),
    )
    .await;
    match result {
        Ok(reply) => Some(Duration::from_millis(reply.rtt as u64)),
        Err(_) => None,
    }
}

async fn run_service(
    config: Config,
    log_path: PathBuf,
    stats: Arc<RwLock<HashMap<String, HostStats>>>,
    stop_flag: Arc<AtomicBool>,
    stdout: bool,
    verbose: bool,
) {
    let mut handles = vec![];

    for host in config.hosts {
        host.validate().expect("Invalid host configuration");

        let log_path = log_path.clone();
        let stats = Arc::clone(&stats);
        let stop_flag = Arc::clone(&stop_flag);
        let log_buffer_size = config.log_buffer_size.unwrap_or(10);

        handles.push(tokio::spawn(async move {
            let mut interruption_start: Option<chrono::DateTime<chrono::Local>> = None;
            let mut failure_count = 0;
            let mut current_frequency = host.get_avail_frequency();

            while !stop_flag.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(current_frequency)).await;
                let result = ping_host(&host.address, host.get_timeout()).await;
                let mut stats_guard = stats.write().await;
                let host_stats = stats_guard
                    .entry(host.address.clone())
                    .or_insert_with(|| HostStats::new(log_buffer_size));
                host_stats.update(result, interruption_start);

                let avg_latency = host_stats
                    .average_latency()
                    .map(|lat| format!("{}ms", lat))
                    .unwrap_or_else(|| "N/A".to_string());

                if verbose {
                    let log_entry = format!(
                        "[{}] {} time={} mean={}",
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                        host.address,
                        result
                            .map(|r| format!("{}ms", r.as_millis()))
                            .unwrap_or_else(|| "unreachable".to_string()),
                        avg_latency
                    );
                    write_log(&log_path, &log_entry, stdout);
                }

                if result.is_none() {
                    failure_count += 1;
                    if failure_count >= host.get_unavail_threshold() && interruption_start.is_none()
                    {
                        interruption_start = Some(chrono::Local::now());
                        let log_entry = format!(
                            "[{}] {} unreachable!",
                            interruption_start.unwrap().format("%Y-%m-%d %H:%M:%S"),
                            host.address
                        );
                        write_log(&log_path, &log_entry, stdout);
                    }
                    current_frequency = host.get_unavail_frequency();
                } else {
                    failure_count = 0;
                    if interruption_start.is_some() {
                        interruption_start = None;
                        let log_entry = format!(
                            "[{}] {} reachable again!",
                            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                            host.address
                        );
                        write_log(&log_path, &log_entry, stdout);
                    }
                    current_frequency = host.get_avail_frequency();
                }
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config = read_config(&args.config).expect("Failed to read configuration");
    let address = config
        .address
        .clone()
        .unwrap_or_else(|| "0.0.0.0".to_string());
    let port = config.port.unwrap_or(8080);
    let log_path = Path::new(config.log_path.as_deref().unwrap_or("./nootnoot.log")).to_path_buf();
    let stats: Arc<RwLock<HashMap<String, HostStats>>> = Arc::new(RwLock::new(HashMap::new()));
    let stop_flag = Arc::new(AtomicBool::new(false));

    let stats_clone = Arc::clone(&stats);
    let stop_flag_clone = Arc::clone(&stop_flag);
    thread::spawn(move || {
        ctrlc::set_handler(move || {
            eprintln!("Shutting down service...");
            stop_flag_clone.store(true, Ordering::Relaxed);
        })
        .expect("Error setting Ctrl-C handler");
    });

    tokio::spawn(async move {
        warp::serve(warp::path("dashboard").and_then(move || {
            let stats_clone = Arc::clone(&stats_clone);
            async move {
                let stats_guard = stats_clone.read().await;
                let response: Vec<_> =
                    stats_guard
                        .iter()
                        .map(|(host, stats)| {
                            let status_color = if stats.unreachable > 0 {
                                "red"
                            } else {
                                "green"
                            };
                            let unavail_log: String = stats
                                .unavail_log
                                .iter()
                                .map(|(start, duration)| {
                                    format!(
                                        "<li>{} - {} seconds</li>",
                                        start.format("%Y-%m-%d %H:%M:%S"),
                                        duration.num_seconds()
                                    )
                                })
                                .collect();
                            format!(
                        "<div style='border: 1px solid black; padding: 10px; margin: 10px;'>
                            <h2 style='color: {};'>{}</h2>
                            <p>Reachable: {}</p>
                            <p>Unreachable: {}</p>
                            <p>Average Latency: {} ms</p>
                            <p>Reachable Percentage: {:.2}%</p>
                            <h3>Unavailability Log:</h3>
                            <ul>{}</ul>
                        </div>",
                        status_color, host, stats.reachable, stats.unreachable,
                        stats.average_latency().map_or("N/A".to_string(), |lat| lat.to_string()),
                        stats.reachable_percentage(), unavail_log
                    )
                        })
                        .collect();
                Ok::<_, warp::Rejection>(warp::reply::html(response.join("")))
            }
        }))
        .run((address.parse::<IpAddr>().unwrap(), port))
        .await;
    });

    run_service(
        config,
        log_path,
        Arc::clone(&stats),
        stop_flag,
        args.stdout,
        args.verbose,
    )
    .await;
}
