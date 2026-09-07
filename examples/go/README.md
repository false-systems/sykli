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

To reproduce this with separate agent invocations and save every command and
result, run from the repository root:

```sh
python3 examples/production/demo.py --language go --binary "$(command -v sykli)" --output /tmp/sykli-go-demo.json
```

The script works in a temporary directory. It prepares the request, runs only
the build, resumes both checks with fresh clients, retrieves their bound
diagnostics, and proves changed source cannot inherit success.
