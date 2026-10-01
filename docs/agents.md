# Agents

FilmCraft is built to be driven by AI agents, and largely built by them. Part 1 covers using
FilmCraft from an agent. Part 2 covers developing FilmCraft as an agent.

# Part 1: driving FilmCraft

## 1. MCP server

`filmcraft-cli mcp` serves MCP on stdio. It has two modes:

| Mode | Command | What it drives |
|---|---|---|
| Headless | `filmcraft-cli mcp --demo` or `--project p.fcproj` (neither = empty project) | an in-process engine session, no window |
| Bridge | `filmcraft-cli mcp --bridge 127.0.0.1:9876` | the running desktop app started with `filmcraft --control 9876` |

**Claude Code.** The repository's `.mcp.json` registers both servers (`filmcraft` = bridge,
`filmcraft-headless` = demo). Both point at `target/release/filmcraft-cli`, so build it first:

```sh
cargo build --release -p filmcraft-cli -p filmcraft
./target/release/filmcraft --control 9876 &      # only for the bridge server
claude                                           # approve the project MCP servers when asked
```

To register the server by hand, or for another MCP client, use the absolute binary path:

```sh
claude mcp add filmcraft-headless -- /abs/path/filmcraft/target/release/filmcraft-cli mcp --demo
```

```json
{ "mcpServers": { "filmcraft": { "command": "/abs/path/filmcraft-cli", "args": ["mcp", "--bridge", "127.0.0.1:9876"] } } }
```

### Tools

| Tool | Modes | Purpose |
|---|---|---|
| `command_list` | both | every command: id, label, menu, shortcut, params, enabled now (`filter`, `enabled_only`) |
| `command_run` | both | run a command `{id, params}`; edits are undoable |
| `project_inspect` | both | bins and items with ids, types, durations; active sequence |
| `sequence_inspect` | both | the active sequence: tracks, clips (ticks and frames), effects, transitions, markers, playhead, selection |
| `media_import` | both | import files by absolute path (`text`, one path per line) |
| `render_frame` | both | PNG of the program frame at `seconds` (headless renders; bridge screenshots the Program monitor) |
| `ui_inspect` | bridge | UI state: tool, workspace, panels, zoom, playback, fps |
| `ui_elements` | bridge | on-screen interactive elements with id, label and rect (`prefix` filter) |
| `ui_click` | bridge | click by `id` or `x`,`y`; `button`, `count`, `modifiers` |
| `ui_drag` | bridge | press–move–release between points or element ids |
| `ui_key` | bridge | key or shortcut: `Space`, `Cmd+K`, `Shift+Delete` |
| `ui_type` | bridge | type text into the focused field |
| `ui_screenshot` | bridge | PNG of the window or one `panel` |
| `ui_control` | bridge | call any control-channel method directly (`ui.set`, `ui.scroll`, `ui.timeline.locate`, …) |

Typical loop: `project_inspect` / `sequence_inspect` → get ids → `command_run` → `render_frame` or
`ui_screenshot` → look at the result → `edit.undo` if it's wrong.

## 2. Control channel

`filmcraft --control 9876` (or `FILMCRAFT_CONTROL_PORT=9876`) listens on `127.0.0.1` only. Send one
JSON request per line and get one JSON reply per line. Full method table:
[control-protocol.md](control-protocol.md).

```jsonc
{"id":1,"method":"engine.commands"}
{"id":2,"method":"engine.execute","params":{"command":"file.openDemoProject"}}
{"id":3,"method":"engine.execute","params":{"command":"playhead.set","params":{"seconds":2}}}
{"id":4,"method":"engine.execute","params":{"command":"sequence.addEdit"}}
{"id":5,"method":"engine.execute","params":{"command":"effects.apply","params":{"effect":"Gaussian Blur"}}}
{"id":6,"method":"ui.elements","params":{"prefix":"tools."}}
{"id":7,"method":"ui.click","params":{"id":"tools.Razor"}}
{"id":8,"method":"ui.key","params":{"key":"Cmd+Z"}}
{"id":9,"method":"ui.screenshot","params":{"path":"/tmp/timeline.png","panel":"Timeline"}}
```

Replies look like `{"id":4,"ok":true,"result":{"cuts":2}}` or `{"id":7,"ok":false,"error":"…"}`.
Minimal client:

