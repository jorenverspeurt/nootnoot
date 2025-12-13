use std::{
    collections::{HashMap, VecDeque},
    fs::OpenOptions,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use clap::{ArgAction, Parser};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{process::Command, signal, sync::mpsc};

mod webui;

#[derive(Parser, Debug)]
#[command(name = "nootnoot")]
#[command(author = "You")]
#[command(version)]
#[command(about = "Simple multi-host ping monitor with optional web dashboard")]
struct Args {
    /// Path to configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Host specification: name,addr,up_ms,down_ms (can be repeated)
    /// Example: --host "router,192.168.0.1,1000,3000"
    #[arg(long)]
    host: Vec<String>,

    /// Log file path (overrides config if set). If omitted, logs go to stdout in ad-hoc usage.
    #[arg(long)]
    log_file: Option<PathBuf>,

    /// Enable web dashboard (overrides config)
    #[arg(long, action = ArgAction::SetTrue)]
    web: bool,

    /// Bind address for web dashboard, e.g. 0.0.0.0:8080
    #[arg(long)]
    web_bind: Option<String>,

    /// Summary interval in seconds (overrides config)
    #[arg(long)]
    summary_interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    hosts: Vec<HostConfig>,
    #[serde(default = "default_summary_interval")]
    summary_interval_secs: u64,
    /// Optional global log file. If omitted, stdout is used (CLI style).
    log_file: Option<PathBuf>,
    web: Option<WebConfig>,
}

#[derive(Debug, Deserialize)]
struct WebConfig {
    /// Example: "0.0.0.0:8080"
    bind: String,
}

fn default_summary_interval() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize)]
struct HostConfig {
    name: String,
    address: String,
    /// Ping interval in ms when host is reachable
    up_interval_ms: u64,
    /// Ping interval in ms when host is not reachable
    down_interval_ms: u64,
    /// Optional detailed log file for all pings
    detailed_log: Option<PathBuf>,
}

#[derive(Debug, Error)]
enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("Failed to parse TOML config: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("Invalid --host argument: {0}")]
    InvalidHostArg(String),
    #[error("No hosts configured")]
    NoHosts,
}

/// Shared logger for summary & reachability messages
#[derive(Clone)]
struct Logger {
    inner: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Logger {
    fn new_to_stdout() -> Self {
        Logger {
            inner: Arc::new(Mutex::new(Box::new(io::stdout()) as Box<dyn Write + Send>)),
        }
    }

    fn new_to_file(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Logger {
            inner: Arc::new(Mutex::new(Box::new(file) as Box<dyn Write + Send>)),
        })
    }

    fn log_line(&self, line: &str) {
        let mut guard = self.inner.lock();
        let _ = writeln!(guard, "{}", line);
        let _ = guard.flush();
    }
}

/// Per-host detailed logger
#[derive(Clone)]
struct DetailedLogger {
    inner: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl DetailedLogger {
    fn new(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(DetailedLogger {
            inner: Arc::new(Mutex::new(Box::new(file) as Box<dyn Write + Send>)),
        })
    }

    fn log_ping(&self, timestamp: DateTime<Utc>, reachable: bool, latency_ms: Option<f64>) {
        let mut guard = self.inner.lock();
        let _ = writeln!(
            guard,
            "{},reachable={},latency_ms={}",
            timestamp.to_rfc3339(),
            reachable,
            latency_ms
                .map(|v| format!("{:.3}", v))
                .unwrap_or_else(|| "NaN".to_string())
        );
        let _ = guard.flush();
    }
}

/// Event sent from host tasks to the stats aggregator & web dashboard
#[derive(Debug)]
struct PingSample {
    host_name: String,
    timestamp: DateTime<Utc>,
    reachable: bool,
    latency_ms: Option<f64>,
}

/// Simple online stats (Welford)
#[derive(Debug, Clone)]
struct OnlineStats {
    count: u64,
    mean: f64,
    m2: f64,
}

impl OnlineStats {
    fn new() -> Self {
        OnlineStats {
            count: 0,
            mean: 0.0,
            m2: 0.0,
        }
    }

