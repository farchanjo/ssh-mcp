//! Semantic keystroke encoding for interactive PTY shells.
//!
//! Maps named keys (e.g. `ctrl_c`, `arrow_up`, `f5`) to xterm-compatible
//! byte sequences ready to be written to a russh PTY channel.
//!
//! # Encoding overview
//!
//! - Control characters use C0 codes `0x01..=0x1A`.
//! - `Backspace` is encoded as `0x7f` (modern xterm default — terminals
//!   configured to send `0x08` are not supported here; clients that need
//!   the legacy form should fall back to `ssh_shell_write` with raw
//!   bytes).
//! - Plain navigation/function keys default to xterm SS3 form when xterm
//!   does so (e.g. `Home` → `\x1bOH`). Modified variants always switch to
//!   the CSI form `\x1b[<param>;<mod>{letter}` per xterm conventions.
//! - The modifier code is `1 + (shift?1:0) + (alt?2:0) + (ctrl?4:0)`,
//!   yielding the range `2..=8` whenever at least one modifier is active.
//!
//! # Modifier policy
//!
//! Most keys reject modifiers entirely:
//! - All `Ctrl*` variants (the modifier is already baked in).
//! - `Enter`, `Escape`, `Backspace`, `Space`.
//!
//! `Tab` accepts only `Shift` (which encodes as `\x1b[Z`, the CBT/back-tab
//! sequence). Any other Tab modifier combination is rejected.
//!
//! Arrows, navigation keys (`Home`, `End`, `PageUp`, `PageDown`, `Insert`,
//! `Delete`), and `F1..=F12` accept any combination of Shift / Alt / Ctrl.

use std::borrow::Cow;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Named keystroke that can be sent to an interactive PTY shell.
///
/// Variants are serialized in `snake_case` for the MCP JSON schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellKey {
    /// Ctrl+A — beginning of line.
    CtrlA,
    /// Ctrl+C — interrupt (SIGINT).
    CtrlC,
    /// Ctrl+D — EOF / logout.
    CtrlD,
    /// Ctrl+E — end of line.
    CtrlE,
    /// Ctrl+K — kill to end of line.
    CtrlK,
    /// Ctrl+L — clear screen.
    CtrlL,
    /// Ctrl+R — reverse history search.
    CtrlR,
    /// Ctrl+U — kill to beginning of line.
    CtrlU,
    /// Ctrl+W — kill previous word.
    CtrlW,
    /// Ctrl+Z — suspend (SIGTSTP).
    CtrlZ,
    /// Enter / Return — submits the current input line.
    Enter,
    /// Tab — completion. With Shift modifier produces back-tab `\x1b[Z`.
    Tab,
    /// Escape — `\x1b`.
    Escape,
    /// Backspace — `\x7f` (modern xterm).
    Backspace,
    /// Space — `b" "`.
    Space,
    /// Delete (forward delete) — CSI `\x1b[3~`.
    Delete,
    /// Arrow up — CSI `\x1b[A`.
    ArrowUp,
    /// Arrow down — CSI `\x1b[B`.
    ArrowDown,
    /// Arrow left — CSI `\x1b[D`.
    ArrowLeft,
    /// Arrow right — CSI `\x1b[C`.
    ArrowRight,
    /// Home — SS3 `\x1bOH` plain, CSI `\x1b[1;<mod>H` with modifier.
    Home,
    /// End — SS3 `\x1bOF` plain, CSI `\x1b[1;<mod>F` with modifier.
    End,
    /// Page Up — CSI `\x1b[5~`.
    PageUp,
    /// Page Down — CSI `\x1b[6~`.
    PageDown,
    /// Insert — CSI `\x1b[2~`.
    Insert,
    /// F1 — SS3 `\x1bOP` plain, CSI `\x1b[1;<mod>P` with modifier.
    F1,
    /// F2 — SS3 `\x1bOQ` plain, CSI `\x1b[1;<mod>Q` with modifier.
    F2,
    /// F3 — SS3 `\x1bOR` plain, CSI `\x1b[1;<mod>R` with modifier.
    F3,
    /// F4 — SS3 `\x1bOS` plain, CSI `\x1b[1;<mod>S` with modifier.
    F4,
    /// F5 — CSI `\x1b[15~` plain, `\x1b[15;<mod>~` with modifier.
    F5,
    /// F6 — CSI `\x1b[17~`.
    F6,
    /// F7 — CSI `\x1b[18~`.
    F7,
    /// F8 — CSI `\x1b[19~`.
    F8,
    /// F9 — CSI `\x1b[20~`.
    F9,
    /// F10 — CSI `\x1b[21~`.
    F10,
    /// F11 — CSI `\x1b[23~`.
    F11,
    /// F12 — CSI `\x1b[24~`.
    F12,
}

