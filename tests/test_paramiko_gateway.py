import importlib.util
import io
import pathlib
import struct
import subprocess
import sys
import types
import unittest
from collections import deque
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]
HELPER = ROOT / "tools" / "paramiko_gateway.py"


def load_helper():
    if not HELPER.is_file():
        raise AssertionError(f"Paramiko helper is missing: {HELPER}")
    spec = importlib.util.spec_from_file_location("paramiko_gateway", HELPER)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


# Local transport fixtures only. These opaque event-shaped bytes are not real
# credentials/signatures and do not claim protocol or cross-host validation.
LAUNCH_BYTES = b'{"kind":"signed-event","event":"launch","signature":"synthetic"}\n'
TERMINAL_BYTES = b'{"kind":"signed-event","event":"done","signature":"synthetic"}\n'
SYNTHETIC_KEY = b"synthetic-not-a-real-private-key"


class LocalStdin:
    def __init__(self):
        self.writes = []
        self.flushed = False
        self.shutdown_write_called = False
        self.channel = self

    def write(self, value):
        self.writes.append(value)
        return len(value)

    def flush(self):
        self.flushed = True

    def shutdown_write(self):
        self.shutdown_write_called = True


class LocalChannel:
    """In-memory byte queues; exceptions are delivered only after prior bytes."""

    def __init__(self, stdout=(), stderr=(), *, early_status=False, status=0,
                 status_error=None, stall_after_output=False):
        self.stdout = deque(stdout)
        self.stderr = deque(stderr)
        self.early_status = early_status
        self.status = status
        self.status_error = status_error
        self.stall_after_output = stall_after_output
        self.received_stdout = bytearray()
        self.received_stderr = bytearray()
        self.status_reads = 0

    def settimeout(self, timeout):
        self.timeout = timeout

    def exit_status_ready(self):
        return self.early_status or (
            not self.stdout and not self.stderr and not self.stall_after_output
        )

    def recv_ready(self):
        return bool(self.stdout)

    def recv_stderr_ready(self):
        return bool(self.stderr)

    @staticmethod
    def _receive(queue, size, received):
        value = queue.popleft()
        if isinstance(value, BaseException):
            raise value
        chunk = value[:size]
        if len(value) > size:
            queue.appendleft(value[size:])
        received.extend(chunk)
        return chunk

    def recv(self, size):
        return self._receive(self.stdout, size, self.received_stdout)

    def recv_stderr(self, size):
        return self._receive(self.stderr, size, self.received_stderr)

    def recv_exit_status(self):
        self.status_reads += 1
        if self.status_error is not None:
            raise self.status_error
        return self.status


class LocalClock:
    """Bound stalls deterministically, without sleeping or network activity."""

    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        self.now += 0.001
        return self.now

    def sleep(self, duration):
        self.now += duration


class LocalHostKeyError(Exception):
    pass


class LocalRejectPolicy:
    def missing_host_key(self, client, hostname, key):
        raise LocalHostKeyError("synthetic unknown host")


class LocalClient:
    def __init__(self, channel, *, unknown_host=False, close_error=None,
                 connect_delay=0, exec_delay=0, clock=None):
        self.connect_delay = connect_delay
        self.exec_delay = exec_delay
        self.clock = clock
        self.channel = channel
        self.unknown_host = unknown_host
        self.close_error = close_error
        self.calls = []
        self.missing_host_key_policy = None
        self.connect_kwargs = None
        self.exec_command_value = None
        self.closed = False
        self.stdin = LocalStdin()

    def load_host_keys(self, path):
        # Deliberately record, never open the synthetic path.
        self.calls.append(("load_host_keys", path))

    def set_missing_host_key_policy(self, policy):
        self.missing_host_key_policy = policy
        self.calls.append(("policy", type(policy)))

    def connect(self, **kwargs):
        self.connect_kwargs = kwargs
        self.calls.append(("connect",))
        if self.clock:
            self.clock.sleep(self.connect_delay)
        if self.unknown_host:
            self.missing_host_key_policy.missing_host_key(
                self, kwargs["hostname"], object()
            )

    def exec_command(self, command, timeout=None):
        self.exec_command_value = (command, timeout)
        self.calls.append(("exec_command",))
        if self.clock:
            self.clock.sleep(self.exec_delay)
        stream = types.SimpleNamespace(channel=self.channel)
        return self.stdin, stream, stream

    def close(self):
        self.closed = True
        self.calls.append(("close",))
        if self.close_error is not None:
            raise self.close_error


