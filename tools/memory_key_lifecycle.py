"""Temporary Paramiko SSH key lifecycle primitives.

The private key is kept in a bytearray for the lease lifetime. Callers may
persist the returned public authorization record, but this module never places
private-key bytes in that record or in command strings.
"""
from __future__ import annotations

import contextlib
import hashlib
import io
import shlex
from typing import Callable, Iterator


def _public_record(public_key: str) -> dict[str, str]:
    return {
        "schema": "temporary-ssh-authorization/1",
        "key_type": public_key.split(" ", 1)[0],
        "public_key": public_key,
        "public_key_sha256": hashlib.sha256(public_key.encode("ascii")).hexdigest(),
    }


@contextlib.contextmanager
def generated_key_lease(bits: int = 2048) -> Iterator[tuple[bytearray, dict[str, str]]]:
    """Generate one RSA key and clear the exported PEM bytearray on exit."""
    import paramiko

    key = paramiko.RSAKey.generate(bits)
    stream = io.StringIO()
    key.write_private_key(stream)
    private_key = bytearray(stream.getvalue().encode("ascii"))
    public_key = f"{key.get_name()} {key.get_base64()}"
    record = _public_record(public_key)
    try:
        yield private_key, record
    finally:
        private_key[:] = b"\x00" * len(private_key)
        del key


def _exec_command(client, command: str) -> tuple[int, str, str]:
    result = client.exec_command(command)
    if not isinstance(result, tuple) or len(result) != 3:
        raise TypeError("root client exec_command adapter must return (exit, stdout, stderr)")
    code, stdout, stderr = result
    return int(code), str(stdout), str(stderr)


def _authorized_key_command(user: str, uid: int, gid: int, public_key: str, remove: bool) -> str:
    file_path = f"/home/{user}/.ssh/authorized_keys"
    quoted_file = shlex.quote(file_path)
    quoted_line = shlex.quote(public_key)
    if not remove:
        return (
            "set -eu; "
            f"file={quoted_file}; line={quoted_line}; "
            "grep -F -x -q -- \"$line\" \"$file\" || printf '%s\\n' \"$line\" >> \"$file\"; "
            f"chown {int(uid)}:{int(gid)} \"$file\"; chmod 600 \"$file\""
        )
    temp_path = f"/home/{user}/.ssh/.authorized_keys.awt-cleanup"
    quoted_temp = shlex.quote(temp_path)
    return (
        "set -eu; "
        f"file={quoted_file}; tmp={quoted_temp}; line={quoted_line}; "
        "if grep -F -x -q -- \"$line\" \"$file\"; then "
        "umask 077; grep -F -x -v -- \"$line\" \"$file\" > \"$tmp\" || test $? -eq 1; "
        f"chown {int(uid)}:{int(gid)} \"$tmp\"; chmod 600 \"$tmp\"; mv \"$tmp\" \"$file\"; "
        "else printf 'not-found\\n'; fi"
    )


@contextlib.contextmanager
def temporary_authorized_key(
    root_client,
    *,
    user: str,
    uid: int,
    gid: int,
    record: Callable[[dict[str, str]], None] | None = None,
) -> Iterator[dict[str, object]]:
    """Install a generated public key, yield the private PEM, then remove it."""
    with generated_key_lease() as (private_key, public_record):
        public_key = public_record["public_key"]
        install = _authorized_key_command(user, uid, gid, public_key, remove=False)
        code, stdout, stderr = _exec_command(root_client, install)
        if code != 0:
            raise RuntimeError(f"temporary public-key installation failed: {stderr or stdout}")
        installed_record = {**public_record, "authorization_status": "INSTALLED"}
        if record is not None:
            record(installed_record)
        try:
            yield {"private_key": private_key, "public_key": public_key, "record": installed_record}
        finally:
            cleanup = _authorized_key_command(user, uid, gid, public_key, remove=True)
            code, stdout, stderr = _exec_command(root_client, cleanup)
            if code != 0:
                raise RuntimeError(f"temporary public-key cleanup failed: {stderr or stdout}")
            if record is not None:
                record({**installed_record, "authorization_status": "REMOVED"})