    fn add_sample(&mut self, x: f64) {
        self.count += 1;
        let delta = x - self.mean;
        self.mean += delta / (self.count as f64);
        let delta2 = x - self.mean;
        self.m2 += delta * delta2;
    }

    fn mean(&self) -> Option<f64> {
        if self.count > 0 {
            Some(self.mean)
        } else {
            None
        }
    }

    fn stddev(&self) -> Option<f64> {
        if self.count > 1 {
            Some((self.m2 / ((self.count - 1) as f64)).sqrt())
        } else {
            None
        }
    }
}

/// Web dashboard state

#[derive(Debug, Clone, Serialize)]
struct ReachabilityEvent {
    timestamp: DateTime<Utc>,
    reachable: bool,
}

#[derive(Debug, Clone, Serialize)]
struct LatencySample {
    timestamp: DateTime<Utc>,
    latency_ms: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize)]
struct HostWebState {
    last_status: Option<bool>,
    reachability_events: VecDeque<ReachabilityEvent>, // limited to last 3 events
    latency_samples: VecDeque<LatencySample>,         // last 3h
}

#[derive(Clone)]
pub struct WebState {
    inner: Arc<RwLock<HashMap<String, HostWebState>>>,
}

impl WebState {
    fn new(hosts: &[HostConfig]) -> Self {
        let mut map = HashMap::new();
        for h in hosts {
            map.insert(h.name.clone(), HostWebState::default());
        }
        WebState {
            inner: Arc::new(RwLock::new(map)),
        }
    }

    fn update_from_sample(&self, sample: &PingSample) {
        const MAX_REACHABILITY_EVENTS: usize = 3;
        const MAX_WINDOW: Duration = Duration::from_secs(3 * 60 * 60); // 3 hours

        let mut guard = self.inner.write();
        let state = guard.entry(sample.host_name.clone()).or_default();

        // reachability change detection
        let prev_status = state.last_status;
        if prev_status != Some(sample.reachable) {
            let ev = ReachabilityEvent {
                timestamp: sample.timestamp,
                reachable: sample.reachable,
            };
            state.reachability_events.push_front(ev);
            while state.reachability_events.len() > MAX_REACHABILITY_EVENTS {
                state.reachability_events.pop_back();
            }
            state.last_status = Some(sample.reachable);
        }

        // latency samples
        state.latency_samples.push_back(LatencySample {
            timestamp: sample.timestamp,
            latency_ms: sample.latency_ms,
        });

        // trim old samples (> 3h)
        let cutoff = sample.timestamp - chrono::Duration::from_std(MAX_WINDOW).unwrap();
        while let Some(front) = state.latency_samples.front() {
            if front.timestamp < cutoff {
                state.latency_samples.pop_front();
            } else {
                break;
            }
        }
    }

    fn snapshot(&self) -> HashMap<String, HostWebState> {
        self.inner.read().clone()
    }
}

// ============ Config loading ============

fn parse_cli_hosts(args: &Args) -> Result<Vec<HostConfig>, ConfigError> {
    let mut hosts = Vec::new();
    for hs in &args.host {
        // "name,addr,up_ms,down_ms"
        let parts: Vec<_> = hs.split(',').map(|s| s.trim()).collect();
        if parts.len() != 4 {
            return Err(ConfigError::InvalidHostArg(hs.clone()));
        }
        let name = parts[0].to_string();
        let address = parts[1].to_string();
        let up_interval_ms: u64 = parts[2]
            .parse()
            .map_err(|_| ConfigError::InvalidHostArg(hs.clone()))?;
        let down_interval_ms: u64 = parts[3]
            .parse()
            .map_err(|_| ConfigError::InvalidHostArg(hs.clone()))?;
        hosts.push(HostConfig {
            name,
            address,
            up_interval_ms,
            down_interval_ms,
            detailed_log: None,
        });
    }
    Ok(hosts)
}

fn load_file_config_from(path: &Path) -> Result<FileConfig, ConfigError> {
    let content = std::fs::read_to_string(path)?;
    let cfg: FileConfig = toml::from_str(&content)?;
    Ok(cfg)
}