/// Modifier flags applied on top of a [`ShellKey`].
///
/// All flags default to `false`. Use [`KeyModifiers::is_empty`] to detect
/// the no-modifier case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    /// Shift modifier.
    pub shift: bool,
    /// Alt / Meta modifier.
    pub alt: bool,
    /// Ctrl modifier.
    pub ctrl: bool,
}

impl KeyModifiers {
    /// `true` when no modifier is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.shift && !self.alt && !self.ctrl
    }

    /// xterm modifier code: `1 + shift + 2*alt + 4*ctrl`. Returns `1` when
    /// no modifier is active (which xterm omits in the encoded sequence).
    #[must_use]
    pub const fn xterm_code(self) -> u8 {
        let mut code: u8 = 1;
        if self.shift {
            code += 1;
        }
        if self.alt {
            code += 2;
        }
        if self.ctrl {
            code += 4;
        }
        code
    }
}

/// Errors returned by [`ShellKey::encode`].
#[derive(Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The key cannot accept the requested modifier combination.
    ///
    /// `key_label` carries the `snake_case` identifier (matches the serde
    /// rename, e.g. `"ctrl_c"`). `requested` carries the modifier set the
    /// caller asked for so the error message can echo it verbatim.
    ModifierNotAllowed {
        /// Stable key identifier in `snake_case` form.
        key_label: &'static str,
        /// Modifier combination that was rejected.
        requested: KeyModifiers,
    },
}

