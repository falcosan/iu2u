<p align="center">
  <img src="assets/iu2u.svg" width="220" alt="iu2u: a man in a suit pointing at you">
</p>

<h1 align="center">iu2u</h1>

<p align="center">
  <em>Your agent has confidence. Give it judgment.</em>
</p>

<p align="center">
  <a href="https://github.com/falcosan/iu2u/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/falcosan/iu2u/ci.yml?branch=main&style=flat-square&label=ci&labelColor=1A100A" alt="CI status"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-FBF5BE?style=flat-square&labelColor=1A100A" alt="Apache-2.0 license"></a>
</p>

<p align="center">
  <img src="assets/hero.svg" width="880" alt="iu2u examples: guard blocks an irreversible command and asks before a remote action; proof requests a reported check; filter selects failure logs.">
</p>

<p align="center">
  <strong>Decision-model powers for coding agents.</strong><br>
  <sub>Local or hosted. Your model. Your permission rules.</sub>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#hooks">Hooks</a> ·
  <a href="#tools">Tools</a> ·
  <a href="#command-line">Command line</a> ·
  <a href="#settings">Settings</a> ·
  <a href="#how-the-guard-decides">Guard behavior</a>
</p>

---

You ask for a small fix. Your agent reaches for `rm -rf`, lines up a push to `main`, and announces that everything works.

You were hoping for a test.

**iu2u gives your agent a second opinion.** A decision model checks shell commands before they run, looks for evidence in completion reports, and helps sift through output. The same model can rank a backlog, search code by meaning, or judge hundreds of items against the same questions.

Five hooks. Four tools. Two shell commands. And one deliberate limit: **the guard never grants permission.** It can block, ask, or leave your existing permission rules in charge.

_iu2u. It's up to you._

## Install

