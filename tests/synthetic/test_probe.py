import hashlib
import hmac
import os
import unittest
from unittest.mock import patch

os.environ.setdefault("SYNTHETIC_BASE_URL", "https://synthetic.invalid")
os.environ.setdefault("SYNTHETIC_PROBE_SECRET", "unit-test-secret")
os.environ.setdefault("SYNTHETIC_PUSHGATEWAY_URL", "http://pushgateway.invalid")
os.environ.setdefault("SYNTHETIC_PUSHGATEWAY_TOKEN", "unit-test-push-token")

import probe


class ProbeSigningTests(unittest.TestCase):
    def test_signature_covers_timestamp_and_exact_body(self):
        body = b'{"query":"query SyntheticProbe { __typename }"}'
        headers = probe.signed_headers(body)
        expected = hmac.new(
            probe.PROBE_SECRET,
            headers["X-Synthetic-Probe-Timestamp"].encode() + b"." + body,
            hashlib.sha256,
        ).hexdigest()

        self.assertEqual(headers["X-Synthetic-Probe-Signature"], f"sha256={expected}")

    @patch("probe.urllib.request.urlopen")
    def test_failed_probe_push_does_not_clear_last_success(self, urlopen):
        class Accepted:
            status = 202

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

        urlopen.return_value = Accepted()
        probe.push_metrics({"webhook_callback": False})

        pushed = urlopen.call_args.args[0].data.decode()
        self.assertIn('synapse_synthetic_probe_success{probe_type="synthetic"} 0', pushed)
        self.assertNotIn("last_success_timestamp_seconds", pushed)


if __name__ == "__main__":
    unittest.main()
