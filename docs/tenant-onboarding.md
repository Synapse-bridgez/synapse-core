# Tenant Onboarding Workflow

This document describes the governed, self-service tenant onboarding workflow
that complements the tenant data model in `src/tenant/mod.rs` and
`migrations/20260430000000_create_tenants.sql`.

## Workflow Stages

1. **Request** — A prospective tenant (or internal requester) submits a tenant
   creation request containing: organization name, requested tenant slug,
   contact/owner email, and intended use case.
2. **Approval** — A designated approver (admin role) reviews the request
   against provisioning criteria (uniqueness of slug, compliance checks,
   capacity) and approves or rejects it. This step is mandatory — tenant
   creation is never auto-approved.
3. **Provisioning** — On approval, the tenant record is created via the
   existing tenant creation path, RLS policies are applied per-tenant, and
   tenant secrets are generated/rotated per the existing secrets workflow.
4. **Audit Trail** — Every request, approval/rejection decision (with
   approver identity and timestamp), and provisioning action is recorded for
   audit purposes.

## Status States

`requested -> approved -> provisioned`
`requested -> rejected`

## Notes

- This is a design/process document. Implementation (API endpoints, approval
  queue storage, and provisioning automation) is tracked as follow-up work
  and should build on the existing tenant model and RLS-based isolation.
- Tenant secret generation must follow the existing tenant secrets handling
  conventions already established in this codebase.
