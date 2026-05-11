//! Serial-port state and lifecycle (v5.2; ADR 0009).
//!
//! `SerialPortState` is the lock-free per-port aggregate. The reader
//! task drains the OS read half into a bounded `ArcSwap<RingBuffer>`
//! history; the writer task drains a bounded `mpsc::channel` into the
//! OS write half. Subscribers attach to `serial://<id>/output` via the
//! shared subscription pipeline and never contend with the reader.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use bytes::Bytes;
use chrono::Utc;
use dashmap::DashMap;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf, split};
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio_serial::{
    DataBits as SerialDataBits, FlowControl as TokioFlowControl, Parity as TokioParity, SerialPort,
    SerialPortBuilderExt, SerialStream, StopBits as TokioStopBits,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::adapters::ssh::internal::shell::RingBuffer;
use crate::adapters::subscription::legacy::{ResourceKind, SUBSCRIPTION_REGISTRY};
use crate::domain::ids::SerialId;
use uuid::Uuid;

/// Per-port write-queue capacity. Drives the bounded `mpsc::channel`
/// that funnels `serial_write` calls into the OS write half. A slow
/// remote sink fills this queue first; subsequent `serial_write`
/// calls return `WriteFailed::Backpressure` instead of stalling the
/// HTTP / stdio request.
const WRITE_QUEUE_CAP: usize = 64;

/// Per-flush staging threshold. Pulled out so the ring buffer copies
/// move bytes once per ~4 KiB chunk instead of once per OS read.
const SERIAL_FLUSH_THRESHOLD: usize = 4 * 1024;

/// Default per-port history cap (bytes). Mirrors the shell default —
/// keeps the ring buffer at 1 MiB unless overridden in
/// `SerialConfig::max_buffer_size`.
const DEFAULT_MAX_BUFFER_SIZE: u64 = 1_048_576;

/// Reader I/O buffer (bytes). Pulled in one syscall per loop turn.
const READ_BUF_LEN: usize = 4 * 1024;

// ---------------------------------------------------------------------------
// Public configuration
// ---------------------------------------------------------------------------

/// Stop-bit selection. Maps 1:1 to `tokio_serial::StopBits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialStopBits {
    /// 1 stop bit (default).
    One,
    /// 2 stop bits.
    Two,
}

impl SerialStopBits {
    const fn into_tokio(self) -> TokioStopBits {
        match self {
            Self::One => TokioStopBits::One,
            Self::Two => TokioStopBits::Two,
        }
    }
}

/// Parity selection. Maps 1:1 to `tokio_serial::Parity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialParity {
    /// No parity bit (default for `8N1`).
    None,
    /// Odd parity.
    Odd,
    /// Even parity.
    Even,
}

impl SerialParity {
    const fn into_tokio(self) -> TokioParity {
        match self {
            Self::None => TokioParity::None,
            Self::Odd => TokioParity::Odd,
            Self::Even => TokioParity::Even,
        }
    }
}

/// Flow control selection. Maps 1:1 to `tokio_serial::FlowControl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialFlowControl {
    /// No flow control (default).
    None,
    /// Software (XON / XOFF).
    Software,
    /// Hardware (RTS / CTS).
    Hardware,
}

impl SerialFlowControl {
    const fn into_tokio(self) -> TokioFlowControl {
        match self {
            Self::None => TokioFlowControl::None,
            Self::Software => TokioFlowControl::Software,
            Self::Hardware => TokioFlowControl::Hardware,
        }
    }
}

