#!/usr/bin/env bash
# Install the VS Code CLI and the tunnel supervisor (SPEC.md §11.1).
#
# Everything here handles one of the four failure modes §21.5 anticipates. None
# of them is handled in `cluster-ctl`: they are properties of the container, and
# a control plane reaching into a container to fix its UID mapping would be a
# control plane that has to know what every image does.
set -euo pipefail

SESSION_ID="${SESSIONID:-}"
NAME_PREFIX="${NAMEPREFIX:-dc-}"
BACKOFF_INITIAL="${BACKOFFINITIALSECONDS:-2}"
BACKOFF_MAX="${BACKOFFMAXSECONDS:-60}"

share=/usr/local/share/cluster-tunnel
install -d -m 0755 "$share"

# --- Pre-bake the CLI, and only the CLI ---------------------------------------
#
# The CLI is small, stable, and safe to bake into this layer: it removes most of
# the cold start. The *server* payload is deliberately not baked. vscode.dev
# pins the server by commit and ships weekly, so a baked server goes stale and is
# re-downloaded regardless --- the layer would grow for nothing and the staleness
# would be invisible. Roughly thirty seconds on first connect after each VS Code
# release is the accepted cost (§21.5).
arch=$(uname -m)
case "$arch" in
  x86_64)  target=cli-alpine-x64  ;;
  aarch64) target=cli-alpine-arm64 ;;
  *) echo "cluster-tunnel: unsupported architecture $arch" >&2; exit 1 ;;
esac
curl --silent --show-error --fail --location \
  "https://code.visualstudio.com/sha/download?build=stable&os=${target}" \
  --output /tmp/vscode-cli.tar.gz
tar -xzf /tmp/vscode-cli.tar.gz -C /usr/local/bin
rm -f /tmp/vscode-cli.tar.gz
chmod 0755 /usr/local/bin/code

# --- UID alignment ------------------------------------------------------------
#
# The CLI writes to ~/.vscode-cli/. The auth volume is shared between sessions,
# and devcontainer images do not agree on the container user's UID --- most are
# 1000, some are not. A mismatch fails authentication *silently*, on that one
# container, which is the worst way for it to fail.
#
# So the directory is namespaced by UID and chowned here, at install, while this
# script still runs as root and the answer is knowable.
container_user="${_REMOTE_USER:-root}"
container_uid=$(id -u "$container_user")
auth_root=/var/lib/cluster-tunnel/auth
install -d -m 0755 "$auth_root"
install -d -m 0700 -o "$container_uid" "$auth_root/$container_uid"

cat > "$share/config.env" <<CONFIG
CLUSTER_TUNNEL_NAME=${NAME_PREFIX}${SESSION_ID}
CLUSTER_TUNNEL_AUTH_DIR=${auth_root}/${container_uid}
CLUSTER_TUNNEL_USER=${container_user}
CLUSTER_TUNNEL_BACKOFF_INITIAL=${BACKOFF_INITIAL}
CLUSTER_TUNNEL_BACKOFF_MAX=${BACKOFF_MAX}
CONFIG

# --- Supervision --------------------------------------------------------------
#
# Devcontainers have no init by default, so a `code tunnel` started from
# postStartCommand has nothing to restart it if it dies mid-session. The
# container runs with --init and this loop re-execs on exit.
cat > "$share/supervise.sh" <<'SUPERVISE'
#!/usr/bin/env bash
# Keep the tunnel registered for as long as the container lives (§11.1).
set -uo pipefail
. /usr/local/share/cluster-tunnel/config.env

export VSCODE_CLI_DATA_DIR="${CLUSTER_TUNNEL_AUTH_DIR}"
backoff="${CLUSTER_TUNNEL_BACKOFF_INITIAL}"

while true; do
  # `--accept-server-license-terms` because there is no operator at a prompt.
  # `--random-name` is deliberately not used: the name is the address, and a
  # random one would change on every restart --- which is the property §14.3
  # depends on not changing.
  /usr/local/bin/code tunnel \
    --accept-server-license-terms \
    --name "${CLUSTER_TUNNEL_NAME}" \
    --no-sleep

  status=$?
  echo "cluster-tunnel: exited ${status}; restarting in ${backoff}s" >&2
  sleep "${backoff}"

  # Exponential, capped. A tunnel failing because its token expired would spin
  # against the identity provider until something rate-limited it, and the
  # rate limit would be shared with every other container on this node.
  backoff=$(( backoff * 2 ))
  if [ "${backoff}" -gt "${CLUSTER_TUNNEL_BACKOFF_MAX}" ]; then
    backoff="${CLUSTER_TUNNEL_BACKOFF_MAX}"
  fi
done
SUPERVISE
chmod 0755 "$share/supervise.sh"

cat > "$share/entrypoint.sh" <<'ENTRY'
#!/usr/bin/env bash
# Start the supervisor in the background and hand control back.
#
# A Feature entrypoint that blocked would stop the container ever finishing its
# start-up, so this backgrounds and returns. The container runs with --init, so
# the supervisor is reaped properly when the container stops.
set -euo pipefail
. /usr/local/share/cluster-tunnel/config.env

if [ -n "${CLUSTER_TUNNEL_NAME:-}" ] && [ "${CLUSTER_TUNNEL_NAME}" != "dc-" ]; then
  setsid /usr/local/share/cluster-tunnel/supervise.sh \
    >/var/log/cluster-tunnel.log 2>&1 &
fi
exec "$@"
ENTRY
chmod 0755 "$share/entrypoint.sh"

# --- Unregistering ------------------------------------------------------------
#
# Tunnel names are globally unique per account, so §15.3's archive step runs
# this. An archive that left the name registered would collide with any session
# later recreated under the same id, and the collision appears as an editor that
# will not connect --- a long way from its cause.
cat > "$share/unregister.sh" <<'UNREG'
#!/usr/bin/env bash
set -uo pipefail
. /usr/local/share/cluster-tunnel/config.env
export VSCODE_CLI_DATA_DIR="${CLUSTER_TUNNEL_AUTH_DIR}"
/usr/local/bin/code tunnel unregister --name "${CLUSTER_TUNNEL_NAME}" || true
UNREG
chmod 0755 "$share/unregister.sh"

echo "cluster-tunnel: installed for ${NAME_PREFIX}${SESSION_ID} (uid ${container_uid})"
