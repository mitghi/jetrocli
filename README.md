# jetrocli

<p align="center">
  <img src="media/jetrocli.png" alt="jetrocli">
</p>

Interactive split-pane TUI for [jetro](https://github.com/mitghi/jetro) — paste JSON, write expressions, see results live. Built on `ratatui` + `crossterm`.

## Features

- **Live evaluation** — expression re-runs on every keystroke.
- **Syntax-highlighted JSON result** with pretty-print (strings, numbers, keys, booleans, null distinct).
- **Structural folding** in JSON editor. Fold any `{…}` / `[…]` block, with gutter triangles (`▾` / `▸`) and inline `⋯ N lines` markers.
- **Schema-aware completion** — suggests fields at the current path, auto-unwraps element fields inside array chains, filters builtins by receiver type.
- **Inline docs pane** next to completions — every jetro builtin ships with signature, summary, and example.
- **Emacs-style bindings** throughout (`C-a/C-e`, `C-f/C-b`, `M-f/M-b`, `C-n/C-p`, `C-g`, `C-c` prefix chord).
- **Expression formatter** — breaks long jetro chains onto indented lines (`C-c C-f`).

## Install

### Homebrew (macOS / Linux)

```sh
brew tap mitghi/jetrocli https://github.com/mitghi/homebrew-jetrocli
brew install jetrocli
```

### Shell installer

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/mitghi/jetrocli/releases/latest/download/jetrocli-installer.sh | sh
```

### From source

```sh
git clone https://github.com/mitghi/jetrocli
cd jetrocli
cargo build --release
```

Binary lands at `target/release/jetrocli`.

## Usage

### Interactive TUI

Default when stdin is a TTY.

```sh
jetrocli                            # sample document
jetrocli -i data.json               # load from file
jetrocli -i data.json -e '$.users'  # pre-fill expression
```

### Pipe / batch mode

When stdin is piped or redirected, TUI is skipped — jetrocli evaluates the expression against stdin and prints the result with `jq`-style colorized JSON (ANSI dropped when stdout not a TTY; respects `NO_COLOR` and `JETROCLI_COLOR=never`).

```sh
echo '{"users":[{"name":"a"},{"name":"b"}]}' | jetrocli '$.users.name'
curl -s api.example.com/data | jetrocli '$.items.first()'
```

With no expression (or empty string), jetrocli just pretty-prints stdin as JSON, like `jq` with no filter:

```sh
cat data.json | jetrocli
echo '{"a":1,"b":[2,3]}' | jetrocli ''
```

When stdin is backed by a regular file (`jetrocli EXPR < big.json`), input is `mmap`'d instead of streamed — zero-copy load for large documents. Real pipes fall back to a buffered read.

## License

MIT
