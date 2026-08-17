# Universal Context Manager

Repository-level instructions for adapter and documentation work:

- treat `adapters/shared/` as canonical for the shared skill and launcher scripts
- after editing shared assets, run `./scripts/sync-shared-assets.sh` and commit the copied plugin-root files too
- run `./scripts/validate-adapters.sh` after changes to adapters, plugin fixtures, marketplace fixtures, or docs that describe the install flow
- keep hook behavior fail-open and keep README/docs honest about the current MVP boundary
- accurately distinguish the implemented local persistence/MCP contracts from unpublished, unsigned release artifacts
