# Sykli for coding agents

For typed artifact production, start with `sykli targets --json` and
`sykli plan sykli.production.json --target TARGET --json`. Use the discovered
target name (`sykli` in this repository), not an assumed `app` alias.
`sykli produce TARGET --prepare --summary --json` captures the request without
executing it. Keep its production ID. A fresh worker can inspect
`sykli status ID --summary --json`, then select ready work with
`sykli resume ID --operation NAME --summary --json` or finish all remaining work
with `sykli resume ID --jobs 2 --summary --json`.
The same local store is sufficient; no chat handover is required. Compact state
omits command captures; retrieve them with `sykli diagnostics ID ATTEMPT --json`.
Read `assessment` and `delivery` separately: an available artifact can still
have unfinished checks, and a failed operation needs an explicit `--retry`.

To learn what a pull request has established before acting on it, run
`sykli inspect --repo OWNER/NAME --pr N --requirements FILE --json` and read the
embedded `sykli-assessment.v1`: each unresolved obligation carries a reason code,
the evidence it would need (`missing`), and source references into the saved
bundle. Replay or explain it offline with `sykli assess BUNDLE --requirements FILE
--why ID`. The result is advisory and never a merge authorization.