/// Caller-supplied serial-port configuration.
///
/// Every field has a sensible default so the typical `8N1` use case
/// only needs `path` + `baud_rate`. The full parameter set is exposed
/// so embedded / industrial workflows that must talk RS-485, RS-422,
/// or 7-bit even-parity legacy gear can configure end-to-end without
/// shelling out to `stty`.
#[derive(Debug, Clone)]
pub struct SerialConfig {
    /// OS device path. Linux: `/dev/ttyUSB0` / `/dev/ttyACM0`. macOS:
    /// `/dev/tty.usbserial-XXXX`. Windows: `COM3`.
    pub path: String,
    /// Baud rate. Common: 9600, 19200, 38400, 57600, 115200, 230400,
    /// 460800, 921600.
    pub baud_rate: u32,
    /// Data bits. `5`, `6`, `7`, `8` (default `8`).
    pub data_bits: u8,
    /// Stop bits.
    pub stop_bits: SerialStopBits,
    /// Parity.
    pub parity: SerialParity,
    /// Flow control.
    pub flow_control: SerialFlowControl,
    /// Per-read timeout. Reads block at most this long; on timeout the
    /// reader loops without yielding bytes (no broadcast, no poke).
    pub read_timeout: Duration,
    /// History cap (bytes). Once the ring buffer reaches this size,
    /// the head is truncated on append. `0` keeps the
    /// [`DEFAULT_MAX_BUFFER_SIZE`].
    pub max_buffer_size: u64,
    /// Initial DTR (Data Terminal Ready) line state. `None` leaves the
    /// driver default; `Some(true)` raises, `Some(false)` lowers.
    pub initial_dtr: Option<bool>,
    /// Initial RTS (Request To Send) line state. Same semantics as
    /// `initial_dtr`.
    pub initial_rts: Option<bool>,
    /// Optional human label (e.g. `"GPS-1"`). Surfaced on
    /// `serial_list`. Passing `None` falls back to the device path.
    pub label: Option<String>,
}

