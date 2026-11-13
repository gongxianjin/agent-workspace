#!/usr/bin/env python3
"""Bounded, memory-key Paramiko gateway helper.

The process receives one length-framed request on stdin. The private key is a
separate binary field and is never part of the JSON metadata, argv, or env.
Worker stdout and stderr are the only successful output streams.
"""

from __future__ import annotations

import io
import json
import struct
import sys
import time
from typing import Any

MAGIC = b"AO-PARAMIKO-GATEWAY/1\n"
MAX_METADATA = 64 * 1024
MAX_PRIVATE_KEY = 64 * 1024
MAX_MODEL_TOKEN = 16 * 1024
MAX_FRAME = 192 * 1024
MAX_STDOUT = 1024 * 1024
MAX_STDERR = 64 * 1024


class BoundedBuffer:
    def __init__(self, limit: int) -> None:
        self.limit = limit
        self._value = bytearray()
        self.overflow = False

    def append(self, value: bytes) -> bytes:
        available = max(0, self.limit - len(self._value))
        if len(value) > available:
            self.overflow = True
        accepted = value[:available]
        self._value.extend(accepted)
        return accepted

    def value(self) -> bytes:
        return bytes(self._value)


def _read_exact(raw: bytes, offset: int, length: int) -> tuple[bytes, int]:
    end = offset + length
    if length < 0 or end > len(raw):
        raise ValueError("truncated Paramiko gateway frame")
    return raw[offset:end], end


def encode_frame(metadata: dict[str, Any], private_key: bytes, model_token: bytes = b"") -> bytes:
    """Encode one bounded request with JSON metadata and separate binary fields.

    The private key and the optional model token are binary fields outside the
    JSON metadata: neither ever appears in metadata, argv, or the environment.
    A legacy caller that passes no token produces the exact two-field frame.
    """
    if not isinstance(private_key, bytes) or not private_key or len(private_key) > MAX_PRIVATE_KEY:
        raise ValueError("private key field is empty or oversized")
    if not isinstance(model_token, bytes) or len(model_token) > MAX_MODEL_TOKEN:
        raise ValueError("model token field is oversized")
    metadata_raw = json.dumps(
        metadata, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    ).encode("utf-8")
    if len(metadata_raw) > MAX_METADATA:
        raise ValueError("metadata field is oversized")
    frame = MAGIC + struct.pack(">I", len(metadata_raw)) + metadata_raw
    frame += struct.pack(">I", len(private_key)) + private_key
    if model_token:
        frame += struct.pack(">I", len(model_token)) + model_token
    if len(frame) > MAX_FRAME:
        raise ValueError("Paramiko gateway frame is oversized")
    return frame


