"""In-process paramiko SSH server fixture for ssh-mcp v4.7 tests.

The chaos / integration suite needs a deterministic SSH endpoint that
runs in-process so tests do not depend on a real sshd or the user's SSH
config. This module spins up a `paramiko.Transport` against a locally
bound `127.0.0.1:<random_port>` socket and accepts:

- Password auth (`testuser` / `testpass` by default).
- Key auth (passwordless ed25519 keypair generated per fixture).
- Agent auth refused; tests can fall back to password if needed.

Each accepted channel handles a tiny sh-style request:

- `exec_request` — runs the command in a subprocess (`/bin/sh -c <cmd>`).
- `pty_request` + `shell_request` — opens an interactive `/bin/sh -i`.
- `subsystem_request("sftp")` — paramiko's built-in SFTP server.

Tests treat the fixture as a local sshd: pass the address back as
`SSH_MCP_TEST_TARGET=127.0.0.1:<port>` and the user as `testuser`.

Usage::

    with LocalSshdFixture() as fx:
        target_address = fx.address  # "127.0.0.1:<port>"
        username = fx.username
        password = fx.password
        key_path = fx.private_key_path  # optional ed25519 private key

The fixture is cooperative — every accepted Transport thread shuts down
when the listener socket is closed and the active channel sees EOF.
"""

from __future__ import annotations

import os
import select
import socket
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import closing
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator

import paramiko
from paramiko.sftp_handle import SFTPHandle
from paramiko.sftp_si import SFTPServerInterface
from struct import error as struct_error


def _free_port() -> int:
    with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def _generate_ed25519_keypair(target_dir: Path) -> tuple[Path, Path]:
    """Generate a passwordless ed25519 keypair on disk.

    Paramiko exposes `Ed25519Key.generate()` only in recent versions; rely
    on the `cryptography` backend to produce both halves and persist them
    in OpenSSH PEM format so paramiko reads them back via `from_private_key_file`.
    """
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    private = Ed25519PrivateKey.generate()
    target_dir.mkdir(parents=True, exist_ok=True)
    priv_path = target_dir / "id_ed25519"
    pub_path = target_dir / "id_ed25519.pub"

    priv_bytes = private.private_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PrivateFormat.OpenSSH,
        encryption_algorithm=serialization.NoEncryption(),
    )
    priv_path.write_bytes(priv_bytes)
    priv_path.chmod(0o600)

    public = private.public_key()
    pub_bytes = public.public_bytes(
        encoding=serialization.Encoding.OpenSSH,
        format=serialization.PublicFormat.OpenSSH,
    )
    pub_path.write_bytes(pub_bytes)
    return priv_path, pub_path


def _generate_host_key(target_dir: Path) -> Path:
    """Generate an RSA 2048 host key (ed25519 paramiko host keys are
    available but the helper kept it simple — RSA is universally accepted)."""
    target_dir.mkdir(parents=True, exist_ok=True)
    host_key_path = target_dir / "ssh_host_rsa_key"
    key = paramiko.RSAKey.generate(2048)
    key.write_private_key_file(str(host_key_path))
    return host_key_path


# ---------------------------------------------------------------------------
# SFTP server interface — paramiko ships a built-in but expects the file
# operations to be plumbed through. Defer to the local filesystem so we can
# upload to /tmp/<port>/...
# ---------------------------------------------------------------------------


class _LocalSftpHandle(SFTPHandle):
    pass


class _LocalSftpServer(SFTPServerInterface):
    """Filesystem-backed SFTP. Pass ``rootdir`` if you want a chroot;
    default is the system root so tests can SFTP `/tmp/<x>` directly.
    """

    ROOT = "/"

    def _resolve(self, path: str) -> str:
        # Trust the path as-is — tests target /tmp/<port>/... which is safe
        # in the local sandbox.
        return path

    def list_folder(self, path: str):  # pragma: no cover - exercised by paramiko
        try:
            entries = os.listdir(self._resolve(path))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno)
        out = []
        for name in entries:
            full = os.path.join(path, name)
            try:
                attr = paramiko.SFTPAttributes.from_stat(os.stat(full))
                attr.filename = name
                out.append(attr)
            except OSError:
                continue
        return out

    def stat(self, path: str):
        try:
            return paramiko.SFTPAttributes.from_stat(os.stat(self._resolve(path)))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno)

    def lstat(self, path: str):
        try:
            return paramiko.SFTPAttributes.from_stat(os.lstat(self._resolve(path)))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno)

    def open(self, path: str, flags: int, attr):
        try:
            fd = os.open(self._resolve(path), flags, 0o644)
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno)
        try:
            mode = "rb"
            if flags & os.O_WRONLY:
                mode = "wb"
            elif flags & os.O_RDWR:
                mode = "rb+"
            if flags & os.O_APPEND:
                mode = mode.replace("b", "ab")
            f = os.fdopen(fd, mode)
        except OSError as exc:
            try:
                os.close(fd)
            except OSError:
                pass
            return paramiko.SFTPServer.convert_errno(exc.errno)
        handle = _LocalSftpHandle(flags)
        handle.filename = path
        if "r" in mode:
            handle.readfile = f
        if "w" in mode or "a" in mode or "+" in mode:
            handle.writefile = f
        return handle

    def remove(self, path: str):
        try:
            os.remove(self._resolve(path))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno)
        return paramiko.SFTP_OK

    def rename(self, oldpath: str, newpath: str):
        try:
            os.rename(self._resolve(oldpath), self._resolve(newpath))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno)
        return paramiko.SFTP_OK

    def mkdir(self, path: str, attr):
        try:
            os.mkdir(self._resolve(path))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno)
        return paramiko.SFTP_OK

    def rmdir(self, path: str):
        try:
            os.rmdir(self._resolve(path))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno)
        return paramiko.SFTP_OK


