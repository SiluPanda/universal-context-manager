# Security Policy

Universal Context Manager is a local-first validation project and has not received an independent security audit.

## Reporting

Please open a private GitHub security advisory rather than a public issue. Do not include real credentials, private context databases, or conversation data in a report.

## Security boundaries

- The per-user daemon is the only database writer.
- Local clients communicate over a user-only Unix domain socket. Newly created data directories
  are mode `0700`; the database, socket, spool files, and app/CLI export files are mode `0600` on
  Unix platforms.
- The MCP adapter intentionally fails open so a context failure does not block the host harness.
- Credential-shaped content is rejected from automated memory writes.
- Desktop project, import, and export paths are authorized through canonical, one-time,
  operation-bound native-dialog grants.
- No remote listener, cloud sync, analytics, or model API is enabled in the validation MVP.

The database remains readable to software running as the same macOS user. Use FileVault and normal macOS account isolation; this release does not implement application-level database encryption.

Adapters may include composed context in requests sent by their host harness to that harness's
configured model provider. Local UCM persistence does not make third-party model inference local.
