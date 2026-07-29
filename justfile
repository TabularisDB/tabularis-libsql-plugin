set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Build the plugin binary in debug mode.
build:
    cargo build

# Build for release (what the GitHub Actions workflow ships).
release:
    cargo build --release

# Run unit tests.
test:
    cargo test

# Launch the local REPL that simulates Tabularis JSON-RPC calls over stdio.
repl:
    cargo run --bin test_plugin

# Run clippy with warnings denied.
lint:
    cargo clippy --all-targets -- -D warnings

# Format the codebase.
fmt:
    cargo fmt --all

# ---------------------------------------------------------------------------
# Platform-specific install recipes (plugin-dir conventions per OS).
# ---------------------------------------------------------------------------

[linux]
dev-install: build
    mkdir -p ~/.local/share/tabularis/plugins/libsql
    cp target/debug/libsql-plugin ~/.local/share/tabularis/plugins/libsql/
    cp .tabularium ~/.local/share/tabularis/plugins/libsql/
    @echo "Installed to ~/.local/share/tabularis/plugins/libsql"
    @echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[macos]
dev-install: build
    mkdir -p "$HOME/Library/Application Support/tabularis/plugins/libsql"
    cp target/debug/libsql-plugin "$HOME/Library/Application Support/tabularis/plugins/libsql/"
    cp .tabularium "$HOME/Library/Application Support/tabularis/plugins/libsql/"
    @echo "Installed to ~/Library/Application Support/tabularis/plugins/libsql"
    @echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[windows]
dev-install: build
    $dest = Join-Path $env:APPDATA "tabularis\plugins\libsql"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item "target\debug\libsql-plugin.exe" $dest
    Copy-Item ".tabularium" $dest
    Write-Host "Installed to $dest"
    Write-Host "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[linux]
uninstall:
    rm -rf ~/.local/share/tabularis/plugins/libsql

[macos]
uninstall:
    rm -rf "$HOME/Library/Application Support/tabularis/plugins/libsql"

[windows]
uninstall:
    $dest = Join-Path $env:APPDATA "tabularis\plugins\libsql"
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
