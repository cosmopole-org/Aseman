#!/usr/bin/env bash
# docker-entrypoint.sh — Container startup for aseman-node
#
# Mount a .env file at /app/.env, or pass all variables via `docker run -e`.
# Required env vars: OWNER_ID, OWNER_PRIVATE_KEY, CLIENT_TCP_API_PORT, etc.
#
# This entrypoint also starts a per-container QuestDB instance so each
# caspar-node has its own isolated tsdb. Containers run with --network host,
# so each node uses its own QUESTDB_PORT (8812 / 8912 / 9012) along with
# matching HTTP and ILP ports — see run-nodes.sh for the layout.

set -e

# Source mounted env file if present
if [[ -f /app/.env ]]; then
  set -a
  # shellcheck disable=SC1091
  source /app/.env
  set +a
fi

# Create directories that must exist before the node starts
mkdir -p \
  "${STORAGE_ROOT_PATH:-/app/data/storage}" \
  "${BASE_DB_PATH:-/app/data/db}" \
  "${APPLET_DB_PATH:-/app/data/applet}" \
  "${SEARCH_INDEX_PATH:-/app/data/search}" \
  "${STORE_LOGS_DB:-/app/data/store_logs}" \
  "${TELEMETRY_DB_PATH:-/app/data/telemetry}" \
  "${BABBLE_DIR:-/app/data/babble}" \
  "${QUESTDB_DATA_DIR:-/app/data/questdb}"

# Default BABBLE_DIR if not set (caspar-node requires it)
export BABBLE_DIR="${BABBLE_DIR:-/app/data/babble}"

# ─── Start QuestDB inside this container ────────────────────────────────────
# Every QuestDB listener must be bound to a per-node port — the JVM crashes
# on bind() the moment any default port (8812 / 9000 / 9003 / 9009) collides
# with another container under --network host, so a partial override is not
# enough. The UDP line-protocol listener is disabled outright (caspar-node
# only uses the PG wire).
QDB_PG_PORT="${QUESTDB_PORT:-8812}"
QDB_HTTP_PORT="${QUESTDB_HTTP_PORT:-9000}"
QDB_HTTP_MIN_PORT="${QUESTDB_HTTP_MIN_PORT:-9003}"
QDB_ILP_PORT="${QUESTDB_ILP_PORT:-9009}"
QDB_DATA="${QUESTDB_DATA_DIR:-/app/data/questdb}"
QDB_LOG="/app/data/questdb.log"

if [[ ! -f /opt/questdb/questdb.jar ]]; then
  echo "[entrypoint] FATAL: /opt/questdb/questdb.jar not found — rebuild the image" >&2
  exit 1
fi

echo "[entrypoint] Starting QuestDB (PG=${QDB_PG_PORT} HTTP=${QDB_HTTP_PORT} MIN=${QDB_HTTP_MIN_PORT} ILP=${QDB_ILP_PORT}, data=${QDB_DATA})"
QDB_PG_NET_BIND_TO="0.0.0.0:${QDB_PG_PORT}" \
QDB_HTTP_NET_BIND_TO="0.0.0.0:${QDB_HTTP_PORT}" \
QDB_HTTP_MIN_NET_BIND_TO="0.0.0.0:${QDB_HTTP_MIN_PORT}" \
QDB_LINE_TCP_NET_BIND_TO="0.0.0.0:${QDB_ILP_PORT}" \
QDB_LINE_UDP_ENABLED=false \
java -jar /opt/questdb/questdb.jar -m io.questdb/io.questdb.ServerMain \
     -d "${QDB_DATA}" >> "${QDB_LOG}" 2>&1 &
QDB_PID=$!

# Wait for QuestDB to accept PG connections before launching the node.
# Uses bash's /dev/tcp pseudo-device so no extra dependencies are needed.
for _i in $(seq 1 300); do
  if (echo > "/dev/tcp/127.0.0.1/${QDB_PG_PORT}") >/dev/null 2>&1; then
    echo "[entrypoint] QuestDB ready on port ${QDB_PG_PORT} (pid ${QDB_PID})"
    break
  fi
  if ! kill -0 "${QDB_PID}" 2>/dev/null; then
    echo "[entrypoint] FATAL: QuestDB exited before becoming ready — see ${QDB_LOG}" >&2
    tail -50 "${QDB_LOG}" >&2 || true
    exit 1
  fi
  sleep 1
done

if ! (echo > "/dev/tcp/127.0.0.1/${QDB_PG_PORT}") >/dev/null 2>&1; then
  echo "[entrypoint] FATAL: QuestDB did not become ready within 300s — see ${QDB_LOG}" >&2
  exit 1
fi

# Forward SIGTERM/SIGINT to both children so the container shuts down cleanly.
_term() {
  kill -TERM "${NODE_PID:-0}" 2>/dev/null || true
  kill -TERM "${QDB_PID}" 2>/dev/null || true
}
trap _term TERM INT

# Mirror caspar-node's stdout/stderr to /app/data/node.log AND the container
# stdout (so `docker logs` still works). Logs end up on the host bind mount,
# which means they survive container removal — without that, `stop-nodes.sh`
# wipes the only copy before the report-collection step runs.
NODE_LOG="/app/data/node.log"
: > "${NODE_LOG}" 2>/dev/null || true
echo "[entrypoint] Starting aseman-node (logs → ${NODE_LOG})"
/usr/local/bin/aseman-node "$@" > >(tee -a "${NODE_LOG}") 2> >(tee -a "${NODE_LOG}" >&2) &
NODE_PID=$!
wait "${NODE_PID}"
NODE_EXIT=$?
kill -TERM "${QDB_PID}" 2>/dev/null || true
exit "${NODE_EXIT}"
