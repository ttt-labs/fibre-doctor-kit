#!/bin/sh
# Run an already installed binary; no download, installation or service mutation.
set -eu
tool=${DOCTOR_FIBRE_KIT_BIN:-${TTT_FIBRE_BIN:-./doctor-fibre-kit}}
exec "$tool" collect "$@"
