"""Generate a reusable Linux B SSH key, then install and verify its public key.

Private PEM is saved only in the explicitly selected key directory. Passwords
are entered interactively, never supplied as command arguments or recorded.
Existing key bundles/evidence are not overwritten. Public-key authorization is
append-only and is retained for reuse, including after verification failure.
"""
from __future__ import annotations

import argparse
import contextlib
from datetime import datetime, timezone
import getpass
import hashlib
import io
import json
import os
from pathlib import Path
import stat

import paramiko

HOST = "172.28.8.210"
PORT = 22
USER = "awt1104094b2a0cea"
UID = 1003
GID = 1004
HOME = "/home/" + USER
PRIVATE = "ordinary-user-rsa"
PUBLIC = PRIVATE + ".pub"


def write_new(path, value):
    with Path(path).open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=True, indent=2)
        stream.write("\n")


def generate(directory):
    directory = Path(directory).resolve()
    directory.mkdir(parents=True, exist_ok=False)
    key = paramiko.RSAKey.generate(2048)
    public = f"{key.get_name()} {key.get_base64()}"
    with io.StringIO() as pem:
        key.write_private_key(pem)
        fd = os.open(directory / PRIVATE, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="ascii", newline="\n") as stream:
            stream.write(pem.getvalue())
    with (directory / PUBLIC).open("x", encoding="ascii", newline="\n") as stream:
        stream.write(public + "\n")
    record = {
        "schema": "temporary-ssh-authorization/1",
        "host": HOST, "port": PORT, "user": USER, "uid": UID, "gid": GID,
        "key_type": key.get_name(),
        "public_key_sha256": hashlib.sha256(public.encode("ascii")).hexdigest(),
        "public_key_path": str(directory / PUBLIC),
        "private_key_path": str(directory / PRIVATE),
        "generated_utc": datetime.now(timezone.utc).isoformat(),
        "authorization_status": "NOT_INSTALLED",
        "remote_login_verified": False,
        "authorized_keys_cleanup": "NOT_REQUESTED_REUSABLE_KEY",
    }
    write_new(directory / "authorization.json", record)
    return record


def install_public_key(sftp, public_line):
    """Append exactly this key without rewriting/removing existing entries."""
    if (not public_line.startswith("ssh-rsa ") or len(public_line) > 16384
            or any(c in public_line for c in ("\r", "\n", "\x00"))):
        raise ValueError("invalid_public_key_line")
    raw_line = public_line.encode("ascii")
    directory = HOME + "/.ssh"
    path = directory + "/authorized_keys"
    for target, is_directory in ((directory, True), (path, False)):
        attrs = sftp.lstat(target)
        correct_type = stat.S_ISDIR(attrs.st_mode) if is_directory else stat.S_ISREG(attrs.st_mode)
        if (not correct_type or attrs.st_uid != UID or attrs.st_gid != GID
                or attrs.st_mode & 0o022):
            raise ValueError("authorization_path_identity_mismatch")
    with sftp.open(path, "rb") as stream:
        before = stream.read(1024 * 1024 + 1)
    if len(before) > 1024 * 1024:
        raise ValueError("authorization_file_oversized")
    if raw_line in before.splitlines():
        return "ALREADY_PRESENT"
    addition = (b"\n" if before and not before.endswith(b"\n") else b"") + raw_line + b"\n"
    with sftp.open(path, "ab") as stream:
        stream.write(addition)
        stream.flush()
    with sftp.open(path, "rb") as stream:
        after = stream.read(1024 * 1024 + len(addition) + 1)
    if after != before + addition:
        raise ValueError("authorization_append_readback_mismatch")
    return "APPENDED"


def identity_lines(client, command):
    stdin, stdout, stderr = client.exec_command(command, timeout=15)
    try:
        output = stdout.read(4097)
        error = stderr.read(4097)
        code = stdout.channel.recv_exit_status()
        if code != 0 or len(output) > 4096 or len(error) > 4096 or error:
            raise ValueError("identity_command_failed")
        return output.decode("utf-8").splitlines()
    finally:
        stdin.close()
        stdout.close()
        stderr.close()
        stdout.channel.close()