```python
import json, socket
s = socket.create_connection(("127.0.0.1", 9876)); f = s.makefile("rw")
def call(method, **params):
    f.write(json.dumps({"id": 1, "method": method, "params": params}) + "\n"); f.flush()
    return json.loads(f.readline())
print(call("engine.execute", command="sequence.inspect"))
```

Notes:

- Time is in ticks (254 016 000 000 per second). Commands also accept `seconds`, `frame` or
  `timecode`.
- `engine.execute` also runs UI-only commands (`tool.razor`, `playback.toggle`,
  `window.workspace.color`, `window.panel.<name>`).
- Element ids come from the previous frame. If an element is missing right after a layout change,
  the app retries on later frames before giving up.
- Without a window, `filmcraft-cli run script.jsonl` (lines of `{"id":"…","params":{…}}`) runs the
  same commands headlessly.

## 3. Verifying UI work

For any UI change, look at the result:

1. `cargo run --release -p filmcraft -- --control 9876`
2. Drive the feature through the control channel or the MCP bridge: commands, then clicks and drags
   by automation id.
3. Assert on `ui.inspect`, `ui.elements` and `sequence.inspect`.
4. Take `ui.screenshot` (the whole window and the panel you changed) and open the PNG.

Screenshots you commit must follow [AGENTS.md](../AGENTS.md) §1: FilmCraft only, openly licensed
media, sidecar and `ATTRIBUTION.md` entry.

# Part 2: developing FilmCraft

## 4. Orientation, in this order

1. [AGENTS.md](../AGENTS.md): absolute rules (assets, clean-room, licences).
2. [CLAUDE.md](../CLAUDE.md): working instructions and non-negotiables.
3. [ROADMAP.md](../ROADMAP.md): milestones, what's done, what's running.
4. [architecture.md](architecture.md), then the README and tests of the crate you'll touch.
5. [contributing.md](contributing.md) (how to add things, gates) and [testing.md](testing.md).

Maintainers also keep a local planning folder, `plan/`. It is gitignored and not in the repository,
and holds the task-level plan, the status file and behaviour reference notes. If you don't have it,
everything you need to contribute is in the public docs above. Ask a maintainer for a task id.

## 5. Autonomous work loop

1. **Orient.** Pick the next task: the next unchecked task in the maintainer status file, or an open
   ROADMAP item. Read the relevant architecture section and crate README.
2. **Plan tests first.** Write down the acceptance test before writing code.
3. **Implement and test.**
4. **Verify.** Run all gates (`cargo xtask ci`). For UI work, run the app with `--control`, drive it
   and look at the screenshots (§3).
5. **Record.** Update the crate README (behaviour decisions, test results, limitations) and
   ROADMAP.md when a milestone moves. Commit as `M<n>.<k>: …`, then move on to the next task.

For parallel agents, use one git worktree and one `CARGO_TARGET_DIR` per agent, and keep each crate
with one owner ([contributing.md §5](contributing.md#5-parallel-work-several-agents-or-worktrees)).

## 6. Decision rules

Don't block on choices you can make yourself:

- **Behaviour.** Match the publicly documented and observable behaviour of professional editors
  (help pages, published shortcut lists, using the app). Never inspect application internals
  ([AGENTS.md](../AGENTS.md) §2). If behaviour is undocumented, choose what editors most commonly
  expect and record the choice in the crate README.
- **Ask a human only about:**
  - licences outside the allowlist;
  - spending money;
  - publishing or pushing;
  - personal data;
  - destructive git operations;
  - anything AGENTS.md leaves unclear ("when in doubt, leave it out").

## 7. Definition of Done

A feature is done when:

- [ ] its command(s) are registered with label, menu path, shortcut (if any) and a parameter doc;
- [ ] engine tests cover it, including undo/redo and the disabled case;
- [ ] the UI is wired: menu, panel control and shortcut;
- [ ] every new interactive widget has an automation id;
- [ ] you have driven it through the control channel and reviewed a screenshot;
- [ ] all gates pass (`cargo xtask ci`), and new assets have sidecars;
- [ ] the crate README and ROADMAP.md are updated where relevant;
- [ ] it is committed with its task id.
