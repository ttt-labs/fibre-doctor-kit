#!/bin/sh
# Run an already installed binary; no download, installation or service mutation.
set -eu
tool=${FIBRE_DOCTOR_KIT_BIN:-${DOCTOR_FIBRE_KIT_BIN:-${TTT_FIBRE_BIN:-./fibre-doctor-kit-linux-x86_64-musl}}}
exec "$tool" collect "$@"
