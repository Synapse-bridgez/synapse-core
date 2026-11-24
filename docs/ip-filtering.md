# IP filtering

`IpFilterLayer` (`src/middleware/ip_filter.rs`) guards the callback and webhook routes using the `ALLOWED_IPS` setting.

- `ALLOWED_IPS=*` allows every client (bypass mode).
- Otherwise it is a comma-separated list of CIDR ranges, IPv4 and IPv6 alike (for example `203.0.113.0/24,2001:db8::/32`). This is allowlist mode.

The client address is parsed into a `std::net::IpAddr` (from the trusted `X-Forwarded-For` hop, or the connection address) and compared with each range, so IPv4 and IPv6 clients are matched against IPv4 and IPv6 rules correctly. In allowlist mode a client that matches no range, including an IPv6 client when only IPv4 ranges are listed, gets `403 Forbidden`; so does a request whose address cannot be determined. To admit an IPv6 client, list its address (`2001:db8::1/128`) or prefix explicitly.