class ParamikoGatewayExchangeTests(unittest.TestCase):
    """Exercise the actual main -> decode -> key-load -> exchange lifecycle."""

    def setUp(self):
        self.helper = load_helper()
        self.metadata = {
            "schema": "ao-paramiko-gateway/1",
            "host": "synthetic.invalid",
            "port": 22,
            "user": "synthetic-user",
            "known_hosts": "synthetic-known-hosts-never-opened",
            "remote_command": "synthetic-command-never-executed",
            "timeout_secs": 1,
            "request": "{\"signed\":\"synthetic\"}\n",
        }

    def run_gateway(self, channel, **client_options):
        clock = LocalClock()
        self.clock = clock
        client = LocalClient(channel, clock=clock, **client_options)
        parsed_keys = []
        parsed_key = object()

        class LocalKey:
            @staticmethod
            def from_private_key(stream):
                parsed_keys.append(stream.read())
                return parsed_key

        # Replace only external I/O boundaries. No production exchange, frame,
        # buffering, redaction, key-loader, or exception handler is mocked.
        paramiko = types.ModuleType("paramiko")
        paramiko.SSHClient = lambda: client
        paramiko.RejectPolicy = LocalRejectPolicy
        paramiko.RSAKey = LocalKey
        frame = self.helper.encode_frame(self.metadata, SYNTHETIC_KEY)
        stdin = io.BytesIO(frame)
        stdout = io.BytesIO()
        stderr = io.BytesIO()
        streams = types.SimpleNamespace(
            stdin=types.SimpleNamespace(buffer=stdin),
            stdout=types.SimpleNamespace(buffer=stdout),
            stderr=types.SimpleNamespace(buffer=stderr),
        )
        with mock.patch.dict(sys.modules, {"paramiko": paramiko}), \
             mock.patch.object(self.helper, "sys", streams), \
             mock.patch.object(self.helper, "time", clock):
            code = self.helper.main()
        self.assertTrue(client.closed, "the actual exchange must close its client")
        self.assertEqual(parsed_keys, [SYNTHETIC_KEY.decode("ascii")])
        return types.SimpleNamespace(
            code=code, stdout=stdout.getvalue(), stderr=stderr.getvalue(),
            client=client, channel=channel, parsed_key=parsed_key,
        )

    def assert_retained_failure(self, result, expected_stdout, expected_stderr=b""):
        # These primary assertions observe production output/status, not merely
        # that the fake was called. Failure must not erase bytes already recv'd.
        self.assertEqual(result.stdout, expected_stdout)
        self.assertTrue(result.stderr.startswith(expected_stderr), result.stderr)
        self.assertNotEqual(result.code, 0)
        self.assertIn(b"Paramiko gateway failure:", result.stderr)

    def delegated(self):
        self.metadata.update(timeout_secs=26, setup_timeout_secs=15, channel_timeout_secs=11)

    def test_late_connect_never_executes_remote_command(self):
        self.delegated()
        result = self.run_gateway(LocalChannel(), connect_delay=16)
        self.assertNotEqual(result.code, 0)
        self.assertIsNone(result.client.exec_command_value)
        self.assertIn(b"TimeoutError", result.stderr)

    def test_late_exec_is_failure_not_evidence_of_no_launch(self):
        self.delegated()
        result = self.run_gateway(LocalChannel(), connect_delay=10, exec_delay=6)
        self.assertNotEqual(result.code, 0)
        self.assertIsNotNone(result.client.exec_command_value)
        self.assertIn(b"TimeoutError", result.stderr)

    def test_total_deadline_is_not_refreshed_after_setup(self):
        self.delegated()
        result = self.run_gateway(LocalChannel([LAUNCH_BYTES], stall_after_output=True), connect_delay=14)
        self.assert_retained_failure(result, LAUNCH_BYTES)
        self.assertLess(self.clock.now, 25.2, "channel must stop at setup-finish+11, not +H")

    def test_frame_read_time_counts_toward_setup_deadline(self):
        self.delegated()
        original = self.helper._read_frame
        def delayed_read():
            self.helper.time.sleep(16)
            return original()
        with mock.patch.object(self.helper, "_read_frame", delayed_read):
            result = self.run_gateway(LocalChannel())
        self.assertNotEqual(result.code, 0)
        self.assertIsNone(result.client.exec_command_value)

    def test_total_deadline_is_an_original_upper_bound(self):
        self.delegated()
        # The boundary fixture below delays stdin transmission, not production timing.
        with mock.patch.object(LocalStdin, "write", lambda stdin, value: (self.helper.time.sleep(20), len(value))[1]):
            result = self.run_gateway(LocalChannel([LAUNCH_BYTES], stall_after_output=True), connect_delay=14)
        self.assertNotEqual(result.code, 0)
        self.assertLess(self.clock.now, 34.2, "expired H must not be refreshed after slow write")
        self.assertEqual(result.stdout, b"")

    def test_late_write_with_ready_exit_still_fails_total_deadline(self):
        self.delegated()
        with mock.patch.object(LocalStdin, "write", lambda stdin, value: (self.helper.time.sleep(20), len(value))[1]):
            result = self.run_gateway(LocalChannel(), connect_delay=14)
        self.assertNotEqual(result.code, 0, "ready exit must not bypass expired H")

    def test_paired_metadata_requires_exact_deadline_arithmetic(self):
        for fields in [dict(setup_timeout_secs=15), dict(channel_timeout_secs=11),
                       dict(setup_timeout_secs=14, channel_timeout_secs=12),
                       dict(setup_timeout_secs=15, channel_timeout_secs=12),
                       dict(setup_timeout_secs=None, channel_timeout_secs=None)]:
            candidate = dict(self.metadata, timeout_secs=26, **fields)
            with self.assertRaises(ValueError):
                self.helper._require_metadata(candidate)

    def test_complete_signed_event_survives_remote_eof_after_received_bytes(self):
        payload = LAUNCH_BYTES + TERMINAL_BYTES
        channel = LocalChannel(
            [payload[:19], payload[19:], EOFError("synthetic remote EOF")],
            [b"synthetic worker diagnostic\n"],
        )
        result = self.run_gateway(channel)
        self.assertEqual(bytes(channel.received_stdout), payload)
        self.assert_retained_failure(result, payload, b"synthetic worker diagnostic\n")

    def test_complete_signed_event_survives_connection_reset_during_stderr_drain(self):
        payload = LAUNCH_BYTES + TERMINAL_BYTES
        channel = LocalChannel(
            [payload],
            [b"synthetic stderr\n", ConnectionResetError("synthetic reset")],
        )
        result = self.run_gateway(channel)
        self.assertEqual(bytes(channel.received_stdout), payload)
        self.assertEqual(bytes(channel.received_stderr), b"synthetic stderr\n")
        self.assert_retained_failure(result, payload, b"synthetic stderr\n")

    def test_complete_signed_event_survives_remote_close_without_exit_status(self):
        payload = LAUNCH_BYTES + TERMINAL_BYTES
        result = self.run_gateway(LocalChannel([payload], status=-1))
        self.assertEqual(result.stdout, payload)
        self.assertEqual(result.stderr, b"")
        self.assertEqual(result.code, -1)

    def test_complete_signed_event_survives_exit_status_connection_error(self):
        payload = LAUNCH_BYTES + TERMINAL_BYTES
        result = self.run_gateway(LocalChannel(
            [payload], status_error=ConnectionError("synthetic status failure")
        ))
        self.assert_retained_failure(result, payload)

    def test_complete_signed_event_survives_client_close_error(self):
        payload = LAUNCH_BYTES + TERMINAL_BYTES
        result = self.run_gateway(
            LocalChannel([payload]), close_error=ConnectionError("synthetic close failure")
        )
        self.assert_retained_failure(result, payload)

    def test_launch_only_failure_retains_bytes_and_returns_non_success(self):
        channel = LocalChannel([LAUNCH_BYTES, ConnectionError("synthetic failure")])
        result = self.run_gateway(channel)
        self.assertEqual(bytes(channel.received_stdout), LAUNCH_BYTES)
        self.assert_retained_failure(result, LAUNCH_BYTES)

    def test_partial_terminal_failure_retains_unfinished_line_and_non_success(self):
        partial = LAUNCH_BYTES + TERMINAL_BYTES[:23]
        channel = LocalChannel([partial, ConnectionResetError("synthetic reset")])
        result = self.run_gateway(channel)
        self.assertEqual(bytes(channel.received_stdout), partial)
        self.assert_retained_failure(result, partial)
        self.assertNotIn(TERMINAL_BYTES, result.stdout)

    def test_partial_output_timeout_retains_stdout_stderr_and_non_success(self):
        partial = LAUNCH_BYTES + TERMINAL_BYTES[:11]
        result = self.run_gateway(LocalChannel(
            [partial], [b"synthetic timeout diagnostic\n"], stall_after_output=True
        ))
        self.assert_retained_failure(result, partial, b"synthetic timeout diagnostic\n")
        self.assertIn(b"TimeoutError", result.stderr)

    def test_exit_status_ready_does_not_skip_remaining_stdout_and_stderr(self):
        payload = LAUNCH_BYTES + TERMINAL_BYTES
        channel = LocalChannel(
            [payload[:9], payload[9:41], payload[41:]],
            [b"err-1", b"err-2", b"err-3"], early_status=True, status=7,
        )
        result = self.run_gateway(channel)
        self.assertEqual(result.code, 7)
        self.assertEqual(result.stdout, payload)
        self.assertEqual(result.stderr, b"err-1err-2err-3")
        self.assertFalse(channel.stdout)
        self.assertFalse(channel.stderr)
        self.assertEqual(channel.status_reads, 1)

    def test_stdout_bound_retains_prefix_drains_both_streams_and_fails(self):
        channel = LocalChannel([b"1234", b"56789", b"tail"], [b"diagnostic"])
        with mock.patch.object(self.helper, "MAX_STDOUT", 8):
            result = self.run_gateway(channel)
        self.assertEqual(result.code, 3)
        self.assertEqual(result.stdout, b"12345678")
        self.assertEqual(
            result.stderr, b"diagnosticParamiko gateway output bound exceeded\n"
        )
        self.assertFalse(channel.stdout)
        self.assertFalse(channel.stderr)

    def test_stderr_bound_retains_prefix_drains_both_streams_and_fails(self):
        channel = LocalChannel([b"stdout"], [b"abcd", b"efghi", b"tail"])
        with mock.patch.object(self.helper, "MAX_STDERR", 8):
            result = self.run_gateway(channel)
        self.assertEqual(result.code, 3)
        self.assertEqual(result.stdout, b"stdout")
        self.assertEqual(
            result.stderr, b"abcdefghParamiko gateway output bound exceeded\n"
        )
        self.assertFalse(channel.stdout)
        self.assertFalse(channel.stderr)

    def test_actual_production_stdout_bound_is_enforced(self):
        limit = self.helper.MAX_STDOUT
        payload = b"s" * (limit + 1)
        channel = LocalChannel([payload], [b"synthetic diagnostic\n"], early_status=True)
        result = self.run_gateway(channel)
        self.assertEqual(result.code, 3)
        self.assertEqual(result.stdout, payload[:limit])
        self.assertEqual(len(channel.received_stdout), limit + 1)
        self.assertEqual(
            result.stderr,
            b"synthetic diagnostic\nParamiko gateway output bound exceeded\n",
        )

    def test_actual_production_stderr_bound_is_enforced(self):
        limit = self.helper.MAX_STDERR
        payload = b"e" * (limit + 1)
        channel = LocalChannel([TERMINAL_BYTES], [payload], early_status=True)
        result = self.run_gateway(channel)
        self.assertEqual(result.code, 3)
        self.assertEqual(result.stdout, TERMINAL_BYTES)
        self.assertEqual(len(channel.received_stderr), limit + 1)
        self.assertEqual(
            result.stderr, payload[:limit] + b"Paramiko gateway output bound exceeded\n"
        )

    def test_output_exactly_at_both_bounds_is_not_overflow(self):
        with mock.patch.object(self.helper, "MAX_STDOUT", 8), \
             mock.patch.object(self.helper, "MAX_STDERR", 8):
            result = self.run_gateway(LocalChannel([b"1234", b"5678"], [b"abcdefgh"]))
        self.assertEqual(result.code, 0)
        self.assertEqual(result.stdout, b"12345678")
        self.assertEqual(result.stderr, b"abcdefgh")

    def test_failure_output_is_redacted_even_when_synthetic_key_spans_chunks(self):
        prefix, suffix = SYNTHETIC_KEY[:12], SYNTHETIC_KEY[12:]
        result = self.run_gateway(LocalChannel(
            [b"out:" + prefix, suffix, ConnectionError("synthetic failure")],
            [b"err:" + SYNTHETIC_KEY],
        ))
        self.assert_retained_failure(result, b"out:[REDACTED]", b"err:[REDACTED]")
        self.assertNotIn(SYNTHETIC_KEY, result.stdout + result.stderr)

    def test_reject_policy_and_disabled_agent_key_search_are_enforced(self):
        result = self.run_gateway(LocalChannel())
        client = result.client
        self.assertEqual(result.code, 0)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(result.stderr, b"")
        self.assertIsInstance(client.missing_host_key_policy, LocalRejectPolicy)
        self.assertEqual(client.calls[:3], [
            ("load_host_keys", self.metadata["known_hosts"]),
            ("policy", LocalRejectPolicy), ("connect",),
        ])
        self.assertIs(client.connect_kwargs["look_for_keys"], False)
        self.assertIs(client.connect_kwargs["allow_agent"], False)
        self.assertIs(client.connect_kwargs["pkey"], result.parsed_key)
        self.assertEqual(client.exec_command_value, (self.metadata["remote_command"], 1))
        self.assertEqual(client.stdin.writes, [self.metadata["request"].encode("utf-8")])
        self.assertTrue(client.stdin.flushed)
        self.assertTrue(client.stdin.shutdown_write_called)

    def test_unknown_host_is_rejected_before_command_execution(self):
        result = self.run_gateway(LocalChannel(), unknown_host=True)
        self.assertNotEqual(result.code, 0)
        self.assertEqual(result.stdout, b"")
        self.assertIn(b"LocalHostKeyError", result.stderr)
        self.assertIsNone(result.client.exec_command_value)
        self.assertEqual(result.client.stdin.writes, [])