impl ShellKey {
    /// Stable label for error messages and logging.
    ///
    /// Matches the serde `rename_all = "snake_case"` form so the label
    /// round-trips with the JSON schema.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive match over all 37 variants is naturally long but trivially readable"
    )]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CtrlA => "ctrl_a",
            Self::CtrlC => "ctrl_c",
            Self::CtrlD => "ctrl_d",
            Self::CtrlE => "ctrl_e",
            Self::CtrlK => "ctrl_k",
            Self::CtrlL => "ctrl_l",
            Self::CtrlR => "ctrl_r",
            Self::CtrlU => "ctrl_u",
            Self::CtrlW => "ctrl_w",
            Self::CtrlZ => "ctrl_z",
            Self::Enter => "enter",
            Self::Tab => "tab",
            Self::Escape => "escape",
            Self::Backspace => "backspace",
            Self::Space => "space",
            Self::Delete => "delete",
            Self::ArrowUp => "arrow_up",
            Self::ArrowDown => "arrow_down",
            Self::ArrowLeft => "arrow_left",
            Self::ArrowRight => "arrow_right",
            Self::Home => "home",
            Self::End => "end",
            Self::PageUp => "page_up",
            Self::PageDown => "page_down",
            Self::Insert => "insert",
            Self::F1 => "f1",
            Self::F2 => "f2",
            Self::F3 => "f3",
            Self::F4 => "f4",
            Self::F5 => "f5",
            Self::F6 => "f6",
            Self::F7 => "f7",
            Self::F8 => "f8",
            Self::F9 => "f9",
            Self::F10 => "f10",
            Self::F11 => "f11",
            Self::F12 => "f12",
        }
    }

    /// Quick predicate: would the given modifier set be rejected?
    ///
    /// Mirrors the logic of [`Self::encode`] but without computing any
    /// byte sequence.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive match over all 37 variants is naturally long but trivially readable"
    )]
    pub const fn rejects_modifiers(self, mods: KeyModifiers) -> bool {
        if mods.is_empty() {
            return false;
        }
        match self {
            // Modifiers baked into the C0 code already.
            Self::CtrlA
            | Self::CtrlC
            | Self::CtrlD
            | Self::CtrlE
            | Self::CtrlK
            | Self::CtrlL
            | Self::CtrlR
            | Self::CtrlU
            | Self::CtrlW
            | Self::CtrlZ
            | Self::Enter
            | Self::Escape
            | Self::Backspace
            | Self::Space => true,
            // Tab accepts Shift only (back-tab); reject anything else.
            Self::Tab => mods.alt || mods.ctrl,
            // Arrows / nav / function keys accept any combination.
            Self::Delete
            | Self::ArrowUp
            | Self::ArrowDown
            | Self::ArrowLeft
            | Self::ArrowRight
            | Self::Home
            | Self::End
            | Self::PageUp
            | Self::PageDown
            | Self::Insert
            | Self::F1
            | Self::F2
            | Self::F3
            | Self::F4
            | Self::F5
            | Self::F6
            | Self::F7
            | Self::F8
            | Self::F9
            | Self::F10
            | Self::F11
            | Self::F12 => false,
        }
    }

    /// Encode the key with the requested modifiers as a byte sequence
    /// ready to write to a PTY channel.
    ///
    /// Plain (no-modifier) results are static slices returned as
    /// `Cow::Borrowed` to avoid allocation. Modified results require
    /// formatting the modifier digit and are returned as `Cow::Owned`.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::ModifierNotAllowed`] when the key rejects
    /// the requested modifier combination — see [`Self::rejects_modifiers`].
    pub fn encode(self, mods: KeyModifiers) -> Result<Cow<'static, [u8]>, EncodeError> {
        if self.rejects_modifiers(mods) {
            return Err(EncodeError::ModifierNotAllowed {
                key_label: self.label(),
                requested: mods,
            });
        }
        if mods.is_empty() {
            return Ok(Cow::Borrowed(encode_plain(self)));
        }
        // Tab + Shift is a single-shot CBT — no modifier digit involved.
        if matches!(self, Self::Tab) {
            return Ok(Cow::Borrowed(b"\x1b[Z"));
        }
        Ok(Cow::Owned(encode_modified(self, mods)))
    }
}

/// Plain (no-modifier) byte sequence for `key`.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive match over all 37 variants is naturally long but trivially readable"
)]
const fn encode_plain(key: ShellKey) -> &'static [u8] {
    match key {
        ShellKey::CtrlA => b"\x01",
        ShellKey::CtrlC => b"\x03",
        ShellKey::CtrlD => b"\x04",
        ShellKey::CtrlE => b"\x05",
        ShellKey::CtrlK => b"\x0b",
        ShellKey::CtrlL => b"\x0c",
        ShellKey::CtrlR => b"\x12",
        ShellKey::CtrlU => b"\x15",
        ShellKey::CtrlW => b"\x17",
        ShellKey::CtrlZ => b"\x1a",
        ShellKey::Enter => b"\r",
        ShellKey::Tab => b"\t",
        ShellKey::Escape => b"\x1b",
        ShellKey::Backspace => b"\x7f",
        ShellKey::Space => b" ",
        ShellKey::Delete => b"\x1b[3~",
        ShellKey::ArrowUp => b"\x1b[A",
        ShellKey::ArrowDown => b"\x1b[B",
        ShellKey::ArrowLeft => b"\x1b[D",
        ShellKey::ArrowRight => b"\x1b[C",
        ShellKey::Home => b"\x1bOH",
        ShellKey::End => b"\x1bOF",
        ShellKey::PageUp => b"\x1b[5~",
        ShellKey::PageDown => b"\x1b[6~",
        ShellKey::Insert => b"\x1b[2~",
        ShellKey::F1 => b"\x1bOP",
        ShellKey::F2 => b"\x1bOQ",
        ShellKey::F3 => b"\x1bOR",
        ShellKey::F4 => b"\x1bOS",
        ShellKey::F5 => b"\x1b[15~",
        ShellKey::F6 => b"\x1b[17~",
        ShellKey::F7 => b"\x1b[18~",
        ShellKey::F8 => b"\x1b[19~",
        ShellKey::F9 => b"\x1b[20~",
        ShellKey::F10 => b"\x1b[21~",
        ShellKey::F11 => b"\x1b[23~",
        ShellKey::F12 => b"\x1b[24~",
    }
}

