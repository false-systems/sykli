# Installing sykli

One static binary, no service, no account. Every path below yields the same
`sykli`; pick by where it runs.

Release assets are named `sykli-<tag>-<platform>-<arch>.tar.gz` with
`platform` in `linux`, `macos` and `arch` in `x86_64`, `aarch64`, plus a
`SHA256SUMS` file and a Homebrew formula `sykli.rb`.

## Installer script

```bash
curl -fsSLO https://raw.githubusercontent.com/false-systems/sykli/main/install.sh
sh install.sh v0.6.0
```

`install.sh` takes exactly one argument, a tag of the form `vX.Y.Z`. It
detects the platform and architecture, downloads that tarball and
`SHA256SUMS` from the GitHub release, refuses to install unless the checksum
matches, and places the binary at `~/.local/bin/sykli`. Set
`SYKLI_INSTALL_DIR` to install somewhere else; set `SYKLI_REPOSITORY` to
install from a fork. Make sure the install directory is on your `PATH`.

## Windows

Releases include `sykli-vX.Y.Z-windows-x86_64.zip` holding `sykli.exe`, listed
in `SHA256SUMS`. In PowerShell:

```powershell
$tag = "v0.6.0"
Invoke-WebRequest "https://github.com/false-systems/sykli/releases/download/$tag/sykli-$tag-windows-x86_64.zip" -OutFile sykli.zip
Expand-Archive sykli.zip -DestinationPath "$env:LOCALAPPDATA\sykli"
```

Add that directory to `PATH`. In Git Bash the installer script above also works
and places `sykli.exe` in `~/.local/bin`. Graph tasks run under a POSIX `sh`
found on `PATH`; Git for Windows provides one. Typed production (`targets`,
`produce`, `status`, `resume`, `diagnostics`, `verify-production`) is not
included in the Windows binary.

## Cargo

```bash
cargo install --git https://github.com/false-systems/sykli --tag v0.6.0 --locked sykli
```

Builds from source with the pinned lockfile; needs Rust 1.85 or newer.

## Homebrew

Every release attaches `sykli.rb`. Once the tap exists it is:

```bash
brew tap false-systems/tap
brew install sykli
```

Creating the tap is one repository: `false-systems/homebrew-tap` with the
release's `sykli.rb` at `Formula/sykli.rb`. Until then, the formula installs
directly: `brew install ./sykli.rb`.

## GitHub Actions

```yaml
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0
      - uses: false-systems/sykli@v0.6.0
        with:
          contract: sykli.json
```

The Action installs the release matching the ref it is called with, runs the
graph, verifies the receipt against the tree, and attaches the receipt as an
artifact. Inputs: `contract`, `version`, `working-directory`, `plan`,
`verify`, `upload-receipt`, `artifact-name`. Outputs: `receipt`, `outcome`,
`tree-oid`, `contract-hash`, `affected`, `verify-code`. Details and the
verify exit codes are in [github-actions.md](github-actions.md).

## Verifying an install

```bash
sykli --version
sykli validate sykli.json --json
```

The second line prints a `sykli-validate.v1` verdict for the contract in the
current directory; exit 1 means the contract is invalid, and the `errors`
array says why.