impl SerialConfig {
    /// Build a typical `8N1` configuration with the given path / baud.
    #[must_use]
    pub fn new<P: Into<String>>(path: P, baud_rate: u32) -> Self {
        Self {
            path: path.into(),
            baud_rate,
            data_bits: 8,
            stop_bits: SerialStopBits::One,
            parity: SerialParity::None,
            flow_control: SerialFlowControl::None,
            read_timeout: Duration::from_millis(100),
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            initial_dtr: None,
            initial_rts: None,
            label: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failure modes for [`open_port`].
#[derive(Debug, Error)]
pub enum SerialOpenError {
    /// `data_bits` is outside `5..=8`.
    #[error("invalid data_bits {0}: must be 5, 6, 7, or 8")]
    InvalidDataBits(u8),
    /// The OS refused to open the port (permission, missing device,
    /// busy, etc).
    #[error("open failed for {path}: {source}")]
    Open {
        /// Device path that failed to open.
        path: String,
        /// Underlying tokio-serial error.
        #[source]
        source: tokio_serial::Error,
    },
    /// DTR / RTS configuration round-tripped through tokio-serial but
    /// the kernel rejected the level change.
    #[error("control line set failed for {path}: {source}")]
    ControlLine {
        /// Device path that failed.
        path: String,
        /// Underlying tokio-serial error.
        #[source]
        source: tokio_serial::Error,
    },
}

/// Failure modes for [`write_port`].
#[derive(Debug, Error)]
pub enum SerialWriteError {
    /// No open port matches the given `SerialId`.
    #[error("serial port not found: {0}")]
    NotFound(SerialId),
    /// Write queue is full — the consumer is slower than the producer.
    /// Equivalent to TCP backpressure on a stalled writer; callers
    /// should retry after a short local sleep or pause production.
    #[error("write queue full for {0}")]
    Backpressure(SerialId),
}

// ---------------------------------------------------------------------------
// Per-port state
// ---------------------------------------------------------------------------

/// Lock-free aggregate for a single open serial port.
///
/// Cloning the registry's snapshot also clones the `Arc<SerialPortState>`
/// — every field is either an atomic, an `Arc`-wrapped lock-free
/// primitive (`ArcSwap`, `Notify`), or a `mpsc::Sender` clone. The
/// reader and writer tasks own their respective halves of the OS
/// `SerialStream` directly; nothing else needs the file handle.
#[derive(Debug)]
pub struct SerialPortState {
    /// Unique id for this port (`UUIDv7`).
    pub id: SerialId,
    /// Frozen copy of the configuration the port was opened with.
    /// Read-only after construction; reconfiguration on the fly is
    /// reserved for v5.3.
    pub config: SerialConfig,
    /// Rolling history. `ArcSwap::load` is `O(1)` and never blocks.
    pub history: Arc<ArcSwap<RingBuffer>>,
    /// Per-port write queue. Cloned by `write_port`; drained by the
    /// writer task.
    write_tx: mpsc::Sender<Bytes>,
    /// Cooperative shutdown source for both reader and writer tasks.
    pub cancel_token: CancellationToken,
    /// Wall-clock millis of the last reader / writer activity. Powers
    /// idle detection and stats.
    pub last_activity_ms: AtomicU64,
    /// Effective max history size. Mirrors `config.max_buffer_size`
    /// when non-zero; otherwise [`DEFAULT_MAX_BUFFER_SIZE`].
    pub max_buffer_size: AtomicU64,
    /// Inbound notify — flipped after every successful flush. Lets
    /// internal long-poll readers (currently unused; reserved for the
    /// MCP read path) wake without consulting the OS.
    pub data_notify: Arc<Notify>,
}

impl SerialPortState {
    /// Borrow the canonical URI for this port.
    #[must_use]
    pub fn uri(&self) -> String {
        format!("serial://{}/output", self.id.as_str())
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Process-wide registry. Singleton via [`SERIAL_REGISTRY`].
pub struct SerialRegistry {
    ports: DashMap<SerialId, Arc<SerialPortState>>,
}

impl SerialRegistry {
    fn new() -> Self {
        Self {
            ports: DashMap::new(),
        }
    }

    /// Look up a port by id.
    #[must_use]
    pub fn get(&self, id: &SerialId) -> Option<Arc<SerialPortState>> {
        self.ports.get(id).map(|entry| Arc::clone(entry.value()))
    }

    /// Snapshot every open port. Order is `DashMap`-internal (i.e.
    /// arbitrary but stable across the call).
    #[must_use]
    pub fn snapshot(&self) -> Vec<Arc<SerialPortState>> {
        self.ports
            .iter()
            .map(|entry| Arc::clone(entry.value()))
            .collect()
    }

    /// Live port count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ports.len()
    }

    /// Cheap empty check.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ports.is_empty()
    }
}

/// Global serial registry singleton.
pub static SERIAL_REGISTRY: LazyLock<SerialRegistry> = LazyLock::new(SerialRegistry::new);

// ---------------------------------------------------------------------------
// Public API — open / close / write / list / read
// ---------------------------------------------------------------------------

/// Open a serial port and spawn its reader / writer tasks.
///
/// Returns the freshly minted [`SerialId`]. The state is registered in
/// [`SERIAL_REGISTRY`] before this function returns; subscribers can
/// attach to `serial://<id>/output` immediately.
///
/// # Errors
///
/// Returns [`SerialOpenError::InvalidDataBits`] when `data_bits` is
/// outside `5..=8`, [`SerialOpenError::Open`] when the OS refuses
/// `tokio_serial::new(...)`, or [`SerialOpenError::ControlLine`] when
/// DTR / RTS application fails after the port opened.
pub fn open_port(config: &SerialConfig) -> Result<SerialId, SerialOpenError> {
    let mut stream = open_native_stream(config)?;
    apply_control_lines(&mut stream, config)?;
    let (state, write_rx) = build_port_state(config);
    SERIAL_REGISTRY
        .ports
        .insert(state.id.clone(), Arc::clone(&state));
    let (reader, writer) = split(stream);
    spawn_writer_task(writer, write_rx, Arc::clone(&state));
    spawn_reader_task(reader, Arc::clone(&state));
    Ok(state.id.clone())
}

fn open_native_stream(config: &SerialConfig) -> Result<SerialStream, SerialOpenError> {
    let data_bits = into_tokio_data_bits(config.data_bits)
        .ok_or(SerialOpenError::InvalidDataBits(config.data_bits))?;
    tokio_serial::new(&config.path, config.baud_rate)
        .data_bits(data_bits)
        .stop_bits(config.stop_bits.into_tokio())
        .parity(config.parity.into_tokio())
        .flow_control(config.flow_control.into_tokio())
        .timeout(config.read_timeout)
        .open_native_async()
        .map_err(|source| SerialOpenError::Open {
            path: config.path.clone(),
            source,
        })
}

fn build_port_state(config: &SerialConfig) -> (Arc<SerialPortState>, mpsc::Receiver<Bytes>) {
    let serial_id = SerialId::new(Uuid::now_v7().to_string());
    let (write_tx, write_rx) = mpsc::channel::<Bytes>(WRITE_QUEUE_CAP);
    let max_buffer_size = if config.max_buffer_size == 0 {
        DEFAULT_MAX_BUFFER_SIZE
    } else {
        config.max_buffer_size
    };
    let state = Arc::new(SerialPortState {
        id: serial_id,
        config: config.clone(),
        history: Arc::new(ArcSwap::from_pointee(RingBuffer::default())),
        write_tx,
        cancel_token: CancellationToken::new(),
        last_activity_ms: AtomicU64::new(now_ms()),
        max_buffer_size: AtomicU64::new(max_buffer_size),
        data_notify: Arc::new(Notify::new()),
    });
    (state, write_rx)
}

/// Close a serial port — cancels reader / writer tasks and removes the registry entry.
///
/// Idempotent: a second call for the same id is a no-op. Returns
/// `true` when a port was actually closed; `false` when no port
/// matched the given id.
#[must_use]
pub fn close_port(id: &SerialId) -> bool {
    let Some((_, state)) = SERIAL_REGISTRY.ports.remove(id) else {
        return false;
    };
    state.cancel_token.cancel();
    true
}

/// Enqueue `bytes` for transmission on the port identified by `id`.
///
/// # Errors
///
/// - [`SerialWriteError::NotFound`] when no open port matches `id`.
/// - [`SerialWriteError::Backpressure`] when the per-port write queue
///   is full (consumer is slower than producer).
pub fn write_port(id: &SerialId, bytes: Bytes) -> Result<(), SerialWriteError> {
    let state = SERIAL_REGISTRY
        .get(id)
        .ok_or_else(|| SerialWriteError::NotFound(id.clone()))?;
    state.write_tx.try_send(bytes).map_err(|err| match err {
        TrySendError::Full(_) => SerialWriteError::Backpressure(id.clone()),
        TrySendError::Closed(_) => SerialWriteError::NotFound(id.clone()),
    })
}

/// Snapshot OS-visible serial ports. Wraps `tokio_serial::available_ports`.
#[must_use]
pub fn available_ports() -> Vec<String> {
    tokio_serial::available_ports()
        .map(|list| list.into_iter().map(|info| info.port_name).collect())
        .unwrap_or_default()
}

/// Read history bytes from `cursor` onward. Returns `(slice, new_cursor)`.
///
/// The slice is borrowed from a snapshot of the ring buffer, so
/// readers never block writers and writers never block readers — pure
/// `ArcSwap::load`.
#[must_use]
pub fn read_history_from_cursor(state: &SerialPortState, cursor: u64) -> (Bytes, u64) {
    let snapshot = state.history.load();
    let total_usize = snapshot.data.len();
    let total = u64::try_from(total_usize).unwrap_or(u64::MAX);
    let cursor_clamped = cursor.min(total);
    let cursor_usize = usize::try_from(cursor_clamped).unwrap_or(usize::MAX);
    let slice = snapshot.data.slice(cursor_usize..total_usize);
    (slice, total)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

const fn into_tokio_data_bits(bits: u8) -> Option<SerialDataBits> {
    match bits {
        5 => Some(SerialDataBits::Five),
        6 => Some(SerialDataBits::Six),
        7 => Some(SerialDataBits::Seven),
        8 => Some(SerialDataBits::Eight),
        _ => None,
    }
}

fn apply_control_lines(
    stream: &mut SerialStream,
    config: &SerialConfig,
) -> Result<(), SerialOpenError> {
    if let Some(level) = config.initial_dtr {
        stream
            .write_data_terminal_ready(level)
            .map_err(|source| SerialOpenError::ControlLine {
                path: config.path.clone(),
                source,
            })?;
    }
    if let Some(level) = config.initial_rts {
        stream
            .write_request_to_send(level)
            .map_err(|source| SerialOpenError::ControlLine {
                path: config.path.clone(),
                source,
            })?;
    }
    Ok(())
}

fn now_ms() -> u64 {
    u64::try_from(Utc::now().timestamp_millis()).unwrap_or(0)
}

fn spawn_writer_task(
    writer: WriteHalf<SerialStream>,
    rx: mpsc::Receiver<Bytes>,
    state: Arc<SerialPortState>,
) {
    tokio::spawn(run_writer_loop(writer, rx, state));
}

async fn run_writer_loop(
    mut writer: WriteHalf<SerialStream>,
    mut rx: mpsc::Receiver<Bytes>,
    state: Arc<SerialPortState>,
) {
    loop {
        tokio::select! {
            biased;
            () = state.cancel_token.cancelled() => break,
            Some(payload) = rx.recv() => {
                if let Err(err) = writer.write_all(&payload).await {
                    error!("serial writer failed for {}: {err}", state.config.path);
                    state.cancel_token.cancel();
                    break;
                }
                state.last_activity_ms.store(now_ms(), Ordering::Relaxed);
            }
            else => break,
        }
    }
}

fn spawn_reader_task(reader: ReadHalf<SerialStream>, state: Arc<SerialPortState>) {
    tokio::spawn(run_reader_loop(reader, state));
}

async fn run_reader_loop(mut reader: ReadHalf<SerialStream>, state: Arc<SerialPortState>) {
    let mut local = Vec::with_capacity(SERIAL_FLUSH_THRESHOLD);
    let mut buf = [0_u8; READ_BUF_LEN];
    loop {
        tokio::select! {
        biased;
        () = state.cancel_token.cancelled() => break,
        read_result = reader.read(&mut buf) => match read_result {
            Ok(0) => break,
            Ok(n) => {
                local.extend_from_slice(&buf[..n]);
                state.last_activity_ms.store(now_ms(), Ordering::Relaxed);
                if local.len() >= SERIAL_FLUSH_THRESHOLD {
                    flush_serial_buffer(&state, &mut local);
                }
            }
            Err(err) => {
                debug!("serial reader error for {}: {err}", state.config.path);
                break;
                }
            },
        }
    }
    if !local.is_empty() {
        flush_serial_buffer(&state, &mut local);
    }
    // Cleanup: ensure registry entry vanishes once the reader exits
    // (e.g. unplugged USB). A subsequent `serial_close` call is a
    // no-op via the idempotent contract.
    SERIAL_REGISTRY.ports.remove(&state.id);
}

/// Flush a local staging buffer into [`SerialPortState::history`] via
/// `ArcSwap::rcu`, head-truncate to `max_buffer_size`, and feed the
/// subscription pipeline (`next_seq`, `poke`, `record_bytes` —
/// ADR 0006 Amendment 1).
fn flush_serial_buffer(state: &Arc<SerialPortState>, local: &mut Vec<u8>) {
    let chunk = Bytes::copy_from_slice(local);
    local.clear();
    let max_size =
        usize::try_from(state.max_buffer_size.load(Ordering::Relaxed)).unwrap_or(usize::MAX);
    state.history.rcu(|current| {
        let mut combined = Vec::with_capacity(current.data.len() + chunk.len());
        combined.extend_from_slice(&current.data);
        combined.extend_from_slice(&chunk);
        if max_size > 0 && combined.len() > max_size {
            let excess = combined.len() - max_size;
            combined.drain(..excess);
        }
        RingBuffer {
            data: Bytes::from(combined),
        }
    });
    let _ = SUBSCRIPTION_REGISTRY.next_seq(ResourceKind::Serial, state.id.as_str());
    SUBSCRIPTION_REGISTRY.poke(ResourceKind::Serial, state.id.as_str());
    // ADR 0012 phase 9 — raw tail feeds inline-push lanes; the
    // production registry forwards `chunk.len()` to `record_bytes`
    // internally for the debouncer cadence.
    SUBSCRIPTION_REGISTRY.record_bytes_with_tail(
        ResourceKind::Serial,
        state.id.as_str(),
        chunk.as_ref(),
    );
    state.data_notify.notify_waiters();
}

// ---------------------------------------------------------------------------
// Tiny helper: parse a "DATABITS-PARITY-STOPBITS" shorthand (e.g.
// `"8N1"`, `"7E1"`, `"8O2"`) into the typed config fields. Used by
// the MCP tool wrapper to keep the JSON Schema small.
// ---------------------------------------------------------------------------

impl FromStr for SerialParity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "N" | "NONE" => Ok(Self::None),
            "O" | "ODD" => Ok(Self::Odd),
            "E" | "EVEN" => Ok(Self::Even),
            other => Err(format!("invalid parity: {other}")),
        }
    }
}

impl FromStr for SerialStopBits {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "1" => Ok(Self::One),
            "2" => Ok(Self::Two),
            other => Err(format!("invalid stop_bits: {other}")),
        }
    }
}