# ---------------------------------------------------------------------------
# ServerInterface — wires auth + channel dispatch.
# ---------------------------------------------------------------------------


class _ChannelServer(paramiko.ServerInterface):
    """Accepts password + publickey auth, allows session channels and
    common requests (exec, shell, pty, subsystem=sftp).
    """

    def __init__(
        self,
        *,
        username: str,
        password: str,
        public_key_blob: bytes,
        banner: bytes | None = None,
    ) -> None:
        super().__init__()
        self._username = username
        self._password = password
        self._public_key_blob = public_key_blob
        self._banner = banner

    def get_banner(self):
        if self._banner:
            return self._banner.decode("utf-8", errors="ignore"), "en-US"
        return None, None

    def check_auth_password(self, username: str, password: str):
        if username == self._username and password == self._password:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_publickey(self, username: str, key):
        if username != self._username:
            return paramiko.AUTH_FAILED
        # Compare the public-key bytes
        offered = key.asbytes()
        if offered == self._public_key_blob:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def get_allowed_auths(self, username: str):
        return "password,publickey"

    def check_channel_request(self, kind: str, chanid: int):
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    @staticmethod
    def _ensure_event(channel):
        ev = getattr(channel, "_request_event", None)
        if ev is None:
            ev = threading.Event()
            channel._request_event = ev
        return ev

    def check_channel_pty_request(
        self, channel, term, width, height, pixelwidth, pixelheight, modes
    ):
        return True

    def check_channel_shell_request(self, channel):
        # Accept shell. The actual /bin/sh -i drive runs once the request
        # completes; see the channel handler thread below.
        self._ensure_event(channel).set()
        return True

    def check_channel_exec_request(self, channel, command):
        # Accept and stash the command for the thread to consume.
        channel.exec_command_payload = command
        self._ensure_event(channel).set()
        return True

    def check_channel_subsystem_request(self, channel, name):
        if name != "sftp":
            return False
        # Mark the channel so the outer ``_drive_channel`` thread does not
        # also try to drive a shell on it. Then defer to paramiko's default
        # implementation which actually instantiates and starts the
        # subsystem handler registered via ``transport.set_subsystem_handler``.
        channel._is_sftp_subsystem = True
        self._ensure_event(channel).set()
        return super().check_channel_subsystem_request(channel, name)


# ---------------------------------------------------------------------------
# Channel runners — bind incoming requests to subprocesses.
# ---------------------------------------------------------------------------