/// Look for a config file in priority order:
/// 1. ./nootnoot.toml
/// 2. ~/.config/nootnoot.toml
/// 3. /etc/nootnoot.toml
fn find_default_config_file() -> Option<PathBuf> {
    let current = PathBuf::from("nootnoot.toml");
    if current.exists() {
        return Some(current);
    }

    if let Some(home) = std::env::var_os("HOME") {
        let p = Path::new(&home).join(".config/nootnoot.toml");
        if p.exists() {
            return Some(p);
        }
    }

    let etc = PathBuf::from("/etc/nootnoot.toml");
    if etc.exists() {
        return Some(etc);
    }

    None
}

#[allow(clippy::type_complexity)]
fn load_config(
    args: &Args,
) -> Result<(Vec<HostConfig>, u64, Option<PathBuf>, Option<WebConfig>), ConfigError> {
    // First priority: explicit --host
    if !args.host.is_empty() {
        let hosts = parse_cli_hosts(args)?;
        if hosts.is_empty() {
            return Err(ConfigError::NoHosts);
        }

        let summary = args.summary_interval.unwrap_or(default_summary_interval());

        let web_cfg = if args.web {
            Some(WebConfig {
                bind: args
                    .web_bind
                    .clone()
                    .unwrap_or_else(|| "127.0.0.1:8080".to_string()),
            })
        } else {
            None
        };

        return Ok((hosts, summary, args.log_file.clone(), web_cfg));
    }

    // Otherwise: config file
    let path = if let Some(ref p) = args.config {
        Some(p.clone())
    } else {
        find_default_config_file()
    };

    let Some(path) = path else {
        return Err(ConfigError::NoHosts);
    };

    let cfg = load_file_config_from(&path)?;
    if cfg.hosts.is_empty() {
        return Err(ConfigError::NoHosts);
    }

    let summary_interval = args.summary_interval.unwrap_or(cfg.summary_interval_secs);
    let log_file = args.log_file.clone().or(cfg.log_file.clone());

    let web_cfg = if args.web {
        Some(WebConfig {
            bind: args
                .web_bind
                .clone()
                .or_else(|| cfg.web.as_ref().map(|w| w.bind.clone()))
                .unwrap_or_else(|| "127.0.0.1:8080".to_string()),
        })
    } else {
        // If CLI didn't force web, use file config as-is
        cfg.web.as_ref().map(|w| WebConfig {
            bind: args.web_bind.clone().unwrap_or_else(|| w.bind.clone()),
        })
    };

    Ok((cfg.hosts, summary_interval, log_file, web_cfg))
}

// ============ Ping implementation ============

#[cfg(not(test))]
async fn ping_once(address: &str, timeout: Duration) -> io::Result<Option<f64>> {
    // Uses system "ping -c 1 -W timeout_secs address"
    // Returns latency in ms, or Ok(None) if unreachable / timeout / parse error
    let timeout_secs = timeout.as_secs().max(1);
    let start = Instant::now();

    let mut cmd = Command::new("ping");
    cmd.arg("-n")
        .arg("-c")
        .arg("1")
        .arg("-W")
        .arg(format!("{}", timeout_secs))
        .arg(address)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let output = cmd.output().await?;
    if !output.status.success() {
        return Ok(None);
    }

    let dur = start.elapsed();
    let approx_latency_ms = dur.as_secs_f64() * 1000.0;

    // Try to parse "time=X ms" from output for more accurate value
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(idx) = stdout.find("time=") {
        let rest = &stdout[idx + 5..];
        if let Some(end_idx) = rest.find(" ms") {
            let num_str = rest[..end_idx].trim();
            if let Ok(val) = num_str.replace(',', ".").parse::<f64>() {
                return Ok(Some(val));
            }
        }
    }

    Ok(Some(approx_latency_ms))
}

#[cfg(test)]
mod test_ping {
    use super::*;
    use once_cell::sync::Lazy;
    use std::sync::Mutex as StdMutex;

    // Shared test latency; tests can tweak it if needed
    pub static TEST_PING_LATENCY_MS: Lazy<StdMutex<Option<f64>>> =
        Lazy::new(|| StdMutex::new(Some(5.0)));

