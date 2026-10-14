# sitty

**A terminal built to host AI harnesses — and to be hacked by them.**

sitty (si + tty, say "city") is an open-source, GPU-accelerated macOS terminal where any harness — Claude Code, Codex, Aider, your own script — can open panes and tabs to show its work, report status, read what others are doing, and talk to the other harnesses in its workspace. You see who's running what, and you can step in at any time. Sessions live in a background daemon, so closing the window never kills your agents.

> **Status:** early development. Nothing to install yet.

## Shape

- `sittyd` — Rust daemon: owns PTYs and terminal state, sessions survive the UI.
- `sitty.app` — native Swift/AppKit window with a Metal renderer.
- `sitty` — CLI + MCP adapter over a JSON-RPC Unix-socket API that any harness can call.
- Config in [KDL](https://kdl.dev), hot-reloaded without ever killing a pane.

Website: [sitty.si](https://sitty.si)

## Try it (headless, from source)

```sh
cargo build --workspace
alias sitty="$PWD/target/debug/sitty"

sitty --human workspace open demo --dir ~/code/demo      # → {"workspace": "w1"}
sitty --human tab open --workspace w1 -- /bin/zsh         # → {"tab": "t2", "pane": "p3"}
sitty --human pane send p3 --text 'make test' --key enter
sitty pane wait p3 --quiet 1000 --human
sitty pane read p3
sitty roster
```

The daemon (`sittyd`) starts on first use and keeps sessions alive after every client exits. Inside a sitty pane, `sitty` acts as that pane's harness automatically (via `$SITTY_TOKEN`).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