def _drive_exec_channel(channel, command: bytes) -> None:
    cmd = command.decode("utf-8", errors="ignore")
    proc = subprocess.Popen(
        cmd,
        shell=True,
        executable="/bin/sh",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        stdin=subprocess.PIPE,
    )
    try:
        out, err = proc.communicate(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        out, err = proc.communicate()
    try:
        if out:
            channel.sendall(out)
        if err:
            channel.sendall_stderr(err)
        # Map signal exit codes (negative on POSIX) into the conventional
        # 128 + signal value so paramiko's struct.pack(">I", ...) does not
        # blow up on negative ints. Also clamp to u32 range as a defensive
        # guard for hypothetically large positive returncodes.
        rc = proc.returncode
        if rc is None:
            wire_rc = 1
        elif rc < 0:
            wire_rc = 128 + (-rc)
        else:
            wire_rc = rc & 0xFFFF_FFFF
        channel.send_exit_status(wire_rc)
    except (OSError, EOFError, paramiko.SSHException, struct_error):
        pass
    finally:
        try:
            channel.shutdown_write()
        except (OSError, paramiko.SSHException):
            pass
        try:
            channel.close()
        except (OSError, paramiko.SSHException):
            pass


def _drive_shell_channel(channel, banner_after_open: bytes | None) -> None:
    """Spawn ``/bin/sh -i`` over a real PTY and pump bytes between
    channel and process.

    The PTY layer is what makes signal-bearing keystrokes (Ctrl+C, Ctrl+Z)
    propagate as SIGINT/SIGTSTP — without it `\\x03` bytes are interpreted
    as literal stdin, which breaks the v4.7 ``ssh_shell_send_key``
    contract end-to-end. Falls back to plain pipes on platforms without
    ``pty`` support.
    """
    try:
        import pty
        master_fd, slave_fd = pty.openpty()
    except (ImportError, OSError):
        master_fd = None
        slave_fd = None

    if master_fd is not None and slave_fd is not None:
        try:
            proc = subprocess.Popen(
                ["/bin/sh", "-i"],
                stdout=slave_fd,
                stderr=slave_fd,
                stdin=slave_fd,
                start_new_session=True,
                env={**os.environ, "PS1": "$ ", "TERM": "xterm-256color"},
            )
        except OSError:
            os.close(master_fd)
            os.close(slave_fd)
            master_fd = None
            slave_fd = None
        else:
            os.close(slave_fd)
            slave_fd = None

    if master_fd is None:
        # Fallback: plain pipes (no PTY semantics).
        proc = subprocess.Popen(
            ["/bin/sh", "-i"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            stdin=subprocess.PIPE,
            bufsize=0,
            env={**os.environ, "PS1": "$ "},
        )

    if banner_after_open:
        try:
            channel.sendall(banner_after_open)
        except (OSError, paramiko.SSHException):
            pass

    def _pty_to_channel():
        assert master_fd is not None
        try:
            while True:
                try:
                    ready, _, _ = select.select([master_fd], [], [], 0.5)
                except (OSError, ValueError):
                    return
                if not ready:
                    if proc.poll() is not None:
                        return
                    continue
                try:
                    buf = os.read(master_fd, 4096)
                except OSError:
                    return
                if not buf:
                    return
                try:
                    channel.sendall(buf)
                except (OSError, paramiko.SSHException):
                    return
        except (OSError, ValueError):
            return

    def _channel_to_pty():
        assert master_fd is not None
        while True:
            try:
                ready, _, _ = select.select([channel], [], [], 0.5)
            except (OSError, ValueError):
                return
            if not ready:
                if proc.poll() is not None:
                    return
                continue
            try:
                data = channel.recv(4096)
            except (OSError, paramiko.SSHException):
                return
            if not data:
                return
            try:
                os.write(master_fd, data)
            except OSError:
                return

    def _pipe_proc_to_channel():
        assert proc.stdout is not None
        try:
            while True:
                buf = proc.stdout.read(4096)
                if not buf:
                    break
                try:
                    channel.sendall(buf)
                except (OSError, paramiko.SSHException):
                    return
        except (OSError, ValueError):
            return

    def _pipe_channel_to_proc():
        assert proc.stdin is not None
        while True:
            try:
                ready, _, _ = select.select([channel], [], [], 0.5)
            except (OSError, ValueError):
                return
            if not ready:
                if proc.poll() is not None:
                    return
                continue
            try:
                data = channel.recv(4096)
            except (OSError, paramiko.SSHException):
                return
            if not data:
                return
            try:
                proc.stdin.write(data)
                proc.stdin.flush()
            except (OSError, ValueError):
                return

    if master_fd is not None:
        t1 = threading.Thread(target=_pty_to_channel, daemon=True)
        t2 = threading.Thread(target=_channel_to_pty, daemon=True)
    else:
        t1 = threading.Thread(target=_pipe_proc_to_channel, daemon=True)
        t2 = threading.Thread(target=_pipe_channel_to_proc, daemon=True)
    t1.start()
    t2.start()

    while proc.poll() is None and not channel.closed:
        time.sleep(0.05)
    try:
        proc.terminate()
    except OSError:
        pass
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()
    if master_fd is not None:
        try:
            os.close(master_fd)
        except OSError:
            pass
    try:
        rc = proc.returncode if proc.returncode is not None else 0
        if rc < 0:
            rc = 128 + (-rc)
        channel.send_exit_status(rc & 0xFFFF_FFFF)
    except (OSError, paramiko.SSHException, struct_error):
        pass
    try:
        channel.close()
    except (OSError, paramiko.SSHException):
        pass


def _accept_loop(
    listener: socket.socket,
    host_key: paramiko.PKey,
    server_factory,
    stop_event: threading.Event,
    *,
    banner_after_shell_open: bytes | None,
) -> None:
    listener.settimeout(0.5)
    while not stop_event.is_set():
        try:
            client_sock, _addr = listener.accept()
        except socket.timeout:
            continue
        except OSError:
            return
        threading.Thread(
            target=_handle_client,
            args=(client_sock, host_key, server_factory, banner_after_shell_open),
            daemon=True,
        ).start()


def _drive_channel(chan, server, banner_after_shell_open: bytes | None) -> None:
    """Drive a single accepted channel based on its first request."""
    # Each channel carries its own per-request event; attach if needed.
    ev = getattr(chan, "_request_event", None)
    if ev is None:
        ev = threading.Event()
        chan._request_event = ev
    ev.wait(10)
    if hasattr(chan, "exec_command_payload"):
        _drive_exec_channel(chan, chan.exec_command_payload)
        return
    # SFTP subsystem drives itself via paramiko's transport-level handler.
    if getattr(chan, "_is_sftp_subsystem", False):
        return
    if not chan.closed:
        _drive_shell_channel(chan, banner_after_shell_open)


def _handle_client(
    client_sock: socket.socket,
    host_key: paramiko.PKey,
    server_factory,
    banner_after_shell_open: bytes | None,
) -> None:
    transport: paramiko.Transport | None = None
    try:
        transport = paramiko.Transport(client_sock)
        transport.add_server_key(host_key)
        # Register the SFTP subsystem so paramiko spawns it in its own
        # thread when the client requests subsystem=sftp.
        transport.set_subsystem_handler("sftp", paramiko.SFTPServer, _LocalSftpServer)
        server = server_factory()
        try:
            transport.start_server(server=server)
        except paramiko.SSHException:
            return
        # Loop on accept so the same SSH transport can host multiple
        # successive channels (each ssh_execute opens a fresh channel
        # against the transport — typical of OpenSSH `MaxSessions`).
        while transport.is_active():
            chan = transport.accept(20)
            if chan is None:
                # No new channel within the window; if the transport is
                # still alive the russh client is just idle. Loop again.
                if not transport.is_active():
                    return
                continue
            # Drive each channel in its own thread so ssh-mcp can pipeline.
            threading.Thread(
                target=_drive_channel,
                args=(chan, server, banner_after_shell_open),
                daemon=True,
            ).start()
    except (paramiko.SSHException, OSError, EOFError):
        return
    finally:
        if transport is not None:
            try:
                transport.close()
            except (OSError, paramiko.SSHException):
                pass
        try:
            client_sock.close()
        except OSError:
            pass


# ---------------------------------------------------------------------------
# Public fixture
# ---------------------------------------------------------------------------


@dataclass
class LocalSshdFixture:
    """In-process paramiko SSH server.

    Used as a context manager — `__enter__` binds the listener and returns
    self; `__exit__` shuts the listener down.
    """

    username: str = "testuser"
    password: str = "testpass"
    banner_after_shell_open: bytes | None = None

    def __post_init__(self) -> None:
        self._tmpdir = tempfile.TemporaryDirectory(prefix="ssh-mcp-fix-")
        self._tmp_path = Path(self._tmpdir.name)
        self._private_key_path, self._public_key_path = _generate_ed25519_keypair(
            self._tmp_path / "client_keys"
        )
        host_key_path = _generate_host_key(self._tmp_path / "host_keys")
        self._host_key = paramiko.RSAKey(filename=str(host_key_path))
        # Pre-load the public key blob for paramiko key-auth comparison.
        client_key = paramiko.Ed25519Key.from_private_key_file(str(self._private_key_path))
        self._public_key_blob = client_key.asbytes()
        self._port = _free_port()
        self._listener: socket.socket | None = None
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self.address = f"127.0.0.1:{self._port}"

    @property
    def private_key_path(self) -> str:
        return str(self._private_key_path)

    def _server_factory(self):
        return _ChannelServer(
            username=self.username,
            password=self.password,
            public_key_blob=self._public_key_blob,
            banner=None,
        )

    def __enter__(self) -> "LocalSshdFixture":
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", self._port))
        listener.listen(64)
        self._listener = listener
        self._thread = threading.Thread(
            target=_accept_loop,
            args=(
                listener,
                self._host_key,
                self._server_factory,
                self._stop,
            ),
            kwargs={"banner_after_shell_open": self.banner_after_shell_open},
            daemon=True,
        )
        self._thread.start()
        # Sanity: ensure the port is reachable.
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                with closing(socket.socket()) as s:
                    s.settimeout(0.5)
                    s.connect(("127.0.0.1", self._port))
                break
            except OSError:
                time.sleep(0.05)
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        self._stop.set()
        if self._listener is not None:
            try:
                self._listener.close()
            except OSError:
                pass
        if self._thread is not None:
            self._thread.join(timeout=2)
        try:
            self._tmpdir.cleanup()
        except OSError:
            pass

    def connect_args(self, **extra) -> dict:
        args: dict = {
            "address": self.address,
            "username": self.username,
            "password": self.password,
            "reuse": "force_new",
        }
        args.update(extra)
        return args


__all__ = ["LocalSshdFixture"]
