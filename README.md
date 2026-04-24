# jetrocli

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


```sh
git clone https://github.com/mitghi/jetrocli
cd jetrocli
cargo build --release
```

Binary lands at `target/release/jetrocli`.

## Usage

```sh
jetrocli                            # sample document
jetrocli -i data.json               # load from file
jetrocli -i data.json -e '$.users'  # pre-fill expression
```

## License

MIT
