#!/bin/sh
set -eu
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
PYTHON=$(command -v python3.12 || true)
if [ -z "$PYTHON" ]; then
  printf '%s\n' 'Python 3.12 is required; no packages were installed.' >&2
  exit 1
fi
exec "$PYTHON" -I -B -X utf8 "$HERE/p1/linux_launch.py" "$@"