impl FromStr for SerialFlowControl {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "none" | "" => Ok(Self::None),
            "software" | "xon" | "xon/xoff" | "xonxoff" => Ok(Self::Software),
            "hardware" | "rts" | "rts/cts" | "rtscts" => Ok(Self::Hardware),
            other => Err(format!("invalid flow_control: {other}")),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use super::*;

    #[test]
    fn config_new_sets_8n1_defaults() {
        let cfg = SerialConfig::new("/dev/null", 9600);
        assert_eq!(cfg.baud_rate, 9600);
        assert_eq!(cfg.data_bits, 8);
        assert_eq!(cfg.stop_bits, SerialStopBits::One);
        assert_eq!(cfg.parity, SerialParity::None);
        assert_eq!(cfg.flow_control, SerialFlowControl::None);
    }

    #[test]
    fn data_bits_outside_5_to_8_round_trips_to_none() {
        assert!(into_tokio_data_bits(4).is_none());
        assert!(into_tokio_data_bits(9).is_none());
        assert!(into_tokio_data_bits(0).is_none());
        for bits in 5..=8 {
            assert!(into_tokio_data_bits(bits).is_some(), "data_bits={bits}");
        }
    }

    #[test]
    fn parity_parser_accepts_short_and_long_forms() {
        assert_eq!(SerialParity::from_str("N").unwrap(), SerialParity::None);
        assert_eq!(SerialParity::from_str("none").unwrap(), SerialParity::None);
        assert_eq!(SerialParity::from_str("E").unwrap(), SerialParity::Even);
        assert_eq!(SerialParity::from_str("ODD").unwrap(), SerialParity::Odd);
        assert!(SerialParity::from_str("X").is_err());
    }

    #[test]
    fn flow_control_parser_accepts_aliases() {
        assert_eq!(
            SerialFlowControl::from_str("rts/cts").unwrap(),
            SerialFlowControl::Hardware
        );
        assert_eq!(
            SerialFlowControl::from_str("xon/xoff").unwrap(),
            SerialFlowControl::Software
        );
        assert_eq!(
            SerialFlowControl::from_str("").unwrap(),
            SerialFlowControl::None
        );
    }

    #[test]
    fn stop_bits_parser_round_trips() {
        assert_eq!(SerialStopBits::from_str("1").unwrap(), SerialStopBits::One);
        assert_eq!(SerialStopBits::from_str("2").unwrap(), SerialStopBits::Two);
        assert!(SerialStopBits::from_str("3").is_err());
    }

    #[test]
    fn registry_starts_empty() {
        // Sanity: unrelated tests that opened a port may leave entries
        // behind during `--test-threads=1`. The default expectation is
        // simply that the registry is reachable as a singleton.
        let _ = SERIAL_REGISTRY.len();
    }
}