/// Modified-form byte sequence (caller already verified the key accepts modifiers).
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive match over all 37 variants is naturally long but trivially readable"
)]
fn encode_modified(key: ShellKey, mods: KeyModifiers) -> Vec<u8> {
    let code = mods.xterm_code();
    match key {
        ShellKey::ArrowUp => encode_arrow_modified(b'A', code),
        ShellKey::ArrowDown => encode_arrow_modified(b'B', code),
        ShellKey::ArrowLeft => encode_arrow_modified(b'D', code),
        ShellKey::ArrowRight => encode_arrow_modified(b'C', code),
        ShellKey::Home => encode_arrow_modified(b'H', code),
        ShellKey::End => encode_arrow_modified(b'F', code),
        ShellKey::F1 => encode_arrow_modified(b'P', code),
        ShellKey::F2 => encode_arrow_modified(b'Q', code),
        ShellKey::F3 => encode_arrow_modified(b'R', code),
        ShellKey::F4 => encode_arrow_modified(b'S', code),
        ShellKey::Insert => encode_tilde_modified(2, code),
        ShellKey::Delete => encode_tilde_modified(3, code),
        ShellKey::PageUp => encode_tilde_modified(5, code),
        ShellKey::PageDown => encode_tilde_modified(6, code),
        ShellKey::F5 => encode_tilde_modified(15, code),
        ShellKey::F6 => encode_tilde_modified(17, code),
        ShellKey::F7 => encode_tilde_modified(18, code),
        ShellKey::F8 => encode_tilde_modified(19, code),
        ShellKey::F9 => encode_tilde_modified(20, code),
        ShellKey::F10 => encode_tilde_modified(21, code),
        ShellKey::F11 => encode_tilde_modified(23, code),
        ShellKey::F12 => encode_tilde_modified(24, code),
        // No-modifier variants and Tab are handled by the caller; reaching
        // any of these arms means encode() is being misused. Returning an
        // empty Vec is preferable to panicking in production code paths.
        ShellKey::CtrlA
        | ShellKey::CtrlC
        | ShellKey::CtrlD
        | ShellKey::CtrlE
        | ShellKey::CtrlK
        | ShellKey::CtrlL
        | ShellKey::CtrlR
        | ShellKey::CtrlU
        | ShellKey::CtrlW
        | ShellKey::CtrlZ
        | ShellKey::Enter
        | ShellKey::Tab
        | ShellKey::Escape
        | ShellKey::Backspace
        | ShellKey::Space => Vec::new(),
    }
}

/// CSI `\x1b[1;<code>{letter}` form used by arrows, Home/End, and F1-F4.
fn encode_arrow_modified(letter: u8, code: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(b"\x1b[1;");
    out.extend_from_slice(code.to_string().as_bytes());
    out.push(letter);
    out
}