def authorize(directory, evidence, known_hosts, password):
    directory, evidence = Path(directory).resolve(), Path(evidence).resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    result = {
        "schema": "linux-b-saved-key-authorization/1",
        "host": HOST, "port": PORT, "user": USER,
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "status": "BLOCKED_OR_FAILED", "installation": "NOT_RUN",
        "authorization_status": "NOT_INSTALLED", "remote_login_verified": False,
        "remote_mutation_attempted": False,
        "authorized_keys_cleanup": "NOT_REQUESTED_REUSABLE_KEY",
        "wait_kind": "PARAMIKO_CHANNEL_ONLY", "native_os_wait_completed": False,
    }
    stage = "admin_credential"
    try:
        if not isinstance(password, str) or not password or len(password) > 4096:
            raise ValueError("admin_password_unavailable")
        stage = "key_bundle"
        key = paramiko.RSAKey.from_private_key_file(str(directory / PRIVATE))
        public_line = f"{key.get_name()} {key.get_base64()}"
        record = json.loads((directory / "authorization.json").read_text(encoding="utf-8"))
        expected = {"host": HOST, "port": PORT, "user": USER, "uid": UID, "gid": GID,
                    "public_key_sha256": hashlib.sha256(public_line.encode("ascii")).hexdigest()}
        if any(record.get(name) != value for name, value in expected.items()):
            raise ValueError("key_bundle_binding_mismatch")
        if (directory / PUBLIC).read_text(encoding="ascii").strip() != public_line:
            raise ValueError("public_key_file_mismatch")
        result["public_key_sha256"] = expected["public_key_sha256"]
        result["key_directory"] = str(directory)
        stage = "known_hosts"
        known_hosts = Path(known_hosts).resolve()
        result["known_hosts_sha256"] = hashlib.sha256(known_hosts.read_bytes()).hexdigest()
        stage = "root_authentication"
        with contextlib.closing(paramiko.SSHClient()) as root:
            root.load_host_keys(str(known_hosts))
            root.set_missing_host_key_policy(paramiko.RejectPolicy())
            root.connect(hostname=HOST, port=PORT, username="root", password=password,
                         allow_agent=False, look_for_keys=False,
                         timeout=15, auth_timeout=15, banner_timeout=15)
            password = ""
            stage = "root_identity"
            if identity_lines(root, "id -u") != ["0"]:
                raise ValueError("root_identity_mismatch")
            stage = "public_key_installation"
            result["remote_mutation_attempted"] = True
            result["authorization_status"] = "INSTALLATION_NOT_CONFIRMED"
            with contextlib.closing(root.open_sftp()) as sftp:
                result["installation"] = install_public_key(sftp, public_line)
            result["authorization_status"] = "INSTALLED_NOT_VERIFIED"
        stage = "ordinary_authentication"
        with contextlib.closing(paramiko.SSHClient()) as ordinary:
            ordinary.load_host_keys(str(known_hosts))
            ordinary.set_missing_host_key_policy(paramiko.RejectPolicy())
            ordinary.connect(hostname=HOST, port=PORT, username=USER, pkey=key,
                             allow_agent=False, look_for_keys=False,
                             timeout=15, auth_timeout=15, banner_timeout=15)
            stage = "ordinary_identity"
            command = 'id -u; id -g; id -un; printf "%s\\n" "$HOME"'
            if identity_lines(ordinary, command) != [str(UID), str(GID), USER, HOME]:
                raise ValueError("ordinary_identity_mismatch")
        result.update(status="PASS_ORDINARY_USER_LOGIN", authorization_status="VERIFIED",
                      remote_login_verified=True, uid=UID, gid=GID, home=HOME)
    except Exception as exc:
        result.update(failed_stage=stage, error_type=type(exc).__name__)
    finally:
        password = ""
        result["finished_utc"] = datetime.now(timezone.utc).isoformat()
        write_new(evidence / "result.json", result)
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser("generate", help="Save a new reusable key; never overwrite")
    create.add_argument("--key-dir", required=True)
    install = commands.add_parser("authorize", help="Prompt for root password; append public key and verify ordinary login")
    install.add_argument("--key-dir", required=True)
    install.add_argument("--evidence-dir", required=True)
    install.add_argument("--known-hosts", default=str(Path.home() / ".ssh/known_hosts"))
    args = parser.parse_args(argv)
    try:
        if args.command == "generate":
            result = generate(args.key_dir)
        else:
            password = getpass.getpass("Linux B root password (not recorded): ")
            result = authorize(args.key_dir, args.evidence_dir, args.known_hosts, password)
            password = ""
        print(json.dumps(result, ensure_ascii=True))
        return 0 if args.command == "generate" or result["remote_login_verified"] else 2
    except Exception as exc:
        print(json.dumps({"status": "BLOCKED_OR_FAILED", "error_type": type(exc).__name__}))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
