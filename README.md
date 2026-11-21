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
  <a href="#local-decision-model">Local model</a> ·
  <a href="#how-the-guard-decides">Guard behavior</a>
</p>

---

You ask for a small fix. Your agent reaches for `rm -rf`, lines up a push to `main`, and announces that everything works.

You were hoping for a test.

**iu2u gives your agent a second opinion.** A decision model checks shell commands before they run, looks for evidence in completion reports, and helps sift through output. The same model can rank a backlog, search code by meaning, or judge hundreds of items against the same questions.

Five hooks. Four tools. Two shell commands. And one deliberate limit: **the guard never grants permission.** It can block, ask, or leave your existing permission rules in charge.

_iu2u. It's up to you._

## Install

You need **Rust 1.89+** and **Claude Code**: v2.1.295+ to block commands when the guard cannot launch, v2.1.314+ for shell commands started through Monitor.

```sh
cargo install --locked iu2u     # make sure iu2u is on the PATH Claude Code sees
iu2u init                       # choose the model and the hooks
claude plugin marketplace add falcosan/iu2u
claude plugin install iu2u@iu2u
```

`init` asks for a model and the hooks to turn on, shows what it will write and download, then installs it and asks the model one test question. On a Mac with Apple silicon the default is the [local decision model](#local-decision-model); elsewhere, point it at a server that implements `POST /v1/systemone` or use the [Claude adapter](#adapters). Run it again at any time.

A hosted server's API key goes in `/plugin configure iu2u@iu2u` inside Claude Code, out of your shell history and the config file.

## Hooks

The agent does not have to remember to call these.

| Hook      | Steps in               | What changes                                                                                                            | Default |
| --------- | ---------------------- | ----------------------------------------------------------------------------------------------------------------------- | ------- |
| `guard`   | Before a shell command | Flags irreversible or risky actions. Blocks or asks; never auto-approves.                                               | On      |
| `proof`   | When an agent finishes | Sends unsupported completion claims back for reported checks, or an explanation of what could not be verified.          | On      |
| `trim`    | After a shell command  | Shortens very long output, preserves blocks with common error and warning markers, and saves the full output to a file. | Off     |
| `compass` | On a prompt            | Suggests a relevant personal or project skill.                                                                          | Off     |
| `prune`   | Before compaction      | Advises which steps to preserve in detail and which to drop.                                                            | Off     |

`proof` checks what the agent **reports**; it does not run tests. Do not pair `trim` with another hook that rewrites command output: only one rewrite is kept. With the [local model](#local-decision-model), a `warm` hook also loads the model when a session starts and on each prompt, while any other hook is on.

## Tools

Available to the agent over MCP.

| Tool    | What it does                                                                | Try asking your agent                             |
| ------- | --------------------------------------------------------------------------- | ------------------------------------------------- |
| `weigh` | Applies the same typed questions to up to **500 items**.                    | “Which of these issues describe a regression?”    |
| `rank`  | Orders items by a criterion, highest first.                                 | “Rank these bugs by likely user impact.”          |
| `seek`  | Searches files by meaning and returns file and line ranges.                 | “Find the code that handles expired sessions.”    |
| `lint`  | Checks files against rules in plain language and locates likely violations. | “Find places where an error is silently ignored.” |

## Command line

The same model works in a pipe. Set `IU2U_API_KEY` if your server requires authentication.

### Find the failure, however it was worded

```sh
tail -f app.log | iu2u filter "This line reports a failure"            # probability ≥ 0.5
iu2u filter "This line reports a failure" --at 0.8 < app.log
```

Matching lines stream out in input order. Exit codes follow grep: **0** a match, **1** none, **2** an error.

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
| `with <name> = run <command>`                       | What the command prints, such as a test token, within 2 minutes or its `within`. It stays secret like `$NAME`.              |
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
| `run <command>`                                     | Runs a shell command once; a failing command fails the step. To wait for something, loop inside the command.               |
| `start <name> = <command> [at <url>]`               | Starts a service for the rest of the script and waits for `<url>` to answer, like Playwright's `webServer`.                 |
| `logs <name>`                                       | Shows what a started service has printed.                                                                                   |
| `inject <javascript>`                               | Runs code in every page before its own scripts, like Playwright's `addInitScript`.                                          |
| `use <file>`                                        | Runs the steps of another script here, such as a shared sign-in.                                                            |

`see` and `expect` check whatever the step before them showed: the page, a `request`'s response, or a `run`'s or `logs`'s output. On a response or output they look once, unless `within` lets them send a safe `request` again or read the `logs` again; a `run` never runs again. Files named by `use`, `file` and `zip`, and an `app` given by its path, are found beside the script that names them, by the path as written. A `route` address without `*` matches itself and the paths and queries under it. In the addresses of `open`, `request`, `route`, `release` and `start ... at`, a `$name` after the `?` is percent-encoded, so give it unencoded; a `route` matches a query however the page encodes it. `see` looks in the addresses of the page and its frames only for text with `/`, `?`, `&`, `=` or `#`. A `$name` works in the text and addresses of every step but `app`, `use` and `inject`, in a `with` value, where it can name an earlier `with ... = run`, and in a `file` path; quotes keep a `with` value as written, and `$$` is a literal `$`. In `run` and `start` the shell expands `$NAME`, so their output hides only values named with `with`: write `with token = $API_TOKEN`, then use `$token`. A `run` still going at its `within` limit fails the step.

Without files, `act` reads one script from stdin, so your agent can hand it a task:

```sh
iu2u act <<'EOF'
app Notes
do create a new note that says "Buy milk"
EOF
```

`act` prints each step as it runs, then each failure with the script lines around it:

```text
▶ e2e/checkout.e2e
  ✓ open https://shop.example.com 0.9s
  ✓ do add the blue mug to the cart 2.1s
    › pressed button "Add to cart" · in "Blue mug"
  ✗ see "1 item" 5.0s
    → "1 item" is not on the screen
✗ e2e/checkout.e2e 8.0s · 3 model calls
```

- **Decisions.** Each target is asked for twice, with the options reversed, and `act` acts only when both answers agree. A goal fails, or a claim passes, only at 75% certainty or more.
- **Lock files.** A passing script writes `<script>.lock`; later runs replay it without the model while the screen still matches. Numbers a step does not name, such as dates and ids, do not stop a replay, so name the ones that matter (`do pay invoice 1002`). `--locked` never asks the model, for deterministic CI.
- **Secrets.** Where a secret shows, even escaped for a URL, HTML or JSON, the model sees only its name and gets no screenshot; text the page restyles or draws as a picture, writes with named HTML entities such as `&ntilde;`, or shows only for the moment a screenshot is taken, is not caught. Use long, test-only secrets and pages you trust.
- **Options.** `--locked`, `--before <script>` (runs first, such as one that starts servers), `--headful`, `--json`. `vision = true` adds a screenshot to every question, for a model that takes images.
- **Results.** A script file that fails on a web page saves `<script>.failed.png`, unless a secret shows on the page. Exit codes: **0** passed, **1** a step failed, **2** a script could not run.
- **Requirements.** macOS or Linux; `browser` picks another browser. Mac apps run in the background, without taking your keyboard or mouse: allow your terminal in **System Settings › Privacy & Security › Accessibility**.
- **Long screens.** The model reads as much of the screen as fits in half of its context, keeping 10,000 tokens for the rest of the question, and never less than the first 6,000 characters: on a llama.cpp server, such as the [local model](#local-decision-model), counted in tokens by the server; with the Claude adapter, up to 100,000 bytes, half of the 200,000 tokens every Claude model takes. Other servers read the first 6,000 characters. Each question gets `timeout_ms` for every 6,000 characters it sends. A failed step says when the model saw only part of the screen.

## How it works

A decision model reads text and answers typed questions (**yes or no**, **pick an option**, **rate on a scale**) with probabilities, in one pass and without writing prose. iu2u sends the text and questions to `POST /v1/systemone` and turns the answers into hook decisions, tool results or command output. A chat-completions API alone needs an [adapter](#adapters). The wire format is in [`src/model/protocol.rs`](src/model/protocol.rs).

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

The guard never returns `allow`: a command that fools the model gets silence, not permission, and your host's rules still decide. Check these thresholds against your model and your commands.

### If the model cannot judge

A command longer than **4,000 characters** is too long to judge in time, so the guard asks you about it (Codex blocks it). An unreachable or unconfigured model, a local model that is not installed or still starting, or an unusable response takes the fallback path:

| Claude Code mode       | Fallback                                                  |
| ---------------------- | --------------------------------------------------------- |
| Default, edit, or plan | No permission decision; existing rules apply.             |
| Auto or bypass         | Ask for approval.                                         |
| `dontAsk`              | Request approval, which Claude Code converts to a denial. |

If the **hook itself cannot run**, such as when `iu2u` is missing from Claude Code's `PATH`, the plugin blocks shell commands until the binary works again.

## Settings

One file, `~/.config/iu2u/config.toml` (`$XDG_CONFIG_HOME/iu2u/config.toml` when set), never one from a repository. An `IU2U_*` variable overrides it for one shell or command. Hooks and MCP tools read it at each call, so changes reach open sessions at once.

| Config key   | Environment variable | Default                                                                              |
| ------------ | -------------------- | ------------------------------------------------------------------------------------ |
| `model`      | `IU2U_MODEL`         | None; `decision`, a [local model](#local-decision-model), or an [adapter](#adapters) |
| `effort`     | `IU2U_EFFORT`        | `medium`, for the `claude` adapter                                                   |
| `base_url`   | `IU2U_BASE_URL`      | `http://127.0.0.1:11435`; iu2u appends `/v1/systemone`                               |
| `timeout_ms` | `IU2U_TIMEOUT_MS`    | `10000` milliseconds per request; `act` allows it for each 6,000 characters it sends |
| `guard`      | `IU2U_GUARD`         | `true`                                                                               |
| `proof`      | `IU2U_PROOF`         | `true`                                                                               |
| `trim`       | `IU2U_TRIM`          | `false`                                                                              |
| `compass`    | `IU2U_COMPASS`       | `false`                                                                              |
| `prune`      | `IU2U_PRUNE`         | `false`                                                                              |
| `vision`     | `IU2U_VISION`        | `false`; `true` for a model that sees images                                         |
| `browser`    | `IU2U_BROWSER`       | The first browser found                                                              |

```sh
iu2u config                    # every setting, its value and where it comes from
iu2u config set trim true      # keeps the file's comments and order
iu2u config unset trim
iu2u config get model
```

Switches take `true` or `false`, or `1` or `0`. The API key is never in the file: in Claude Code set it with `/plugin configure iu2u@iu2u`, where the plugin's hooks and tools take it instead of `IU2U_API_KEY`; outside Claude Code, set `IU2U_API_KEY`. It is sent as a bearer token. An unknown key or a wrong value is an error naming the file and the key, and hooks take the fallback path until `iu2u config unset` removes it. A request that times out or gets a 429 or 5xx answer is sent up to three times, one and then two seconds apart. `guard`, `proof` and `compass` wait 12 seconds at most, whatever `timeout_ms` says; `trim` and `prune` wait up to 110 seconds. On Windows, where `init` does not run, use `iu2u config set`; `~` is `%USERPROFILE%` when `HOME` is unset.

## Local decision model

`model = "decision"` runs [Fastino GLiDE-9B](https://huggingface.co/fastino/GLiDE-9B) on your machine with the official [llama.cpp](https://github.com/ggml-org/llama.cpp) server. Nothing it judges leaves the machine.

- **Download.** `init` fetches `fastino_GLiDE-9B-Q4_K_M.gguf` (5.8 GB) from [bartowski/fastino_GLiDE-9B-GGUF](https://huggingface.co/bartowski/fastino_GLiDE-9B-GGUF) and llama.cpp [`b12359`](https://github.com/ggml-org/llama.cpp/releases/tag/b12359) (about 20 MB), both pinned by SHA-256. An interrupted download resumes, and after an upgrade that changes the llama.cpp pin the runtime installs itself on the next request.
- **License.** llama.cpp is MIT. GLiDE-9B's [Fastino Community License](https://huggingface.co/fastino/GLiDE-9B/blob/main/LICENSE) binds you: commercial use must show "Built with Fastino GLiDE", and organisations above 10 million US dollars of yearly revenue need an agreement with Fastino.
- **Other models.** Any GGUF decision model on Hugging Face: give its link to `init`, or set `model` to `decision:<owner>/<repo>[:<file>]` (the `Q4_K_M` file by default). A link to a file at a commit keeps that commit. `HF_TOKEN` unlocks gated repositories.
- **Running.** The first request starts the server in the background; it exits five minutes after the last one. It listens on a private Unix socket, never a port. `iu2u status` shows what is installed and running; `iu2u stop` stops it.
- **Files.** `~/.local/share/iu2u` (`$XDG_DATA_HOME/iu2u` when set). To remove everything: `iu2u stop`, then delete that directory and the config file.
- **Hardware.** About the model's size plus 1.5 GB of memory: 6.8 GB on an Apple M5 Pro, where a guard decision takes under a second. Runs on macOS with Apple silicon and on Linux x86-64 and arm64 (glibc 2.35+, 2.38+ on arm64, `libgomp1`, OpenSSL 3). On Linux the runtime is CPU-only and takes over 10 seconds per question, too slow for hooks: point `base_url` at a GPU server instead.

## Adapters

An adapter asks a model that has no `/v1/systemone` endpoint for the probability of every answer as JSON. The model states these probabilities itself, so they are not calibrated like a decision model's. Adapters take no screenshots and ignore `base_url` and the API key.

### Claude Code

```sh
iu2u config set model claude        # haiku at medium effort; claude:sonnet for another model
```

Each request runs `claude --print` with your Claude Code login, `ANTHROPIC_API_KEY` or your provider's variables (`apiKeyHelper` and `env` in `settings.json` do not apply). The session loads no settings, plugins, MCP servers, memory, `CLAUDE.md` or tools, and iu2u's hooks stay off inside it. Each request takes a few seconds and counts against your plan or API key:

| Power          | Requests                              |
| -------------- | ------------------------------------- |
| `guard`        | One per shell command                 |
| `proof`        | One per turn and per subagent         |
| `compass`      | One per prompt                        |
| `weigh`,`rank` | One per item, up to 500               |
| `seek`,`lint`  | One per excerpt, up to 2,000 per call |
| `filter`       | One per non-empty line                |
| `trim`         | One per 40 lines, up to 200           |
| `prune`        | One per step, up to 150               |
| `act`          | Several per step                      |

`trim` and `prune` stop at the two-minute hook limit and leave what they could not finish as it was.

## Codex

Add `iu2u mcp` as an MCP server in `~/.codex/config.toml`, and `iu2u hook guard` as a `PreToolUse` command hook with matcher `Bash`. Both read iu2u's config file. Codex hooks cannot ask, so the guard **blocks** what it would ask about in Claude Code; when the model cannot judge, Codex's own rules decide.

## The name

**It's up to you.** The guy in the logo is pointing at the person who gets to decide.

## License

[Apache-2.0](LICENSE).
