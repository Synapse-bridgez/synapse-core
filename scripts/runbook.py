#!/usr/bin/env python3
"""Guided, checkpointed runbook procedures with append-only JSONL audit records."""

import argparse
import datetime as dt
import json
import pathlib
import sys
import uuid


def step(title, instruction, command, destructive=False):
    return {
        "title": title,
        "instruction": instruction,
        "command": command,
        "destructive": destructive,
    }


PROCEDURES = {
    "health": [
        step("Application health", "Confirm health and dependency status.", "curl -fsS {base_url}/health"),
        step("Readiness", "Confirm this instance can accept traffic.", "curl -fsS {base_url}/ready"),
        step("Recent errors", "Review errors; correlate events by request_id or trace_id.", "journalctl -u synapse-core --since '15 minutes ago' -p err"),
    ],
    "high-error-rate": [
        step("Confirm impact", "Identify affected routes, instances, and duration.", "curl -fsS {base_url}/health"),
        step("Correlate logs", "Search centralized JSON logs by trace_id/request_id for the incident window.", "Review the configured log store for level=ERROR"),
        step("Check database pressure", "Inspect active and blocked queries before making changes.", "psql $DATABASE_URL -c \"SELECT pid,state,wait_event_type,now()-query_start AS duration,query FROM pg_stat_activity WHERE state <> 'idle' ORDER BY query_start;\""),
        step("Check dependencies", "Check Redis, database, and Horizon health and circuit-breaker state.", "redis-cli -u $REDIS_URL ping"),
    ],
    "database-failure": [
        step("Test connectivity", "Verify the configured database before restarting anything.", "psql $DATABASE_URL -c 'SELECT 1;'"),
        step("Inspect database logs", "Check database logs and host/resource alarms for the same window.", "docker logs synapse-postgres --tail 100"),
        step("Restart database", "Only if unhealthy, approved, and failover is not safer. This interrupts connections.", "docker compose restart postgres", True),
        step("Verify recovery", "Confirm database connectivity and app readiness recovered.", "psql $DATABASE_URL -c 'SELECT 1;' ; curl -fsS {base_url}/ready"),
    ],
    "pool-exhaustion": [
        step("Confirm saturation", "Verify pool usage remains high for at least two minutes.", "curl -fsS {base_url}/health"),
        step("Find long-running queries", "Identify leaks, blockers, and connection budget before changing limits.", "psql $DATABASE_URL -c \"SELECT pid,now()-query_start AS duration,query FROM pg_stat_activity WHERE state='active' ORDER BY query_start;\""),
        step("Change pool limit", "Only after checking aggregate app connection limits against PostgreSQL max_connections and obtaining approval.", "Update DB_MAX_CONNECTIONS in deployment configuration", True),
        step("Roll out setting", "Use a rolling restart; active requests will reconnect.", "Use the deployment platform's approved rolling restart procedure", True),
        step("Verify recovery", "Monitor pool usage, PostgreSQL max_connections, and error rate.", "curl -fsS {base_url}/health"),
    ],
    "failover": [
        step("Verify replica", "Confirm it is reachable, in recovery, and replication lag is acceptable.", "psql $DATABASE_REPLICA_URL -c 'SELECT pg_is_in_recovery();'"),
        step("Measure lag", "Record replay position/data-loss window and escalate if outside policy.", "Inspect pg_stat_replication and replica replay LSN"),
        step("Drain traffic", "Coordinate a maintenance window and stop writes to prevent split-brain.", "Use the deployment platform's approved traffic-drain procedure", True),
        step("Promote replica", "First ensure the old primary cannot accept writes and obtain incident-commander approval.", "Use the database platform's documented replica-promotion command", True),
        step("Repoint and resume", "Update DATABASE_URL; resume traffic only after write/read verification.", "Use the approved configuration rollout and traffic-resume procedure", True),
        step("Verify service", "Check readiness, database writes, topology, and error rate.", "curl -fsS {base_url}/ready"),
    ],
}


def append_record(path, record):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as log:
        log.write(json.dumps(record, sort_keys=True) + "\n")
        log.flush()


def run(args):
    run_id = str(uuid.uuid4())
    log_path = pathlib.Path(args.log_file)
    procedure = args.procedure

    def record(event, index=None, action=None, destructive=None, confirmation=None):
        append_record(log_path, {
            "timestamp": dt.datetime.now(dt.timezone.utc).isoformat(),
            "run_id": run_id,
            "procedure": procedure,
            "event": event,
            "step": index,
            "action": action,
            "destructive": destructive,
            "operator_confirmation": confirmation,
        })

    record("started")
    print(f"Runbook: {procedure}\nRun ID: {run_id}\nAudit log: {log_path}")
    print("Actions are displayed for the operator; this tool does not execute commands.\n")
    for index, item in enumerate(PROCEDURES[procedure], start=1):
        action = item["command"].replace("{base_url}", args.base_url.rstrip("/"))
        tag = " [DESTRUCTIVE]" if item["destructive"] else ""
        print(f"{index}. {item['title']}{tag}\n   {item['instruction']}\n   Action: {action}")
        if item["destructive"]:
            response = input("   After review and execution, type 'yes' to confirm (anything else stops): ").strip()
            confirmed = response == "yes"
            confirmation = "yes" if confirmed else "declined"
        else:
            response = input("   After completing this check, press Enter to continue (q stops): ").strip()
            confirmed = response.lower() != "q"
            confirmation = "acknowledged" if confirmed else "declined"
        record("confirmed" if confirmed else "aborted", index, action, item["destructive"], confirmation)
        if not confirmed:
            print(f"Stopped at step {index}; action not confirmed.")
            return 0
    record("completed")
    print("Runbook completed.")
    return 0


def main():
    parser = argparse.ArgumentParser(description="Guided Synapse on-call runbook procedures")
    parser.add_argument("procedure", choices=PROCEDURES)
    parser.add_argument("--base-url", default="http://localhost:3000")
    parser.add_argument("--log-file", default="runbook-executions.jsonl")
    return run(parser.parse_args())


if __name__ == "__main__":
    sys.exit(main())