def decode_frame(raw: bytes) -> tuple[dict[str, Any], bytes, bytes]:
    if not isinstance(raw, bytes) or len(raw) > MAX_FRAME:
        raise ValueError("Paramiko gateway frame is oversized")
    if not raw.startswith(MAGIC):
        raise ValueError("invalid Paramiko gateway frame magic")
    offset = len(MAGIC)
    length_raw, offset = _read_exact(raw, offset, 4)
    metadata_length = struct.unpack(">I", length_raw)[0]
    if metadata_length > MAX_METADATA:
        raise ValueError("metadata field is oversized")
    metadata_raw, offset = _read_exact(raw, offset, metadata_length)
    key_length_raw, offset = _read_exact(raw, offset, 4)
    key_length = struct.unpack(">I", key_length_raw)[0]
    if key_length == 0 or key_length > MAX_PRIVATE_KEY:
        raise ValueError("private key field is empty or oversized")
    private_key, offset = _read_exact(raw, offset, key_length)
    token_length_raw, token_bytes, token_length = b"", b"", 0
    if offset < len(raw):
        token_length_raw, offset = _read_exact(raw, offset, 4)
        token_length = struct.unpack(">I", token_length_raw)[0]
        if token_length == 0 or token_length > MAX_MODEL_TOKEN:
            raise ValueError("model token field is empty or oversized")
        token_bytes, offset = _read_exact(raw, offset, token_length)
    if offset != len(raw):
        raise ValueError("trailing bytes in Paramiko gateway frame")
    try:
        metadata = json.loads(metadata_raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("metadata field is not valid JSON") from exc
    if not isinstance(metadata, dict):
        raise ValueError("metadata field must be a JSON object")
    return metadata, private_key, token_bytes


def _read_frame() -> bytes:
    raw = sys.stdin.buffer.read(MAX_FRAME + 1)
    if len(raw) > MAX_FRAME:
        raise ValueError("Paramiko gateway frame is oversized")
    return raw


def _load_private_key(private_key: bytes):
    import paramiko

    if len(private_key) > MAX_PRIVATE_KEY:
        raise ValueError("private key field is oversized")
    text = private_key.decode("utf-8")
    errors: list[str] = []
    for name in ("RSAKey", "Ed25519Key", "ECDSAKey"):
        key_type = getattr(paramiko, name, None)
        if key_type is None:
            continue
        try:
            return key_type.from_private_key(io.StringIO(text))
        except Exception as exc:  # Paramiko exposes parser-specific exceptions.
            errors.append(type(exc).__name__)
    raise ValueError("private key could not be parsed: " + ",".join(errors))


def _require_metadata(metadata: dict[str, Any]) -> tuple[str, int, str, str, str, int, int | None, int | None, bytes]:
    if metadata.get("schema") != "ao-paramiko-gateway/1":
        raise ValueError("unsupported Paramiko gateway schema")
    host = metadata.get("host")
    port = metadata.get("port")
    user = metadata.get("user")
    known_hosts = metadata.get("known_hosts")
    remote_command = metadata.get("remote_command")
    timeout_secs = metadata.get("timeout_secs")
    request = metadata.get("request")
    setup_timeout = metadata.get("setup_timeout_secs")
    channel_timeout = metadata.get("channel_timeout_secs")
    delegated = "setup_timeout_secs" in metadata or "channel_timeout_secs" in metadata
    if delegated:
        if not isinstance(setup_timeout, int) or setup_timeout != 15:
            raise ValueError("delegated setup timeout must be 15 seconds")
        if type(channel_timeout) is not int or not 11 <= channel_timeout <= 280:
            raise ValueError("delegated channel timeout is invalid")
        if timeout_secs != setup_timeout + channel_timeout:
            raise ValueError("delegated deadline arithmetic mismatch")
    if not isinstance(host, str) or not host or len(host) > 255:
        raise ValueError("invalid SSH host")
    if not isinstance(port, int) or not 1 <= port <= 65535:
        raise ValueError("invalid SSH port")
    if not isinstance(user, str) or not user or len(user) > 256:
        raise ValueError("invalid SSH user")
    if not isinstance(known_hosts, str) or not known_hosts:
        raise ValueError("known_hosts is required")
    if not isinstance(remote_command, str) or not remote_command:
        raise ValueError("remote command is required")
    if not isinstance(timeout_secs, int) or not 1 <= timeout_secs <= 300:
        raise ValueError("invalid SSH timeout")
    if not isinstance(request, str) or not request.endswith("\n"):
        raise ValueError("signed request must be newline framed")
    return host, port, user, known_hosts, remote_command, timeout_secs, setup_timeout, channel_timeout, request.encode("utf-8")


def redact_output(value: bytes, private_key: bytes) -> bytes:
    if not private_key:
        return value
    return value.replace(private_key, b"[REDACTED]")


def _write(stream, value: bytes) -> None:
    stream.write(value)
    stream.flush()


def _remaining(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("Paramiko cumulative deadline exceeded")
    return remaining


def exchange(metadata: dict[str, Any], private_key: bytes, started: float | None = None, model_token: bytes = b"") -> int:
    started = time.monotonic() if started is None else started
    host, port, user, known_hosts, remote_command, timeout_secs, setup_secs, channel_secs, request = _require_metadata(metadata)
    delegated = setup_secs is not None
    total_deadline = started + timeout_secs
    setup_deadline = min(total_deadline, started + setup_secs) if delegated else None
    key = _load_private_key(private_key)
    import paramiko

    client = paramiko.SSHClient()
    secrets = [private_key, model_token] if model_token else [private_key]
    out: BoundedBuffer | None = None
    err: BoundedBuffer | None = None
    failure: Exception | None = None
    status: int | None = None
    try:
        client.load_host_keys(known_hosts)
        client.set_missing_host_key_policy(paramiko.RejectPolicy())
        setup_remaining = _remaining(setup_deadline) if delegated else timeout_secs
        client.connect(
            hostname=host,
            port=port,
            username=user,
            pkey=key,
            look_for_keys=False,
            allow_agent=False,
            timeout=setup_remaining,
            auth_timeout=setup_remaining,
            banner_timeout=setup_remaining,
        )
        if delegated:
            _remaining(setup_deadline)
        stdin, stdout, stderr = client.exec_command(
            remote_command, timeout=_remaining(setup_deadline) if delegated else timeout_secs
        )
        if delegated:
            _remaining(setup_deadline)
        channel = stdout.channel
        channel_deadline = min(total_deadline, time.monotonic() + channel_secs) if delegated else None
        if delegated:
            channel.settimeout(_remaining(channel_deadline))
        stdin.write(request)
        stdin.flush()
        if model_token:
            # Length-framed so the worker can read exactly this many bytes
            # after the newline-terminated start frame; never logged.
            stdin.write(struct.pack(">I", len(model_token)))
            stdin.write(model_token)
            stdin.flush()
        stdin.channel.shutdown_write()
        if not delegated:
            channel_deadline = time.monotonic() + timeout_secs
        else:
            _remaining(channel_deadline)
        out = BoundedBuffer(MAX_STDOUT)
        err = BoundedBuffer(MAX_STDERR)
        try:
            while not channel.exit_status_ready() or channel.recv_ready() or channel.recv_stderr_ready():
                remaining = channel_deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("Paramiko channel timeout")
                if delegated:
                    channel.settimeout(remaining)
                if channel.recv_ready():
                    chunk = channel.recv(65536)
                    if chunk:
                        out.append(chunk)
                if channel.recv_stderr_ready():
                    if delegated:
                        channel.settimeout(_remaining(channel_deadline))
                    chunk = channel.recv_stderr(65536)
                    if chunk:
                        err.append(chunk)
                if not channel.recv_ready() and not channel.recv_stderr_ready():
                    time.sleep(min(remaining, 0.05))
            if delegated:
                channel.settimeout(_remaining(channel_deadline))
            status = int(channel.recv_exit_status())
            if delegated:
                _remaining(channel_deadline)
        except Exception as exc:
            failure = exc
    except Exception as exc:
        failure = exc
    finally:
        try:
            client.close()
        except Exception as exc:
            if failure is None:
                failure = exc

    if out is not None and err is not None:
        redacted_out = out.value()
        redacted_err = err.value()
        for secret in secrets:
            redacted_out = redact_output(redacted_out, secret)
            redacted_err = redact_output(redacted_err, secret)
        _write(sys.stdout.buffer, redacted_out)
        _write(sys.stderr.buffer, redacted_err)
        if delegated and failure is None:
            try:
                _remaining(total_deadline)
            except TimeoutError as exc:
                failure = exc
        if out.overflow or err.overflow:
            _write(sys.stderr.buffer, b"Paramiko gateway output bound exceeded\n")
            if failure is None:
                return 3
    if failure is not None:
        raise failure
    if status is None:
        raise RuntimeError("Paramiko gateway did not produce an exit status")
    return status


def main() -> int:
    started = time.monotonic()
    try:
        metadata, private_key, model_token = decode_frame(_read_frame())
        return exchange(metadata, private_key, started, model_token)
    except Exception as exc:
        _write(sys.stderr.buffer, (f"Paramiko gateway failure: {type(exc).__name__}\n").encode("ascii"))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