    pub fn set_test_ping_latency(latency: Option<f64>) {
        *TEST_PING_LATENCY_MS.lock().unwrap() = latency;
    }

    pub(super) async fn ping_once(_address: &str, _timeout: Duration) -> io::Result<Option<f64>> {
        Ok(*TEST_PING_LATENCY_MS.lock().unwrap())
    }
}

#[cfg(test)]
use test_ping::ping_once;

// ============ Host task ============

async fn run_host_task(
    cfg: HostConfig,
    logger: Logger,
    detailed_logger: Option<DetailedLogger>,
    tx: mpsc::Sender<PingSample>,
    web_state: Option<WebState>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let mut reachable: Option<bool> = None;

    loop {
        let timeout = Duration::from_secs(3);
        let ping_result = ping_once(&cfg.address, timeout).await;

        let ts = Utc::now();
        let (is_reachable, latency_ms) = match ping_result {
            Ok(Some(lat)) => (true, Some(lat)),
            Ok(None) => (false, None),
            Err(_) => (false, None),
        };

        // Detailed log
        if let Some(ref dl) = detailed_logger {
            dl.log_ping(ts, is_reachable, latency_ms);
        }

        // Reachability change log
        if reachable != Some(is_reachable) {
            let msg = format!(
                "{} host={} address={} reachable={}",
                ts.to_rfc3339(),
                cfg.name,
                cfg.address,
                is_reachable
            );
            logger.log_line(&msg);
            reachable = Some(is_reachable);
        }

        // Send sample to stats aggregator & web
        let sample = PingSample {
            host_name: cfg.name.clone(),
            timestamp: ts,
            reachable: is_reachable,
            latency_ms,
        };

        if let Some(ref ws) = web_state {
            ws.update_from_sample(&sample);
        }

        // ignore if channel closed
        let _ = tx.send(sample).await;

        // pick next interval
        let interval_ms = if is_reachable {
            cfg.up_interval_ms
        } else {
            cfg.down_interval_ms
        };

        let sleep_dur = Duration::from_millis(interval_ms);

        tokio::select! {
            _ = tokio::time::sleep(sleep_dur) => {},
            _ = shutdown_rx.recv() => {
                // graceful shutdown
                break;
            }
        }
    }
}

// ============ Stats aggregator ============

