#!/bin/sh
# Runs the calendar-sync flow once and echoes its exit code to stdout so it
# is visible in `docker logs` regardless of whether cron or the entrypoint's
# startup run invoked it. See AGENTS.md "Exit Codes":
#   0 success, 10 config error, 11 auth error, 12 runtime error,
#   13 partial failure (some courses failed).
set -u

echo "[u_crawler] $(date -Is) starting: u_crawler calendar"
u_crawler calendar
code=$?
echo "[u_crawler] $(date -Is) u_crawler calendar exited with code ${code}"

exit "${code}"
