# Swap clock-skew remediation

`ClockGuard` (`src/swap/clock.rs`) compares local time with a trusted source and
rejects time-sensitive swap operations when skew exceeds `SWAP_CLOCK_MAX_SKEW_MS`
(default 1000). Rejections log `skew_ms` and `max_skew_ms`.

1. Confirm skew: `chronyc tracking` (or `timedatectl timesync-status`) on the node.
2. Fix NTP on the host/node (e.g. restart `chronyd`, check upstream reachability); do not set time by hand in containers.
3. Skew is re-measured each probe interval; readiness recovers automatically once within bounds.
4. If the time source itself is unreachable the last known skew is kept; check egress to the source.
