#!/bin/sh
# Import a GGUF (link mode: nothing is copied) and serve it.
#   COLI_GGUF   path of the mounted .gguf (required unless COLI_MODEL_DIR is a ready model dir)
#   COLI_PORT   default 8000
# Extra arguments are passed to `coli serve`.
set -eu
cd /app
if [ -n "${COLI_GGUF:-}" ]; then
	MODEL_DIR=/tmp/model.colibri
	python3 /app/coli import "$COLI_GGUF" --out "$MODEL_DIR"
elif [ -n "${COLI_MODEL_DIR:-}" ]; then
	MODEL_DIR=$COLI_MODEL_DIR
else
	echo "set COLI_GGUF=<mounted .gguf> or COLI_MODEL_DIR=<model dir>" >&2
	exit 2
fi
exec python3 /app/coli serve --model "$MODEL_DIR" --host 0.0.0.0 --port "${COLI_PORT:-8000}" "$@"
