# Security Advisory Template

Use this template for security advisories affecting the independently
versioned, third-party-consumed components: `synapse-sdk` (`sdks/rust/`) and
`synapse-cli` (`cli/synapse-cli/`). Paste it into a **draft** GitHub Security
Advisory (Security tab, "New draft security advisory") and follow the
publishing checklist in [`SECURITY.md`](../../SECURITY.md#publishing-security-advisories).

Everything above the "Details" heading is safe to publish on release day.
Keep "Details" high-level until the embargo in the checklist has passed.

---

## `<component>`: `<one-line summary of the impact, not the technique>`

| Field | Value |
|---|---|
| Advisory ID | `GHSA-xxxx-xxxx-xxxx` (assigned by GitHub) |
| CVE | `CVE-YYYY-NNNNN` or `requested` or `none` |
| Component | `synapse-sdk` / `synapse-cli` |
| Affected versions | e.g. `>= 0.1.0, < 0.2.1` (every released version that contains the flaw) |
| Fixed version | e.g. `0.2.1` (and any backport, such as `0.1.4`) |
| Severity | Critical / High / Medium / Low, with CVSS v3.1 vector, e.g. `CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:N/A:N` |
| CWE | e.g. `CWE-522: Insufficiently Protected Credentials` |
| Reported | YYYY-MM-DD, by `<reporter, or "internal">` |
| Published | YYYY-MM-DD |

### Impact

Who is affected and what an attacker could do, in terms an integrator can act
on. For example: "Applications that call `AdminSynapseClient::new` with a
proxy URL send the admin key to the proxy in clear text."

### Patches

Upgrade to `<fixed version>`:

```toml
synapse-sdk = "<fixed version>"
```

`synapse-cli`: `cargo install synapse-cli --version <fixed version>`

Changelog entry: `sdks/rust/CHANGELOG.md` / `cli/synapse-cli/CHANGELOG.md`,
section `[<fixed version>]` → `### Security`.

### Workarounds

What to do if an immediate upgrade is impossible (configuration change,
feature flag, API to avoid), or "None; upgrading is the only fix."

### Details

Root cause and the fix, at the level of detail the embargo allows. Link the fix
commit or PR only once the embargo has passed.

### Credits

`<reporter>` for responsible disclosure.

### Timeline

| Date | Event |
|---|---|
| YYYY-MM-DD | Reported |
| YYYY-MM-DD | Confirmed, draft advisory opened |
| YYYY-MM-DD | Fixed version released |
| YYYY-MM-DD | Advisory published |
| YYYY-MM-DD | Full details published (end of embargo) |
