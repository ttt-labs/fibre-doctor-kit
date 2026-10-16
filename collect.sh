#!/bin/sh
# Run an already installed binary; no download, installation or service mutation.
set -eu
tool=${TTT_FIBRE_BIN:-./ttt-fibre}
exec "$tool" collect "$@"
