# Repository Guidelines

## Project Structure & Module Organization

Sykli is an Elixir/BEAM execution engine with polyglot SDKs. Core engine code lives in `core/lib/sykli/`; Elixir tests live in `core/test/`. SDK implementations are under `sdk/{go,rust,typescript,python,elixir}`. Cross-SDK fixtures and schema cases live in `tests/conformance/`, black-box CLI cases in `test/blackbox/`, and agent/eval harnesses in `eval/`. User-facing and design docs are in `docs/`; examples are in `examples/` and `test_projects/`.

## Build, Test, and Development Commands

Run core commands from `core/`:

```bash
mix deps.get          # install Elixir dependencies
mix format            # format Elixir code
mix test              # run core tests, excluding integration/docker/podman tags
mix credo             # lint with project checks
mix escript.build     # build dev binary at core/sykli
```

Repository-level suites:

```bash
tests/conformance/run.sh        # validate SDK JSON output against schema/cases
test/blackbox/run.sh --verbose  # run CLI black-box tests against built binary
eval/oracle/run.sh --case 001   # run a single oracle eval case
```

## Coding Style & Naming Conventions

Use `mix format` for Elixir formatting. Module names use `Sykli.*`; files use snake_case paths matching module names. Tests are named `*_test.exs`. Prefer explicit structured data and existing local helpers over ad hoc parsing. Keep Team Mode boundaries metadata-only: do not sync logs, source, artifacts, contract bytes, or tokens to the coordinator.

## Testing Guidelines

Add focused ExUnit coverage near the changed module in `core/test/`. Use tagged suites for slow or external behavior: `mix test.integration`, `mix test.docker`, and `mix test.podman`. SDK changes should update conformance fixtures and pass `tests/conformance/run.sh`. CLI behavior should be covered by core tests and, when user-visible, black-box cases.

## Commit & Pull Request Guidelines

History uses concise Conventional Commit-style subjects, for example `feat(team): sync run summaries` and `fix(team): unblock executor on remote gate decision`. Before pushing, run `cd core && mix format && mix test && mix credo`; build or conformance tests when relevant. PRs should summarize behavior changes, list verification commands, link issues or follow-ups, and note any user-facing CLI/JSON contract changes.

## Security & Configuration Tips

Never persist or log team tokens. `SYKLI_TEAM_TOKEN` is read from the environment or CLI flags. Local detailed state belongs under `.sykli/`; shared coordinator state must remain scoped metadata. Read `docs/team-mode-security.md` and `docs/local-state-plane.md` before changing coordinator or daemon sync paths.
