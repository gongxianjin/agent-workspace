"""Feed the framed AW-GATEWAY-SECRETS/1 (SSH key + model token) on stdout.

Key comes from the pinned stage key file; token comes from the machine's
Claude configuration env (ANTHROPIC_AUTH_TOKEN). Both are read into memory,
framed, written to stdout, and never logged, stored, or echoed.
"""
import os
import sys

MAGIC = b"AW-GATEWAY-SECRETS/1\n"
KEY_PATH = os.environ.get(
    "AO_CLOSURE_KEY",
    r"C:\Users\61769\Documents\project\vending-middle-trade-platform\docs\v1.0\agent-workspace\dist\linux-owner-ubuntu-221-20261111-w1-1f7af1aff0\key\ordinary-user-rsa",
)


def main() -> int:
    key = open(KEY_PATH, "rb").read()
    token = os.environ["ANTHROPIC_AUTH_TOKEN"].encode("utf-8")
    if not 0 < len(key) <= 65536 or not 0 < len(token) <= 16384:
        return 2
    frame = MAGIC + len(key).to_bytes(4, "big") + key + len(token).to_bytes(4, "big") + token
    sys.stdout.buffer.write(frame)
    sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