/// CSI `\x1b[<param>;<code>~` form used by Insert/Delete/PgUp/PgDn/F5-F12.
fn encode_tilde_modified(param: u8, code: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    out.push(0x1b);
    out.push(b'[');
    out.extend_from_slice(param.to_string().as_bytes());
    out.push(b';');
    out.extend_from_slice(code.to_string().as_bytes());
    out.push(b'~');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_mods() -> KeyModifiers {
        KeyModifiers::default()
    }

    fn shift() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            alt: false,
            ctrl: false,
        }
    }

    fn alt() -> KeyModifiers {
        KeyModifiers {
            shift: false,
            alt: true,
            ctrl: false,
        }
    }

    fn ctrl() -> KeyModifiers {
        KeyModifiers {
            shift: false,
            alt: false,
            ctrl: true,
        }
    }

    fn shift_alt() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            alt: true,
            ctrl: false,
        }
    }

    fn shift_ctrl() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            alt: false,
            ctrl: true,
        }
    }

    fn alt_ctrl() -> KeyModifiers {
        KeyModifiers {
            shift: false,
            alt: true,
            ctrl: true,
        }
    }

    fn shift_alt_ctrl() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            alt: true,
            ctrl: true,
        }
    }

    fn encoded(key: ShellKey, mods: KeyModifiers) -> Vec<u8> {
        match key.encode(mods) {
            Ok(cow) => cow.into_owned(),
            Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    mod plain_encoding {
        use super::*;

        #[test]
        fn ctrl_a_through_z_have_correct_c0_codes() {
            assert_eq!(encoded(ShellKey::CtrlA, no_mods()), b"\x01");
            assert_eq!(encoded(ShellKey::CtrlC, no_mods()), b"\x03");
            assert_eq!(encoded(ShellKey::CtrlD, no_mods()), b"\x04");
            assert_eq!(encoded(ShellKey::CtrlE, no_mods()), b"\x05");
            assert_eq!(encoded(ShellKey::CtrlK, no_mods()), b"\x0b");
            assert_eq!(encoded(ShellKey::CtrlL, no_mods()), b"\x0c");
            assert_eq!(encoded(ShellKey::CtrlR, no_mods()), b"\x12");
            assert_eq!(encoded(ShellKey::CtrlU, no_mods()), b"\x15");
            assert_eq!(encoded(ShellKey::CtrlW, no_mods()), b"\x17");
            assert_eq!(encoded(ShellKey::CtrlZ, no_mods()), b"\x1a");
        }

        #[test]
        fn special_keys_plain() {
            assert_eq!(encoded(ShellKey::Enter, no_mods()), b"\r");
            assert_eq!(encoded(ShellKey::Tab, no_mods()), b"\t");
            assert_eq!(encoded(ShellKey::Escape, no_mods()), b"\x1b");
            assert_eq!(encoded(ShellKey::Backspace, no_mods()), b"\x7f");
            assert_eq!(encoded(ShellKey::Space, no_mods()), b" ");
            assert_eq!(encoded(ShellKey::Delete, no_mods()), b"\x1b[3~");
        }

        #[test]
        fn arrows_plain() {
            assert_eq!(encoded(ShellKey::ArrowUp, no_mods()), b"\x1b[A");
            assert_eq!(encoded(ShellKey::ArrowDown, no_mods()), b"\x1b[B");
            assert_eq!(encoded(ShellKey::ArrowLeft, no_mods()), b"\x1b[D");
            assert_eq!(encoded(ShellKey::ArrowRight, no_mods()), b"\x1b[C");
        }

        #[test]
        fn navigation_plain() {
            assert_eq!(encoded(ShellKey::Home, no_mods()), b"\x1bOH");
            assert_eq!(encoded(ShellKey::End, no_mods()), b"\x1bOF");
            assert_eq!(encoded(ShellKey::PageUp, no_mods()), b"\x1b[5~");
            assert_eq!(encoded(ShellKey::PageDown, no_mods()), b"\x1b[6~");
            assert_eq!(encoded(ShellKey::Insert, no_mods()), b"\x1b[2~");
        }

        #[test]
        fn function_keys_f1_f4_plain_ss3() {
            assert_eq!(encoded(ShellKey::F1, no_mods()), b"\x1bOP");
            assert_eq!(encoded(ShellKey::F2, no_mods()), b"\x1bOQ");
            assert_eq!(encoded(ShellKey::F3, no_mods()), b"\x1bOR");
            assert_eq!(encoded(ShellKey::F4, no_mods()), b"\x1bOS");
        }

        #[test]
        fn function_keys_f5_f12_plain_csi() {
            assert_eq!(encoded(ShellKey::F5, no_mods()), b"\x1b[15~");
            assert_eq!(encoded(ShellKey::F6, no_mods()), b"\x1b[17~");
            assert_eq!(encoded(ShellKey::F7, no_mods()), b"\x1b[18~");
            assert_eq!(encoded(ShellKey::F8, no_mods()), b"\x1b[19~");
            assert_eq!(encoded(ShellKey::F9, no_mods()), b"\x1b[20~");
            assert_eq!(encoded(ShellKey::F10, no_mods()), b"\x1b[21~");
            assert_eq!(encoded(ShellKey::F11, no_mods()), b"\x1b[23~");
            assert_eq!(encoded(ShellKey::F12, no_mods()), b"\x1b[24~");
        }

        #[test]
        fn plain_returns_borrowed_static() {
            match ShellKey::CtrlC.encode(no_mods()) {
                Ok(Cow::Borrowed(b)) => assert_eq!(b, b"\x03"),
                Ok(Cow::Owned(_)) => panic!("expected Cow::Borrowed for plain encoding"),
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
    }

    mod modifier_codes {
        use super::*;

        #[test]
        fn xterm_code_no_mod_is_one() {
            assert_eq!(no_mods().xterm_code(), 1);
        }

        #[test]
        fn xterm_code_shift_is_two() {
            assert_eq!(shift().xterm_code(), 2);
        }

        #[test]
        fn xterm_code_alt_is_three() {
            assert_eq!(alt().xterm_code(), 3);
        }

        #[test]
        fn xterm_code_shift_alt_is_four() {
            assert_eq!(shift_alt().xterm_code(), 4);
        }

        #[test]
        fn xterm_code_ctrl_is_five() {
            assert_eq!(ctrl().xterm_code(), 5);
        }

        #[test]
        fn xterm_code_shift_ctrl_is_six() {
            assert_eq!(shift_ctrl().xterm_code(), 6);
        }

        #[test]
        fn xterm_code_alt_ctrl_is_seven() {
            assert_eq!(alt_ctrl().xterm_code(), 7);
        }

        #[test]
        fn xterm_code_shift_alt_ctrl_is_eight() {
            assert_eq!(shift_alt_ctrl().xterm_code(), 8);
        }

        #[test]
        fn key_modifiers_default_is_empty() {
            assert!(KeyModifiers::default().is_empty());
        }
    }

    mod arrow_modifiers {
        use super::*;

        #[test]
        fn arrow_up_with_shift() {
            assert_eq!(encoded(ShellKey::ArrowUp, shift()), b"\x1b[1;2A");
        }

        #[test]
        fn arrow_down_with_alt() {
            assert_eq!(encoded(ShellKey::ArrowDown, alt()), b"\x1b[1;3B");
        }

        #[test]
        fn arrow_left_with_ctrl() {
            assert_eq!(encoded(ShellKey::ArrowLeft, ctrl()), b"\x1b[1;5D");
        }

        #[test]
        fn arrow_right_with_shift_ctrl() {
            assert_eq!(encoded(ShellKey::ArrowRight, shift_ctrl()), b"\x1b[1;6C");
        }

        #[test]
        fn arrow_up_with_shift_alt_ctrl() {
            assert_eq!(encoded(ShellKey::ArrowUp, shift_alt_ctrl()), b"\x1b[1;8A");
        }

        #[test]
        fn arrow_modified_returns_owned() {
            match ShellKey::ArrowUp.encode(shift()) {
                Ok(Cow::Owned(v)) => assert_eq!(v, b"\x1b[1;2A"),
                Ok(Cow::Borrowed(_)) => panic!("expected Cow::Owned for modified encoding"),
                Err(e) => panic!("unexpected error {e:?}"),
            }
        }
    }

    mod nav_modifiers {
        use super::*;

        #[test]
        fn home_with_shift() {
            assert_eq!(encoded(ShellKey::Home, shift()), b"\x1b[1;2H");
        }

        #[test]
        fn end_with_ctrl() {
            assert_eq!(encoded(ShellKey::End, ctrl()), b"\x1b[1;5F");
        }

        #[test]
        fn page_up_with_shift_alt() {
            assert_eq!(encoded(ShellKey::PageUp, shift_alt()), b"\x1b[5;4~");
        }

        #[test]
        fn page_down_with_alt_ctrl() {
            assert_eq!(encoded(ShellKey::PageDown, alt_ctrl()), b"\x1b[6;7~");
        }

        #[test]
        fn insert_with_shift_ctrl() {
            assert_eq!(encoded(ShellKey::Insert, shift_ctrl()), b"\x1b[2;6~");
        }

        #[test]
        fn delete_with_shift() {
            assert_eq!(encoded(ShellKey::Delete, shift()), b"\x1b[3;2~");
        }

        #[test]
        fn delete_with_shift_alt_ctrl() {
            assert_eq!(encoded(ShellKey::Delete, shift_alt_ctrl()), b"\x1b[3;8~");
        }
    }

    mod function_modifiers {
        use super::*;

        #[test]
        fn f1_with_shift_uses_csi() {
            assert_eq!(encoded(ShellKey::F1, shift()), b"\x1b[1;2P");
        }

        #[test]
        fn f4_with_ctrl_uses_csi() {
            assert_eq!(encoded(ShellKey::F4, ctrl()), b"\x1b[1;5S");
        }

        #[test]
        fn f5_with_alt() {
            assert_eq!(encoded(ShellKey::F5, alt()), b"\x1b[15;3~");
        }

        #[test]
        fn f9_with_shift_ctrl() {
            assert_eq!(encoded(ShellKey::F9, shift_ctrl()), b"\x1b[20;6~");
        }

        #[test]
        fn f12_with_shift_alt_ctrl() {
            assert_eq!(encoded(ShellKey::F12, shift_alt_ctrl()), b"\x1b[24;8~");
        }
    }

    mod tab_modifiers {
        use super::*;

        #[test]
        fn tab_with_shift_is_back_tab() {
            assert_eq!(encoded(ShellKey::Tab, shift()), b"\x1b[Z");
        }

        #[test]
        fn tab_with_alt_rejected() {
            let err = ShellKey::Tab.encode(alt()).unwrap_err();
            match err {
                EncodeError::ModifierNotAllowed { key_label, .. } => {
                    assert_eq!(key_label, "tab");
                }
            }
        }

        #[test]
        fn tab_with_ctrl_rejected() {
            assert!(ShellKey::Tab.encode(ctrl()).is_err());
        }

        #[test]
        fn tab_with_shift_alt_rejected() {
            assert!(ShellKey::Tab.encode(shift_alt()).is_err());
        }
    }

    mod modifier_rejection {
        use super::*;

        #[test]
        fn ctrl_a_rejects_any_modifier() {
            assert!(ShellKey::CtrlA.encode(shift()).is_err());
            assert!(ShellKey::CtrlA.encode(alt()).is_err());
            assert!(ShellKey::CtrlA.encode(ctrl()).is_err());
        }

        #[test]
        fn ctrl_c_rejects_shift() {
            let err = ShellKey::CtrlC.encode(shift()).unwrap_err();
            match err {
                EncodeError::ModifierNotAllowed {
                    key_label,
                    requested,
                } => {
                    assert_eq!(key_label, "ctrl_c");
                    assert!(requested.shift);
                }
            }
        }

        #[test]
        fn enter_rejects_modifiers() {
            assert!(ShellKey::Enter.encode(shift()).is_err());
            assert!(ShellKey::Enter.encode(ctrl()).is_err());
        }

        #[test]
        fn escape_rejects_modifiers() {
            assert!(ShellKey::Escape.encode(alt()).is_err());
        }

        #[test]
        fn backspace_rejects_modifiers() {
            assert!(ShellKey::Backspace.encode(ctrl()).is_err());
        }

        #[test]
        fn space_rejects_modifiers() {
            assert!(ShellKey::Space.encode(shift()).is_err());
        }

        #[test]
        fn rejects_modifiers_predicate_matches_encode() {
            // Matrix sweep — rejects_modifiers must agree with encode.
            let mods = shift_alt_ctrl();
            for key in [
                ShellKey::CtrlA,
                ShellKey::Enter,
                ShellKey::Escape,
                ShellKey::Backspace,
                ShellKey::Space,
            ] {
                assert!(key.rejects_modifiers(mods));
                assert!(key.encode(mods).is_err());
            }
            for key in [
                ShellKey::ArrowUp,
                ShellKey::Home,
                ShellKey::PageUp,
                ShellKey::Insert,
                ShellKey::Delete,
                ShellKey::F1,
                ShellKey::F12,
            ] {
                assert!(!key.rejects_modifiers(mods));
                assert!(key.encode(mods).is_ok());
            }
        }
    }

    mod serde_round_trip {
        use super::*;

        #[test]
        fn snake_case_deserialize_ctrl_c() {
            let raw = serde_json::json!("ctrl_c");
            let key: ShellKey = serde_json::from_value(raw).unwrap();
            assert_eq!(key, ShellKey::CtrlC);
        }

        #[test]
        fn snake_case_deserialize_arrow_up() {
            let raw = serde_json::json!("arrow_up");
            let key: ShellKey = serde_json::from_value(raw).unwrap();
            assert_eq!(key, ShellKey::ArrowUp);
        }

        #[test]
        fn snake_case_deserialize_page_down() {
            let raw = serde_json::json!("page_down");
            let key: ShellKey = serde_json::from_value(raw).unwrap();
            assert_eq!(key, ShellKey::PageDown);
        }

        #[test]
        fn snake_case_deserialize_f12() {
            let raw = serde_json::json!("f12");
            let key: ShellKey = serde_json::from_value(raw).unwrap();
            assert_eq!(key, ShellKey::F12);
        }

        #[test]
        fn snake_case_serialize_ctrl_c() {
            let json = serde_json::to_value(ShellKey::CtrlC).unwrap();
            assert_eq!(json, serde_json::json!("ctrl_c"));
        }

        #[test]
        fn snake_case_serialize_arrow_left() {
            let json = serde_json::to_value(ShellKey::ArrowLeft).unwrap();
            assert_eq!(json, serde_json::json!("arrow_left"));
        }

        #[test]
        fn invalid_key_rejected() {
            let raw = serde_json::json!("ctrl_x");
            assert!(serde_json::from_value::<ShellKey>(raw).is_err());
        }
    }

    mod label_helper {
        use super::*;

        #[test]
        fn ctrl_labels() {
            assert_eq!(ShellKey::CtrlA.label(), "ctrl_a");
            assert_eq!(ShellKey::CtrlC.label(), "ctrl_c");
            assert_eq!(ShellKey::CtrlZ.label(), "ctrl_z");
        }

        #[test]
        fn special_labels() {
            assert_eq!(ShellKey::Enter.label(), "enter");
            assert_eq!(ShellKey::Tab.label(), "tab");
            assert_eq!(ShellKey::Escape.label(), "escape");
            assert_eq!(ShellKey::Backspace.label(), "backspace");
            assert_eq!(ShellKey::Space.label(), "space");
            assert_eq!(ShellKey::Delete.label(), "delete");
        }

        #[test]
        fn arrow_labels() {
            assert_eq!(ShellKey::ArrowUp.label(), "arrow_up");
            assert_eq!(ShellKey::ArrowDown.label(), "arrow_down");
            assert_eq!(ShellKey::ArrowLeft.label(), "arrow_left");
            assert_eq!(ShellKey::ArrowRight.label(), "arrow_right");
        }

        #[test]
        fn nav_labels() {
            assert_eq!(ShellKey::Home.label(), "home");
            assert_eq!(ShellKey::End.label(), "end");
            assert_eq!(ShellKey::PageUp.label(), "page_up");
            assert_eq!(ShellKey::PageDown.label(), "page_down");
            assert_eq!(ShellKey::Insert.label(), "insert");
        }

        #[test]
        fn function_labels() {
            assert_eq!(ShellKey::F1.label(), "f1");
            assert_eq!(ShellKey::F5.label(), "f5");
            assert_eq!(ShellKey::F12.label(), "f12");
        }
    }
}