class ParamikoGatewayFrameTests(unittest.TestCase):
    def test_frame_round_trip_keeps_private_key_outside_json_header(self):
        helper = load_helper()
        metadata = {
            "schema": "ao-paramiko-gateway/1",
            "host": "worker.example",
            "port": 22,
            "user": "worker",
            "known_hosts": "C:/known_hosts",
            "remote_command": "/opt/aw/aw-cli owner-exec --config /etc/aw/worker.json",
            "timeout_secs": 9,
            "request": '{"signed":true}\n',
        }
        secret = "-----BEGIN PRIVATE KEY-----\nnot-for-argv\n-----END PRIVATE KEY-----\n"
        frame = helper.encode_frame(metadata, secret.encode("utf-8"))
        decoded_metadata, decoded_key, decoded_token = helper.decode_frame(frame)
        self.assertEqual(decoded_metadata, metadata)
        self.assertEqual(decoded_key, secret.encode("utf-8"))
        metadata_length_start = len(helper.MAGIC)
        metadata_length = int.from_bytes(frame[metadata_length_start:metadata_length_start + 4], "big")
        metadata_start = metadata_length_start + 4
        metadata_end = metadata_start + metadata_length
        self.assertNotIn(secret.encode("utf-8"), frame[metadata_start:metadata_end])

    def test_frame_rejects_length_that_exceeds_input(self):
        helper = load_helper()
        with self.assertRaises(ValueError):
            helper.decode_frame(helper.MAGIC + (20).to_bytes(4, "big") + b"{}")

    def test_bounded_buffer_reports_overflow_without_unbounded_growth(self):
        helper = load_helper()
        buffer = helper.BoundedBuffer(4)
        buffer.append(b"abc")
        buffer.append(b"defgh")
        self.assertEqual(buffer.value(), b"abcd")
        self.assertTrue(buffer.overflow)

    def test_invalid_frame_does_not_echo_secret(self):
        secret = b"private-key-must-not-be-echoed"
        result = subprocess.run(
            [sys.executable, "-I", str(HELPER)],
            input=b"not-a-frame\n" + secret,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertNotIn(secret, result.stdout + result.stderr)
        self.assertNotIn(b"not-a-frame", result.stdout + result.stderr)

    def test_output_redaction_replaces_private_key_bytes(self):
        helper = load_helper()
        self.assertEqual(
            helper.redact_output(b"prefixsecret-suffix", b"secret"),
            b"prefix[REDACTED]-suffix",
        )


class ParamikoGatewayTokenFieldTests(unittest.TestCase):
    """Third binary frame field carries the model token alongside the private
    key: same transport guarantees (never in JSON metadata, argv, or output)."""

    def test_frame_round_trip_with_token_field(self):
        helper = load_helper()
        metadata = {
            "schema": "ao-paramiko-gateway/1",
            "host": "worker.example",
            "port": 22,
            "user": "worker",
            "known_hosts": "C:/known_hosts",
            "remote_command": "/opt/aw/aw-cli owner-exec --config /etc/aw/worker.json",
            "timeout_secs": 9,
            "request": '{"signed":true}\n',
        }
        secret = b"-----BEGIN PRIVATE KEY-----\nnot-for-argv\n-----END PRIVATE KEY-----\n"
        token = b"sk-live-token-not-for-json-argv-or-logs"
        frame = helper.encode_frame(metadata, secret, token)
        decoded_metadata, decoded_key, decoded_token = helper.decode_frame(frame)
        self.assertEqual(decoded_metadata, metadata)
        self.assertEqual(decoded_key, secret)
        self.assertEqual(decoded_token, token)
        metadata_length_start = len(helper.MAGIC)
        metadata_length = int.from_bytes(
            frame[metadata_length_start:metadata_length_start + 4], "big"
        )
        metadata_start = metadata_length_start + 4
        metadata_end = metadata_start + metadata_length
        self.assertNotIn(token, frame[metadata_start:metadata_end])

    def test_decode_without_token_returns_empty_and_is_backward_compatible(self):
        helper = load_helper()
        metadata = {
            "schema": "ao-paramiko-gateway/1",
            "host": "worker.example", "port": 22, "user": "worker",
            "known_hosts": "C:/known_hosts", "remote_command": "true",
            "timeout_secs": 9, "request": '{"signed":true}\n',
        }
        secret = b"synthetic-not-a-real-private-key"
        # Two-field frame (legacy) must still decode with token == b"".
        frame = helper.encode_frame(metadata, secret)
        decoded_metadata, decoded_key, decoded_token = helper.decode_frame(frame)
        self.assertEqual(decoded_metadata, metadata)
        self.assertEqual(decoded_key, secret)
        self.assertEqual(decoded_token, b"")

    def test_token_oversize_is_rejected(self):
        helper = load_helper()
        metadata = {
            "schema": "ao-paramiko-gateway/1",
            "host": "worker.example", "port": 22, "user": "worker",
            "known_hosts": "C:/known_hosts", "remote_command": "true",
            "timeout_secs": 9, "request": '{"signed":true}\n',
        }
        secret = b"synthetic-not-a-real-private-key"
        oversize = b"x" * (helper.MAX_MODEL_TOKEN + 1)
        with self.assertRaises(ValueError):
            helper.encode_frame(metadata, secret, oversize)

    def test_exchange_writes_token_line_after_signed_request(self):
        # Same production lifecycle as run_gateway: main -> decode -> exchange
        # with only the paramiko module faked. The token must reach the channel
        # stdin AFTER the signed request and never appear in metadata output.
        helper = load_helper()
        metadata = {
            "schema": "ao-paramiko-gateway/1",
            "host": "synthetic.invalid", "port": 22, "user": "synthetic-user",
            "known_hosts": "synthetic-known-hosts-never-opened",
            "remote_command": "synthetic-command-never-executed",
            "timeout_secs": 1, "request": "{\"signed\":\"synthetic\"}\n",
        }
        token = b"sk-live-token-on-second-line"
        channel = LocalChannel(stdout=[b'{"kind":"done"}\n'])
        clock = LocalClock()
        client = LocalClient(channel, clock=clock)
        frame = helper.encode_frame(metadata, SYNTHETIC_KEY, token)
        stdin = io.BytesIO(frame)
        stdout = io.BytesIO()
        stderr = io.BytesIO()
        streams = types.SimpleNamespace(
            stdin=types.SimpleNamespace(buffer=stdin),
            stdout=types.SimpleNamespace(buffer=stdout),
            stderr=types.SimpleNamespace(buffer=stderr),
        )
        paramiko = types.ModuleType("paramiko")
        paramiko.SSHClient = lambda: client
        paramiko.RejectPolicy = LocalRejectPolicy
        paramiko.RSAKey = type("LocalKey", (), {"from_private_key": staticmethod(lambda stream: object())})
        with mock.patch.dict(sys.modules, {"paramiko": paramiko}), \
             mock.patch.object(helper, "sys", streams), \
             mock.patch.object(helper, "time", clock):
            code = helper.main()
        self.assertEqual(code, 0)
        self.assertEqual(
            client.stdin.writes,
            [metadata["request"].encode("utf-8"), struct.pack(">I", len(token)), token],
            "length-framed token must be written after the signed request on channel stdin",
        )
        self.assertNotIn(token, stdout.getvalue() + stderr.getvalue())
        self.assertNotIn(token, frame[: frame.find(b'{"schema"')])


if __name__ == "__main__":
    unittest.main()
