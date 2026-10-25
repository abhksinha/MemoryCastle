# Clients authenticate with static bearer tokens
Author: fixture+claude | Date: 2026-01-08
Files: `src/auth.rs`

## Decision
The server checks a bearer token against a hashed list in the config (`Auth::check`); tokens are rotated by editing the list.

## Rejected
- mTLS: certificate management was too heavy for the small deployments. No auth: unacceptable on shared networks.