You need **Rust 1.88+**, **Claude Code**, and a **decision-model server**. The server can run locally or be hosted; it must implement `POST /v1/systemone`. Bring its base URL and model name, or use an [adapter](#adapters) for Apple Intelligence or Claude Code. iu2u does not bundle a model.

**1. Install the binary.**

```sh
cargo install --locked iu2u
```

Make sure `iu2u` is on the `PATH` Claude Code sees.

**2. Add the plugin.**

Replace `YOUR_MODEL` with the model name on your server. Change the URL if the server runs elsewhere.

```sh
claude plugin marketplace add falcosan/iu2u
claude plugin install iu2u@iu2u \
  --config model="YOUR_MODEL" \
  --config base_url="http://127.0.0.1:11435"
```

**3. Configure it inside Claude Code.**

```text
/plugin configure iu2u@iu2u
```

Use this to set a hosted server's API key without putting it in shell history, or to change the model, URL, and hook switches later. You can also set switches during installation with options such as `--config trim=true`.

**`guard` and `proof` start on.** The other hooks are opt-in. All four MCP tools are available to the agent.

Use Claude Code **v2.1.295+** for blocking when the guard cannot launch. Shell commands started through Monitor need **v2.1.314+**.

## Hooks

The agent does not have to remember to call these.

| Hook      | Steps in               | What changes                                                                                                            | Default |
| --------- | ---------------------- | ----------------------------------------------------------------------------------------------------------------------- | ------- |
| `guard`   | Before a shell command | Flags irreversible or risky actions. Blocks or asks; never auto-approves.                                               | On      |
| `proof`   | When an agent finishes | Sends unsupported completion claims back for reported checks, or an explanation of what could not be verified.          | On      |
| `trim`    | After a shell command  | Shortens very long output, preserves blocks with common error and warning markers, and saves the full output to a file. | Off     |
| `compass` | On a prompt            | Suggests a relevant personal or project skill.                                                                          | Off     |
| `prune`   | Before compaction      | Advises which steps to preserve in detail and which to drop.                                                            | Off     |

`proof` checks what the agent **reports**. It does not run tests itself or establish that a reported result is true.

When enabling `trim`, avoid combining it with another hook that redacts command output: only one rewrite is kept.

## Tools

One question, a whole pile of things to ask it about. Available over MCP.

| Tool    | What it does                                                                | Try asking your agent                             |
| ------- | --------------------------------------------------------------------------- | ------------------------------------------------- |
| `weigh` | Applies the same typed questions to up to **500 items**.                    | “Which of these issues describe a regression?”    |
| `rank`  | Orders items by a criterion, highest first.                                 | “Rank these bugs by likely user impact.”          |
| `seek`  | Searches files by meaning and returns file and line ranges.                 | “Find the code that handles expired sessions.”    |
| `lint`  | Checks files against rules in plain language and locates likely violations. | “Find places where an error is silently ignored.” |

## Command line

The same judgment works in a pipe. Configure the server for your shell first:

```sh
export IU2U_MODEL="YOUR_MODEL"
export IU2U_BASE_URL="http://127.0.0.1:11435"
```

Set `IU2U_API_KEY` if your server requires authentication.

### Find the failure, however it was worded

```sh
tail -f app.log | iu2u filter "This line reports a failure"
```

`filter` keeps lines whose probability meets the threshold: **0.5** by default. Raise it with `--at`:

```sh
iu2u filter "This line reports a failure" --at 0.8 < app.log
```

Matching lines stream out in input order. Exit codes follow grep: **0** for a match, **1** for no matches, **2** for an error.

### Drive a browser or a Mac app

`act` runs a script, one sentence per line. It drives a real browser or a native Mac app toward each goal, then checks what the screen shows.

```text
# tests/checkout.e2e
open http://localhost:3000
with email = qa@example.com
with password = $SHOP_PASSWORD
do sign in
do add the cheapest red shirt to the cart
expect the cart shows one red shirt
see "Total"
```

```sh
iu2u act tests/*.e2e
```

| Line                                                | What it does                                                                                                                |
| --------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `open <url>`                                        | Opens the page in Chrome, Chromium, Edge or Brave.                                                                          |
| `app <name>`                                        | Opens a Mac app, such as `app Notes`, and drives it through its accessibility tree, with your real documents.               |
| `with <name> = <value>`                             | A value the model may type. `$NAME` reads an environment variable and stays secret: the model only sees its name.           |
| `with <name> = file <paths>`                        | Files or folders for the file chooser or picker a `do` goal opens when it names them.                                       |
| `with <name> = zip <folder>`                        | A folder handed over as one `<folder>.zip`.                                                                                 |
| `with <name> = run <command>`                       | What the command prints, such as a test token. It stays secret like `$NAME`.                                                |
| `do <goal>`                                         | Acts until the goal is reached: press, hover, type, choose, Enter, Escape, scroll or, with a model that sees, press a spot. |
| `expect <claim>`                                    | Asks the model whether the screen confirms the claim, waiting up to 5 seconds for it to hold; `within 30s` waits longer.    |
| `see "<text>"`                                      | Checks, without the model, that the text is on the screen or in the address. Waits up to 5 seconds, or `within`.            |
| `see no "<text>"`                                   | Checks that the text is not on the screen, waiting for it to go.                                                            |
| `extract <name> = <what>`                           | Keeps a value from the screen as `$name`, such as `extract id = the id of the new prototype`.                               |
| `request <METHOD> <url>`                            | Sends an HTTP request outside the browser. Headers, `json {...}` or `form name = value` go on indented lines under it.      |
| `route <address> …`                                 | Like Playwright's `page.route`: `to <address>`, `answer <status> [<body>]`, or `hold` until `release <address>`.            |
| `release <address>`                                 | Lets the requests a `hold` keeps waiting go.                                                                                |
| `back`, `forward`, `reload`                         | Like the browser buttons.                                                                                                   |
| `wait <time>`                                       | Lets real time pass, such as `wait 30s`.                                                                                    |
| `clock install`, `clock freeze`, `clock run <time>` | Like Playwright's `clock.install`, `clock.setFixedTime` and `clock.runFor`.                                                 |
| `run <command>`                                     | Runs a shell command. A failing command fails the step.                                                                     |
| `start <name> = <command> [at <url>]`               | Starts a service for the rest of the script and waits for `<url>` to answer, like Playwright's `webServer`.                 |
| `logs <name>`                                       | Shows what a started service has printed.                                                                                   |
| `inject <javascript>`                               | Runs code in every page before its own scripts, like Playwright's `addInitScript`.                                          |
| `use <file>`                                        | Runs the steps of another script here, such as a shared sign-in.                                                            |

`see` and `expect` check whatever the step before them showed: the page, a `request`'s response, or a `run`'s or `logs`'s output.

Without files, `act` reads one script from stdin, so your agent can hand it a task:

```sh
iu2u act <<'EOF'
app Notes
do create a new note that says "Buy milk"
EOF
```

`act` prints each step as it runs, then the failures with the lines around them:

```text
▶ e2e/checkout.e2e
  ✓ open https://shop.example.com 0.9s
  ✓ do add the blue mug to the cart 2.1s
    › pressed button "Add to cart" · in "Blue mug"
  ✗ see "1 item" 5.0s
    → "1 item" is not on the screen
✗ e2e/checkout.e2e 8.0s · 3 model calls

⎯⎯⎯⎯⎯⎯⎯ Failures 1 ⎯⎯⎯⎯⎯⎯⎯

 FAIL  e2e/checkout.e2e:3 › see "1 item"
"1 item" is not on the screen

    1 | open https://shop.example.com
    2 | do add the blue mug to the cart
  > 3 | see "1 item"

⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯⎯

    Scripts  1 failed (1)
      Steps  1 failed | 2 passed (3)
Model calls  3
   Duration  8.0s
```

**How it decides.** Each target is asked for twice, with the options in reverse order, and `act` acts only when both answers agree. A goal fails, or a claim passes, only when the model is at least 75% sure. A spot is found on a grid over the screenshot, then on a finer grid around it.

**Lock files.** When a script passes, `act` writes `<script>.lock` next to it, and later runs replay it without the model while the screen still matches. Dates, ids and other numbers the step does not name do not stop a replay, so name the ones that matter, such as `do pay invoice 1002`. With `--locked`, `act` never asks the model and fails where the screen does not match, so CI stays deterministic.

**Secrets.** Where the screen shows a secret, the model sees only its name and gets no screenshot; text the page restyles or draws in a picture is not caught. Use long, unique, test-only secrets, and open only pages you trust: text on a page can steer which field gets a value.

**Options.**

- `--locked` replays only, as above.
- `--before <script>` runs one script before the others, such as one that starts the servers.
- `--headful` opens the browser in a window.
- `--json` prints a JSON report for each script.
- `IU2U_VISION=true` adds a screenshot to every question about a page, for a model and server that take images.

A failed script saves what the page showed as `<script>.failed.png`. Exit codes: **0** passed, **1** a step failed, **2** a script could not run.

`act` needs macOS or Linux. Set `IU2U_BROWSER` to use another browser. Mac apps run in the background, so `act` never takes over your keyboard or mouse; allow the app that runs `iu2u`, such as your terminal, in **System Settings › Privacy & Security › Accessibility**. A request carries the screen text and can pass 10,000 tokens, so the model server needs that much room.

## How it works

A decision model reads text and answers typed questions: **yes or no**, **pick an option**, or **rate on a scale**. It returns probabilities in one pass, without generating a prose response.

iu2u sends the relevant text and questions to your server's `POST /v1/systemone` endpoint, then turns the answers into hook decisions, tool results, or command output. Latency and judgment quality depend on your model and server.

The endpoint matters: a server exposing only a chat-completions API is not sufficient without an [adapter](#adapters). The wire format lives in [`src/model/protocol.rs`](src/model/protocol.rs).

## How the guard decides

Every evaluated shell command is sent **alone, as data**, with three questions:

1. Would this cause damage that is hard to recover from?
2. Would this act outside the computer? Reading or downloading alone does not count.
3. Could the worst realistic outcome harm shared data, other people, or production?

The first matching rule wins:

| Model signal                                                                              | Guard response                                               |
| ----------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| Probability of irreversible damage **≥ 0.85**                                             | **Block**                                                    |
| Probability of irreversible damage, external action, or the most severe outcome **≥ 0.5** | **Ask you**                                                  |
| All three signals below **0.5**                                                           | **No permission decision**; the host's rules and mode decide |

The guard never returns `allow`. A misleading command can fool the model into staying silent; that silence still does not grant permission. Your host may nevertheless permit the command under its own rules. Evaluate these thresholds on your model and your own commands.

### If the model cannot judge

An unreachable or unconfigured model, an unusable response, or a command longer than **4,000 characters** takes the fallback path:

| Claude Code mode       | Fallback                                                  |
| ---------------------- | --------------------------------------------------------- |
| Default, edit, or plan | No permission decision; existing rules apply.             |
| Auto or bypass         | Ask for approval.                                         |
| `dontAsk`              | Request approval, which Claude Code converts to a denial. |

If the **hook itself cannot run**—for example, the binary is missing from Claude Code's `PATH` or its version is incompatible—the plugin requests that shell commands be blocked. This requires Claude Code **v2.1.295+**. Restore a working binary before continuing.

## Settings

Inside Claude Code, plugin options take precedence. For standalone commands and Codex, set environment variables.

| Plugin option | Environment variable | Default                                      |
| ------------- | -------------------- | -------------------------------------------- |
| `base_url`    | `IU2U_BASE_URL`      | `http://127.0.0.1:11435`                     |
| `model`       | `IU2U_MODEL`         | Required; or an [adapter](#adapters)         |
| `effort`      | `IU2U_EFFORT`        | `medium`, for the `claude` adapter           |
| `api_key`     | `IU2U_API_KEY`       | None                                         |
| —             | `IU2U_TIMEOUT_MS`    | `10000` milliseconds per request             |
| `guard`       | `IU2U_GUARD`         | `true`                                       |
| `proof`       | `IU2U_PROOF`         | `true`                                       |
| `trim`        | `IU2U_TRIM`          | `false`                                      |
| `compass`     | `IU2U_COMPASS`       | `false`                                      |
| `prune`       | `IU2U_PRUNE`         | `false`                                      |
| —             | `IU2U_BROWSER`       | The first browser found                      |
| —             | `IU2U_VISION`        | `false`; `true` for a model that sees images |

Use the server's base URL: iu2u appends `/v1/systemone`. The API key is sent as a bearer token. Hook switches accept `true` or `false`.

## Adapters

An adapter lets iu2u ask a model that has no `/v1/systemone` endpoint. Set `IU2U_MODEL`, or the `model` plugin option, to `apple-intelligence`, `claude` or `claude:<model>`; any other name is a model on a System One server, as above. An adapter asks the model, in one request, for the probability of every answer, option or level as JSON. The model states these probabilities itself, so they are not calibrated the way a decision model's are. Adapters take no screenshots and ignore `IU2U_BASE_URL` and `IU2U_API_KEY`. `trim` asks once per 40 lines, up to 200 requests, and `prune` once per step, up to 150, within a two-minute hook limit: apfel can run out of time on the longest outputs and sessions, which then stay as they were.

### Apple Intelligence

Apple's on-device model, through [apfel](https://github.com/Arthur-Ficial/apfel), installed with Homebrew or any tool that puts it on the `PATH`, such as `mise use -g github:Arthur-Ficial/apfel`:

```sh
brew install apfel
export IU2U_MODEL="apple-intelligence"
```

Each request runs `apfel` once, without your `APFEL_` settings, so no server has to be running: in tests a server answered no faster, because the model handles one request at a time. apfel needs macOS 26 with Apple Intelligence turned on. The model reads 4,096 tokens, or 8,192 on macOS 27 with an M3 or later and 12 GB of memory, which is too little for `act`. It also judges commands poorly: in tests it rated `ls -la` and `cat README.md` as irreversible, so the guard blocks harmless commands. Turn the guard off with `IU2U_GUARD=false`, or the `guard` plugin option, when you use this adapter.

### Claude Code

```sh
export IU2U_MODEL="claude"
```

Each request runs `claude --print` with your Claude Code login, or with `ANTHROPIC_API_KEY` or your provider's variables set in the environment: an `apiKeyHelper` or `env` in `settings.json` does not apply. It runs on `haiku` at `medium` effort: set `IU2U_MODEL` to `claude:<model>`, such as `claude:sonnet`, for another model, and `IU2U_EFFORT` for another effort. The session loads no user, project or local settings, no plugins, MCP servers, auto memory, `CLAUDE.md` or tools, and iu2u's own hooks stay off inside it even when managed settings load them. Every request takes a few seconds and counts against your Claude plan, or is billed to your API key. The guard makes one for every shell command, proof one per turn and per subagent, and compass one per prompt; `weigh` and `rank` make one per item, up to 500, and `seek` and `lint` one per excerpt, up to 2,000 per call; `filter` makes one per non-empty line, with no limit, and `act` several per step, trying one that times out up to 3 times.

## Codex

Add `iu2u mcp` as an MCP server in `~/.codex/config.toml`, with `IU2U_MODEL` and the other server settings available to the process. For the guard, register `iu2u hook guard` as a `PreToolUse` command hook with matcher `Bash`.

The Codex adapter cannot request approval from a hook: it **blocks** commands it would ask about in Claude Code. If the model cannot judge, it makes **no permission decision**, leaving Codex's own rules in charge.

## The name

**It's up to you.** The guy in the logo is pointing at the person who gets to decide.

## License

[Apache-2.0](LICENSE).
