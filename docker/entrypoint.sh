#!/bin/sh
# Container entrypoint: runs as root (PID 1) so it can align the `appuser`
# uid/gid with the host before dropping to it, then hands off to cron in the
# foreground. The actual calendar-sync flow always runs as `appuser`, never
# as root.
set -eu

PUID="${PUID:-1000}"
PGID="${PGID:-1000}"
CRON_SCHEDULE="${CRON_SCHEDULE:-0 6 * * *}"
RUN_ON_START="${RUN_ON_START:-true}"

# Re-point appuser at the host uid/gid so files written into the mounted
# caldir volume (and config dir) are owned by someone the host can use --
# a root-owned caldir tree is useless to the host and to caldir running
# elsewhere. Defaults to 1000:1000 if PUID/PGID are not set.
current_uid="$(id -u appuser)"
current_gid="$(id -g appuser)"
if [ "${current_gid}" != "${PGID}" ]; then
    groupmod -o -g "${PGID}" appuser
fi
if [ "${current_uid}" != "${PUID}" ]; then
    usermod -o -u "${PUID}" appuser
fi
chown -R appuser:appuser /home/appuser

# cron(8) strips almost the entire environment before running a job, so the
# job's environment is spelled out explicitly here rather than relied upon
# to inherit ours. HOME matters: it is how `directories::ProjectDirs`
# resolves ~/.config/u_crawler, i.e. where the mounted config.toml must be.
cat > /etc/cron.d/u_crawler <<EOF
SHELL=/bin/sh
PATH=/usr/local/bin:/usr/bin:/bin
HOME=/home/appuser
${CRON_SCHEDULE} appuser /usr/local/bin/run-calendar.sh >>/proc/1/fd/1 2>>/proc/1/fd/2
EOF
chmod 0644 /etc/cron.d/u_crawler

echo "[entrypoint] $(date -Is) u_crawler calendar cron container starting"
echo "[entrypoint] schedule='${CRON_SCHEDULE}' uid=${PUID} gid=${PGID} run_on_start=${RUN_ON_START}"

if [ "${RUN_ON_START}" = "true" ]; then
    echo "[entrypoint] running the flow once now, so a fresh container proves itself without waiting for the next cron tick"
    su -s /bin/sh appuser -c /usr/local/bin/run-calendar.sh || true
fi

exec cron -f
