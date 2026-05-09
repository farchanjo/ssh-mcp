//! MCP tool argument structs for the serial transport (v5.2; ADR 0009).
//!
//! Every serial tool accepts the same `8N1` defaults SSH-shell does — caller
//! supplies only `path` + optional `baud_rate` for the typical flow.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `serial_open` arguments. The full-fat parameter list is exposed
/// so embedded / industrial workflows that talk RS-485 / RS-422 / 7E1
/// can configure end-to-end without shelling out to `stty`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SerialOpenArgs {
    /// OS device path. Linux: `/dev/ttyUSB0` / `/dev/ttyACM0`.
    /// macOS: `/dev/tty.usbserial-XXXX`. Windows: `COM3`.
    pub path: String,
    /// Baud rate (default `115_200`). Common values: 9600, 19200,
    /// 38400, 57600, 115200, 230400, 460800, 921600.
    #[serde(default = "default_baud")]
    pub baud_rate: u32,
    /// Data bits (`5`, `6`, `7`, `8`). Default `8`.
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    /// Stop bits — accepts `"1"` or `"2"`. Default `"1"`.
    #[serde(default = "default_stop_bits")]
    pub stop_bits: String,
    /// Parity — accepts `"none"`/`"N"`, `"odd"`/`"O"`, `"even"`/`"E"`. Default `"none"`.
    #[serde(default = "default_parity")]
    pub parity: String,
    /// Flow control — accepts `"none"`, `"software"`/`"xon/xoff"`,
    /// `"hardware"`/`"rts/cts"`. Default `"none"`.
    #[serde(default = "default_flow_control")]
    pub flow_control: String,
    /// Per-read timeout in milliseconds. Default `100`.
    #[serde(default = "default_read_timeout_ms")]
    pub read_timeout_ms: u64,
    /// Maximum history buffer (bytes). `0` falls back to the
    /// adapter default (1 MiB). Default `0`.
    #[serde(default)]
    pub max_buffer_size: u64,
    /// Initial DTR (Data Terminal Ready) line. `None` leaves the
    /// driver default; `Some(true)` raises, `Some(false)` lowers.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: null (driver default).
    #[serde(default)]
    pub initial_dtr: Option<bool>,
    /// Initial RTS (Request To Send) line. Same semantics as
    /// `initial_dtr`.
    ///
    /// Type: boolean (JSON `true` or `false` — NOT the strings `"true"`/`"false"`). Default: null (driver default).
    #[serde(default)]
    pub initial_rts: Option<bool>,
    /// Optional human label (e.g. `"GPS-1"`) surfaced on
    /// `serial_active`.
    #[serde(default)]
    pub label: Option<String>,
}

const fn default_baud() -> u32 {
    115_200
}

const fn default_data_bits() -> u8 {
    8
}

fn default_stop_bits() -> String {
    "1".to_string()
}

fn default_parity() -> String {
    "none".to_string()
}

fn default_flow_control() -> String {
    "none".to_string()
}

const fn default_read_timeout_ms() -> u64 {
    100
}

/// `serial_close` arguments.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SerialCloseArgs {
    /// `SERIAL_ID` returned by `serial_open`.
    pub serial_id: String,
}

/// `serial_write` arguments. The payload accepts plain UTF-8
/// text (`text` field) OR base64-encoded raw bytes (`bytes_base64`)
/// for binary protocols. Exactly one must be supplied.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SerialWriteArgs {
    /// `SERIAL_ID` returned by `serial_open`.
    pub serial_id: String,
    /// UTF-8 text payload. Newlines are NOT auto-appended; pass `"foo\n"`
    /// or use `serial_press` for `enter` / `cr` / `lf`.
    #[serde(default)]
    pub text: Option<String>,
    /// Base64-encoded raw bytes (binary protocols, packed frames).
    #[serde(default)]
    pub bytes_base64: Option<String>,
}

/// `serial_press` arguments. Mirrors the shell `send_key`
/// surface for serial-friendly named keystrokes.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SerialPressArgs {
    /// `SERIAL_ID` returned by `serial_open`.
    pub serial_id: String,
    /// Key name. Accepted: `"enter"` / `"cr"` (`\r`), `"lf"` (`\n`),
    /// `"crlf"` (`\r\n`), `"esc"` (`\x1b`), `"tab"` (`\t`),
    /// `"backspace"` (`\x08`), `"ctrl_c"` (`\x03`), `"ctrl_d"` (`\x04`),
    /// `"ctrl_z"` (`\x1a`).
    pub key: String,
    /// Repeat count. `1..=64`. Default `1`.
    #[serde(default = "default_repeat")]
    pub repeat: u32,
}

const fn default_repeat() -> u32 {
    1
}

/// `serial_scan` takes no arguments.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::empty_structs_with_brackets,
    reason = "schemars derives type:null for unit structs (`pub struct X;`); MCP spec mandates inputSchema.type==object"
)]
pub struct SerialListPortsArgs {}

/// `serial_active` takes no arguments.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::empty_structs_with_brackets,
    reason = "schemars derives type:null for unit structs (`pub struct X;`); MCP spec mandates inputSchema.type==object"
)]
pub struct SerialListOpenArgs {}
