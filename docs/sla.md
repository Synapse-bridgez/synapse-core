# Tenant Service Level Agreement (SLA)

This document defines the formal, tenant-facing reliability commitments for the
platform, the methodology used to measure each commitment, and how compliance is
tracked and reported. It is the externally-published counterpart to the internal
SLO work (issue 74): wherever a commitment overlaps with an internal SLO, both are
derived from the **same underlying metrics** so that internal and external
reliability numbers stay consistent rather than being two independently-computed
sources of truth.

## 1. Commitments

| # | Commitment | Target | Measurement window |
|---|------------|--------|--------------------|
| SLA-1 | Monthly uptime | 99.9% of eligible minutes | Calendar month |
| SLA-2 | Support first response (P1) | 1 hour | Per incident |
| SLA-3 | Support first response (P2) | 8 business hours | Per incident |
| SLA-4 | Data durability | 99.999999% (11 nines) of stored objects | Rolling 30 days |

### SLA-1 — Monthly uptime

Uptime is the fraction of *eligible minutes* in the calendar month during which
the service was available. A minute is **unavailable** when the availability
probe records a failed request for that minute. Eligible minutes exclude
**planned maintenance windows** (see §3).

```
uptime = (eligible_minutes - unavailable_minutes) / eligible_minutes
```

### SLA-2 / SLA-3 — Support response time

First response time is measured from the moment a support ticket is created to
the first human (non-automated) reply. P1 tickets target a 1-hour first response;
P2 tickets target 8 business hours. Business hours are 09:00–18:00 in the
tenant's configured timezone, Monday–Friday, excluding public holidays.

### SLA-4 — Data durability

Durability is the fraction of stored objects that remain retrievable over a
rolling 30-day window, derived from the same backup-verification metrics used by
the internal durability SLO. A single lost or unverifiable object counts against
the window.

## 2. Measurement methodology

All commitments are computed from the shared metrics source (`src/metrics.rs`),
the same source consumed by the internal SLO tracking. The compliance job
(`src/services/scheduler.rs`) runs on a recurring schedule, reads the relevant
metric series for the requested tenant and period, and produces a compliance
report. Because both internal and external numbers read the same series, a
change in the underlying measurement is reflected consistently in both.

## 3. Planned maintenance windows

Planned maintenance is **excluded** from SLA-1 uptime calculations, provided the
window is:

1. registered as a maintenance window in the metrics source, and
2. announced to affected tenants at least **72 hours** in advance.

Maintenance that is not registered, or that is registered with less than 72
hours' notice, is **counted as downtime** and reduces measured uptime. This rule
is applied uniformly by the compliance calculation so that the treatment of
maintenance is deterministic and auditable.

## 4. Compliance reporting

For any tenant and period, the compliance job produces a report containing, per
commitment:

- the target,
- the measured value,
- the number of eligible minutes / incidents / objects in scope,
- whether the commitment was met, and
- the excluded maintenance minutes (for SLA-1).

Reports are tracking-and-reporting only. Credit or penalty payouts for SLA
breaches are **out of scope** and handled by a separate business process.

## 5. Related documents

- `docs/quota-configuration.md` — tenant quota and usage configuration.
- Internal SLO definitions (issue 74) — the internal counterpart to these
  commitments.
