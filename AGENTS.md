# AGENTS.md

Quick-start guidance for AI coding agents working in this repository.

For complete architecture and command details, use [CLAUDE.md](CLAUDE.md) as the primary reference.

## Start Here

1. Read [CLAUDE.md](CLAUDE.md) for architecture, layering boundaries, and command catalog.
2. Read [CONTRIBUTING.md](CONTRIBUTING.md) for local setup, test expectations, and contribution workflow.
3. Use [README.md](README.md) for product overview and public-facing usage context.
4. Check [rules/](rules/) for AST-grep enforced conventions before changing Rust or UI layers.

## Repo Shape

- `crates/`: open-source Rust components. Core server binary is `crates/tabby`.
- `ee/`: enterprise webserver, GraphQL schema/DAO, DB, Next.js UI, email templates.
- `clients/`: editor integrations and shared LSP agent (`clients/tabby-agent`).
- `website/`: docs site.
- `crates/llama-cpp-server/llama.cpp`: git submodule.

## Day-1 Commands

- Initialize submodules: `git submodule update --recursive --init`
- Rust build: `cargo build`
- Default test command: `cargo test -- --skip golden`
- Rust autofix/lint/format: `make fix`
- Workspace install: `pnpm install`
- JS/TS build: `pnpm build`
- JS/TS lint fix: `pnpm lint:fix` (or `make fix-ui`)
- Full-stack dev loop: `make dev`
- Refresh embedded web assets after UI/email changes: `make update-ui`

## Enforced Conventions

These are CI-enforced by AST-grep rules in [rules/](rules/):

- [rules/only-dao-and-policy-can-depend-tabby-db.yml](rules/only-dao-and-policy-can-depend-tabby-db.yml): only `ee/tabby-schema/src/dao.rs` and `ee/tabby-schema/src/policy.rs` may depend on `tabby-db`.
- [rules/use-schema-result.yml](rules/use-schema-result.yml): use `schema::Result` in `ee/tabby-schema` (except allowed files noted in the rule).
- [rules/use-basic-job.yml](rules/use-basic-job.yml): background jobs must use `BasicJob` / `CronJob` helpers.
- [rules/do-not-use-logkit-crate.yml](rules/do-not-use-logkit-crate.yml): avoid `logkit::` outside approved sink usage.
- [rules/do-not-use-next-pages.yml](rules/do-not-use-next-pages.yml): do not use Next.js `pages/`; app-router only.
- [rules/validate-requires-code.yml](rules/validate-requires-code.yml): validators must include `code` and `message`.

## Where Changes Belong

- New REST route: `crates/tabby/src/routes` plus router wiring in `crates/tabby/src/serve.rs`.
- Authenticated or DB-backed server feature: `ee/tabby-webserver` via schema/DAO boundaries.
- New GraphQL field: update `ee/tabby-schema`, implement in `ee/tabby-webserver`, then run `make update-graphql-schema` and UI codegen.
- Inference backend integration: implement `tabby-inference` traits in binding crates and wire service factories.
- Cross-editor behavior: add feature in `clients/tabby-agent`, then bind per editor integration.
- Completion postprocessing: add in `clients/tabby-agent/src/codeCompletion/postprocess/` with tests.

## Practical Guardrails

- Do not bypass schema/DAO boundaries for DB access.
- Avoid broad refactors across `crates/`, `ee/`, and `clients/` in one change unless necessary.
- Prefer scoped tests first (package/crate level), then broader runs.
- Run Rust/UI fixers before finalizing changes when relevant.

## Additional References

- [ee/tabby-webserver/docs/api_spec.md](ee/tabby-webserver/docs/api_spec.md)
- [website/docs/](website/docs/)
- [Makefile](Makefile)
