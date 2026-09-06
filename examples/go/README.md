# Go, with a worker change

Install Sykli from this checkout and Go, then enter this directory. No module
downloads are needed for this example.

```sh
sykli init --production --smoke 'test "$("$SYKLI_INPUT_executable")" = 42'
sykli targets
sykli plan sykli.production.json --target app
sykli produce app --stop-after build
```

The last command exits 1: the executable exists, but checks remain. Copy the
printed production ID and open a fresh terminal in this directory:

```sh
sykli status PRODUCTION_ID
sykli resume PRODUCTION_ID
sykli verify-production PRODUCTION_ID
```

Run the printed artifact path: it prints `42`. Add `--json` to these commands
for machine-readable responses.

Now change `return 42` to `return 43` in `main.go`, leaving the tests unchanged,
and run `sykli produce app` again. The new production fails its checks. Inspect
the original ID: its captured source, executable and passing checks remain.
