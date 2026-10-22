"""Reference verifier for Synapse outbound webhook signatures (#1520).

Header: X-Synapse-Signature: t=<unix>,id=<delivery id>,v1=<hex>[,v1=<hex>]
Signed string: f"{t}.{id}." + raw request body bytes (do not re-serialize).
"""
import hashlib
import hmac
import time

TOLERANCE_SECS = 300


def verify(secrets, header, raw_body: bytes, now=None, tolerance=TOLERANCE_SECS) -> bool:
    parts = [p.split("=", 1) for p in header.split(",") if "=" in p]
    ts = next((v for k, v in parts if k == "t"), None)
    did = next((v for k, v in parts if k == "id"), None)
    sigs = [v for k, v in parts if k == "v1"]
    if ts is None or did is None or not sigs or not ts.lstrip("-").isdigit():
        return False
    if abs((time.time() if now is None else now) - int(ts)) > tolerance:
        return False
    msg = f"{ts}.{did}.".encode() + raw_body
    for secret in secrets:
        expected = hmac.new(secret.encode(), msg, hashlib.sha256).hexdigest()
        if any(hmac.compare_digest(expected, s) for s in sigs):
            return True
    return False