async fn run_stats_aggregator(
    mut rx: mpsc::Receiver<PingSample>,
    logger: Logger,
    summary_interval_secs: u64,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let mut per_host_stats: HashMap<String, OnlineStats> = HashMap::new();
    let mut last_summary = Instant::now();
    let summary_interval = Duration::from_secs(summary_interval_secs);

    loop {
        tokio::select! {
            maybe_sample = rx.recv() => {
                match maybe_sample {
                    Some(sample) => {
                        if let Some(lat) = sample.latency_ms {
                            let stats = per_host_stats
                                .entry(sample.host_name.clone())
                                .or_insert_with(OnlineStats::new);
                            stats.add_sample(lat);
                        }

                        let now = Instant::now();
                        if now.duration_since(last_summary) >= summary_interval {
                            let ts = Utc::now();
                            for (host, stats) in &per_host_stats {
                                let line = format!(
                                    "{} summary host={} count={} mean_ms={:.3} stddev_ms={:.3}",
                                    ts.to_rfc3339(),
                                    host,
                                    stats.count,
                                    stats.mean().unwrap_or(f64::NAN),
                                    stats.stddev().unwrap_or(f64::NAN)
                                );
                                logger.log_line(&line);
                            }
                            last_summary = now;
                        }
                    }
                    None => {
                        // all senders dropped
                        break;
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                // print one last summary on shutdown
                let ts = Utc::now();
                for (host, stats) in &per_host_stats {
                    let line = format!(
                        "{} summary host={} count={} mean_ms={:.3} stddev_ms={:.3}",
                        ts.to_rfc3339(),
                        host,
                        stats.count,
                        stats.mean().unwrap_or(f64::NAN),
                        stats.stddev().unwrap_or(f64::NAN)
                    );
                    logger.log_line(&line);
                }
                break;
            }
        }
    }
}

// ============ Signals ============

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut term = signal(SignalKind::terminate()).expect("failed to bind SIGTERM handler");
        tokio::select! {
            _ = signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        let _ = signal::ctrl_c().await;
    }
}

// ============ main ============

#[allow(suspicious_double_ref_op)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // simple tracing setup
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let (hosts, summary_interval, log_file, web_cfg) = match load_config(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Configuration error: {}", e);
            std::process::exit(1);
        }
    };

    let logger = if let Some(ref p) = log_file {
        Logger::new_to_file(p)?
    } else {
        Logger::new_to_stdout()
    };

    // optional web state
    let web_state = web_cfg.as_ref().map(|_| WebState::new(&hosts));

    // channel for samples
    let (tx, rx) = mpsc::channel::<PingSample>(1024);

    // broadcast for shutdown
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(16);

    // spawn stats aggregator
    let stats_logger = logger.clone();
    let stats_shutdown_rx = shutdown_tx.subscribe();
    let stats_handle = tokio::spawn(async move {
        run_stats_aggregator(rx, stats_logger, summary_interval, stats_shutdown_rx).await;
    });

    // spawn web server if enabled
    let web_handle = if let (Some(cfg), Some(ref ws)) = (web_cfg.as_ref(), web_state.as_ref()) {
        let app = webui::build_router(ws.clone().clone());

        let addr: std::net::SocketAddr = cfg.bind.parse().expect("invalid web bind address");
        let shutdown_for_server = shutdown_tx.clone();

        Some(tokio::spawn(async move {
            tracing::info!("Starting web dashboard on {}", addr);
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("failed to bind TCP listener");

            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_for_server.subscribe().recv().await;
                })
                .await
                .unwrap();
        }))
    } else {
        None
    };

    // spawn host tasks
    let mut host_handles = Vec::new();
    for h in hosts {
        let logger_clone = logger.clone();
        let tx_clone = tx.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        let detailed_log = h
            .detailed_log
            .as_ref()
            .map(|p| DetailedLogger::new(p))
            .transpose()?;

        let web_state_clone = web_state.clone();
        let cfg_clone = h.clone();

        let handle = tokio::spawn(async move {
            run_host_task(
                cfg_clone,
                logger_clone,
                detailed_log,
                tx_clone,
                web_state_clone,
                shutdown_rx,
            )
            .await;
        });

        host_handles.push(handle);
    }

    // Wait for shutdown signal
    shutdown_signal().await;
    tracing::info!("Shutdown signal received, finishing...");

    // notify all tasks
    let _ = shutdown_tx.send(());

    // wait for host tasks
    for h in host_handles {
        let _ = h.await;
    }

    // close sample channel so aggregator can exit
    drop(tx);
    let _ = stats_handle.await;

    if let Some(h) = web_handle {
        let _ = h.await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ping::set_test_ping_latency;
    use tempfile;
    use tokio::sync::broadcast;
    use tokio::time::{sleep, Duration};

    // --- Config tests ---

    #[test]
    fn parse_cli_hosts_works() {
        let args = crate::Args {
            config: None,
            host: vec!["router,1.2.3.4,1000,5000".to_string()],
            log_file: None,
            web: false,
            web_bind: None,
            summary_interval: None,
        };

        let (hosts, summary_interval, log_file, web_cfg) =
            load_config(&args).expect("config should load");

        assert_eq!(hosts.len(), 1);
        let h = &hosts[0];
        assert_eq!(h.name, "router");
        assert_eq!(h.address, "1.2.3.4");
        assert_eq!(h.up_interval_ms, 1000);
        assert_eq!(h.down_interval_ms, 5000);

        assert_eq!(summary_interval, default_summary_interval());
        assert!(log_file.is_none());
        assert!(web_cfg.is_none());
    }

    #[test]
    fn load_config_from_file_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nootnoot.toml");

        std::fs::write(
            &path,
            r#"
summary_interval_secs = 42

[[hosts]]
name = "host1"
address = "10.0.0.1"
up_interval_ms = 100
down_interval_ms = 200
"#,
        )
        .unwrap();

        let args = crate::Args {
            config: Some(path),
            host: Vec::new(),
            log_file: None,
            web: false,
            web_bind: None,
            summary_interval: None,
        };

        let (hosts, summary_interval, log_file, web_cfg) =
            load_config(&args).expect("config from file should load");

        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "host1");
        assert_eq!(hosts[0].address, "10.0.0.1");
        assert_eq!(hosts[0].up_interval_ms, 100);
        assert_eq!(hosts[0].down_interval_ms, 200);

        assert_eq!(summary_interval, 42);
        assert!(log_file.is_none());
        assert!(web_cfg.is_none());
    }

    // --- Web state tests ---

    #[test]
    fn web_state_tracks_reachability_changes() {
        let hosts = vec![HostConfig {
            name: "h".to_string(),
            address: "1.2.3.4".to_string(),
            up_interval_ms: 1000,
            down_interval_ms: 1000,
            detailed_log: None,
        }];

        let ws = WebState::new(&hosts);

        let t1 = chrono::Utc::now();

        let s1 = PingSample {
            host_name: "h".into(),
            timestamp: t1,
            reachable: true,
            latency_ms: Some(10.0),
        };

        ws.update_from_sample(&s1);

        let snap = ws.snapshot();
        let host_state = snap.get("h").expect("host exists in snapshot");
        assert_eq!(host_state.last_status, Some(true));
        assert_eq!(host_state.reachability_events.len(), 1);

        let s2 = PingSample {
            host_name: "h".into(),
            timestamp: t1 + chrono::Duration::seconds(1),
            reachable: false,
            latency_ms: None,
        };

        ws.update_from_sample(&s2);
        let snap2 = ws.snapshot();
        let host_state2 = snap2.get("h").unwrap();
        assert_eq!(host_state2.last_status, Some(false));
        assert_eq!(host_state2.reachability_events.len(), 2);
    }

    // --- Stats aggregator tests ---

    #[tokio::test]
    async fn stats_aggregator_consumes_samples_and_shuts_down() {
        let (tx, rx) = mpsc::channel(16);
        let logger = Logger::new_to_stdout();

        let (sd_tx, sd_rx) = broadcast::channel(1);

        let handle = tokio::spawn(async move {
            run_stats_aggregator(rx, logger, 1, sd_rx).await;
        });

        let now = chrono::Utc::now();

        tx.send(PingSample {
            host_name: "host1".into(),
            timestamp: now,
            reachable: true,
            latency_ms: Some(100.0),
        })
        .await
        .unwrap();

        tx.send(PingSample {
            host_name: "host1".into(),
            timestamp: now,
            reachable: true,
            latency_ms: Some(200.0),
        })
        .await
        .unwrap();

        // Give it time to hit the summary interval
        sleep(Duration::from_millis(1200)).await;

        // Trigger shutdown
        sd_tx.send(()).unwrap();
        handle.await.unwrap();
    }

    // --- Host task tests ---

    #[tokio::test]
    async fn host_task_emits_samples_and_respects_shutdown() {
        // Make test ping return a fixed latency
        set_test_ping_latency(Some(5.0));

        let host = HostConfig {
            name: "h".into(),
            address: "127.0.0.1".into(),
            up_interval_ms: 10,
            down_interval_ms: 10,
            detailed_log: None,
        };

        let logger = Logger::new_to_stdout();
        let (tx, mut rx) = mpsc::channel(16);

        let hosts = vec![host.clone()];
        let ws = Some(WebState::new(&hosts));

        let (sd_tx, sd_rx) = broadcast::channel(1);

        let task = tokio::spawn(async move {
            run_host_task(host, logger, None, tx, ws, sd_rx).await;
        });

        // Wait for at least one sample
        let sample = rx.recv().await.expect("expected a sample from host task");
        assert_eq!(sample.host_name, "h");
        assert_eq!(sample.latency_ms, Some(5.0));
        assert!(sample.reachable);

        // Shut down the task
        sd_tx.send(()).unwrap();
        task.await.unwrap();
    }
}
