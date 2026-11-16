#!/bin/bash
set -euo pipefail

QNUM="${NFQUEUE_NUM:-200}"
PID_FILE="/var/run/nfqws2.pid"

cleanup() {
    trap - EXIT INT TERM
    echo "[dpi] Shutting down zapret2..."
    if [[ -f "$PID_FILE" ]]; then
        kill "$(cat "$PID_FILE")" 2>/dev/null || true
        rm -f "$PID_FILE"
    fi
    iptables -t mangle -D OUTPUT -p tcp -m multiport --dports 80,443 -m mark ! --mark 0x40000000/0x40000000 -m connbytes --connbytes-dir=original --connbytes-mode=packets --connbytes 1:12 -j NFQUEUE --queue-num "$QNUM" --queue-bypass 2>/dev/null || true
    iptables -t mangle -D OUTPUT -p udp --dport 443 -m mark ! --mark 0x40000000/0x40000000 -m connbytes --connbytes-dir=original --connbytes-mode=packets --connbytes 1:6 -j NFQUEUE --queue-num "$QNUM" --queue-bypass 2>/dev/null || true
    ip6tables -t mangle -D OUTPUT -p tcp -m multiport --dports 80,443 -m mark ! --mark 0x40000000/0x40000000 -m connbytes --connbytes-dir=original --connbytes-mode=packets --connbytes 1:12 -j NFQUEUE --queue-num "$QNUM" --queue-bypass 2>/dev/null || true
    ip6tables -t mangle -D OUTPUT -p udp --dport 443 -m mark ! --mark 0x40000000/0x40000000 -m connbytes --connbytes-dir=original --connbytes-mode=packets --connbytes 1:6 -j NFQUEUE --queue-num "$QNUM" --queue-bypass 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Ensure legacy iptables is selected
update-alternatives --set iptables /usr/sbin/iptables-legacy >/dev/null 2>&1 || true
update-alternatives --set ip6tables /usr/sbin/ip6tables-legacy >/dev/null 2>&1 || true

# Setup NFQUEUE rules if iptables is available and permitted
if iptables -t mangle -L OUTPUT >/dev/null 2>&1; then
    echo "[dpi] Setting up iptables NFQUEUE rules (queue $QNUM)..."
    iptables -t mangle -A OUTPUT -p tcp -m multiport --dports 80,443 -m mark ! --mark 0x40000000/0x40000000 -m connbytes --connbytes-dir=original --connbytes-mode=packets --connbytes 1:12 -j NFQUEUE --queue-num "$QNUM" --queue-bypass
    iptables -t mangle -A OUTPUT -p udp --dport 443 -m mark ! --mark 0x40000000/0x40000000 -m connbytes --connbytes-dir=original --connbytes-mode=packets --connbytes 1:6 -j NFQUEUE --queue-num "$QNUM" --queue-bypass 2>/dev/null || true
    ip6tables -t mangle -A OUTPUT -p tcp -m multiport --dports 80,443 -m mark ! --mark 0x40000000/0x40000000 -m connbytes --connbytes-dir=original --connbytes-mode=packets --connbytes 1:12 -j NFQUEUE --queue-num "$QNUM" --queue-bypass 2>/dev/null || true
    ip6tables -t mangle -A OUTPUT -p udp --dport 443 -m mark ! --mark 0x40000000/0x40000000 -m connbytes --connbytes-dir=original --connbytes-mode=packets --connbytes 1:6 -j NFQUEUE --queue-num "$QNUM" --queue-bypass 2>/dev/null || true

    echo "[dpi] Starting zapret2 nfqws2..."
    # shellcheck disable=SC2086
    nfqws2 \
        --qnum="$QNUM" \
        --daemon \
        --user=root \
        --pidfile="$PID_FILE" \
        --lua-init=@/opt/zapret2/lua/zapret-lib.lua \
        --lua-init=@/opt/zapret2/lua/zapret-antidpi.lua \
        --blob=tls_google:@/opt/zapret2/fake/tls_clienthello_www_google_com.bin \
        --filter-tcp=443 --filter-l7=tls \
        --payload=tls_client_hello \
        ${NFQWS2_OPT}

    sleep 0.5
    if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
        echo "[dpi] zapret2 active (pid $(cat "$PID_FILE")). DPI bypass enabled."
    else
        echo "[dpi] WARNING: nfqws2 failed to start." >&2
    fi
else
    echo "[dpi] WARNING: iptables is not available or lacks permissions." >&2
    echo "[dpi] Run container with --cap-add=NET_ADMIN to enable DPI bypass." >&2
fi

# Ensure /data and /data/tmp directories exist and belong to app
mkdir -p /data/tmp /cookies /plugins 2>/dev/null || true
chown -R app:app /data /cookies /plugins 2>/dev/null || true

if [ "$#" -eq 0 ]; then
    set -- /usr/local/bin/tg-media-bot
elif [ "${1:0:1}" = "-" ] || [ "$1" = "fetch" ] || [ "$1" = "health" ] || [ "$1" = "healthcheck" ]; then
    set -- /usr/local/bin/tg-media-bot "$@"
fi

# Run the command as user app
gosu app "$@" &
MAIN_PID=$!

forward_term() {
    kill -TERM "$MAIN_PID" 2>/dev/null || true
}
forward_int() {
    kill -INT "$MAIN_PID" 2>/dev/null || true
}
trap forward_term TERM
trap forward_int INT

wait "$MAIN_PID" 2>/dev/null || true
EXIT_CODE=$?
cleanup
exit $EXIT_CODE
