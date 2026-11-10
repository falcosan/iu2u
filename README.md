# iu2u

Powers for coding agents, backed by a fast decision model of your choice.

A decision model reads some text and answers typed questions (yes/no, pick one option, rate on a scale) with probabilities, in one pass, in milliseconds, without generating text. iu2u puts that speed to work inside Claude Code: hooks that guard, check and trim what the agent does, and tools the agent calls when the same question applies to many things.

Bring any model, local or hosted: iu2u talks to any server that answers `POST /v1/systemone`.

## Powers

| Power         | Kind                            | What it does                                                                                                                      | Default |
| ------------- | ------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- | ------- |
| `guard`       | Hook, before each shell command | Blocks commands that look irreversible and asks you about risky ones. It never approves anything on its own                       | On      |
| `proof`       | Hook, when an agent finishes    | Sends the agent back to work when it says "done" without reporting a check it ran or what it could not check                      | On      |
| `trim`        | Hook, after each shell command  | Hides the irrelevant parts of very long output. Lines with errors or warnings always stay, and the full output is saved to a file | Off     |
| `compass`     | Hook, on each prompt            | Suggests the personal or project skill that fits the request                                                                      | Off     |
| `prune`       | Hook, before compaction         | Tells compaction which steps to keep in detail and which to drop                                                                  | Off     |
| `weigh`       | Tool                            | Ask the same typed questions about up to 500 items                                                                                | Always  |
| `rank`        | Tool                            | Order items by a criterion, highest first                                                                                         | Always  |
| `seek`        | Tool                            | Search files by meaning and get file and line ranges back                                                                         | Always  |
| `lint`        | Tool                            | Check files against rules written in plain words and get the line range of each likely violation                                  | Always  |
| `iu2u filter` | Command                         | Keep the stdin lines for which a statement is true, like a grep that understands meaning                                          |         |
| `iu2u next`   | Command                         | Pick the next UI action toward a goal, for scripted end-to-end tests                                                              |         |

## Install

1. Build the binary and put it on your `PATH`:

   ```sh
   cargo install --locked --git https://github.com/falcosan/iu2u
   ```

2. Run a decision model server, locally or hosted, and note its URL and model name.
3. Add the plugin to Claude Code:

   ```sh
   claude plugin marketplace add falcosan/iu2u
   claude plugin install iu2u@iu2u --config model=<model> --config base_url=<url>
   ```

   Switch hook powers on or off the same way, for example `--config trim=true`. For a hosted server, set the API key inside Claude Code with `/plugin configure iu2u@iu2u`, which keeps it out of your shell history. Change any option later with the same command.

## Settings

Inside Claude Code, the plugin's options win. Everywhere else, such as `iu2u filter`, `iu2u next` and Codex, the binary reads these environment variables.

| Variable                                                              | Meaning                                  | Default                  |
| --------------------------------------------------------------------- | ---------------------------------------- | ------------------------ |
| `IU2U_BASE_URL`                                                       | Server URL; iu2u appends `/v1/systemone` | `http://127.0.0.1:11435` |
| `IU2U_MODEL`                                                          | Model name on that server                | Required                 |
| `IU2U_API_KEY`                                                        | Bearer key for hosted servers            | None                     |
| `IU2U_TIMEOUT_MS`                                                     | Timeout per request                      | `10000`                  |
| `IU2U_GUARD`, `IU2U_PROOF`, `IU2U_TRIM`, `IU2U_COMPASS`, `IU2U_PRUNE` | `true` or `false`                        | See the table above      |

## How the guard decides

Each shell command, including the ones Claude starts with the Monitor tool (Claude Code v2.1.314 or later), is sent alone, as data, with three questions: is it irreversible, does it act outside this computer (reading or downloading alone does not count), and what is the worst realistic outcome.

- Blocks the command when "irreversible" is 0.85 or more.
- Asks you when any signal is 0.5 or more.
- Otherwise stays silent, so your own permission rules and mode decide.
- It never returns "allow". A model fooled by text inside a command can at worst stay silent, never grant permission.
- When the model is unreachable or not configured, or the command is longer than 4,000 characters, it stays silent if you are watching (default, edit and plan modes) and asks you when nobody is (auto and bypass modes). In dontAsk mode, Claude Code turns every question into a denial.

If the guard itself cannot run, for example because `iu2u` is not on the `PATH` Claude Code sees or does not match the plugin's version, Claude Code blocks every shell command until you fix it. This needs Claude Code v2.1.295 or later.

Thresholds are calibrated per model. Measure them on your own commands before relying on them.

## Codex

The binary works with Codex too: add `iu2u mcp` as an MCP server in `~/.codex/config.toml`, and register `iu2u hook guard` as a `PreToolUse` command hook with matcher `Bash`. Codex cannot ask the user from a hook, so there the guard blocks whatever it would have asked about, and stays silent when the model is down. This setup has not been tested yet.

## Command line

```sh
tail -f app.log | iu2u filter "This line reports a failure"
echo '{"goal":"log in","screen":"...","actions":{"login":"Click Log in","help":"Click Help"}}' | iu2u next
```

`filter` prints each matching line as soon as it is judged and, like grep, exits with 1 when no line matches and 2 on an error. `next` returns the action, its confidence, and the probabilities that the goal is done or stuck.

## Safety

- Everything you send goes to the model server you configure. Use a local server for private code.
- Text inside the judged material can steer a decision model. iu2u keeps each state minimal and treats it as data, but never let a decision alone approve an action that moves money, data, access or code.
- `seek` and `lint` read the files the agent passes them, with your user's permissions. Claude Code's permission rules, such as a `Read` deny rule, do not apply to them.
- Do not turn on `trim` next to another hook that rewrites or redacts command output. Claude Code runs those hooks on the original output and keeps only the last rewrite, so `trim` can bring back what the other hook hid.

## Development

```text
.claude-plugin/marketplace.json   marketplace that lists the plugin
plugins/iu2u/                     the plugin: manifest, hooks, skills
src/model/                        decision model client and our question and answer types
src/hooks/                        one file per hook power
src/tools/                        one file per tool
src/commands/                     command line powers
src/mcp.rs                        MCP server over the tools
tests/                            integration tests against a mocked model server
```

```sh
cargo test
cargo clippy --all-targets -- -D warnings
claude plugin validate .
claude plugin validate plugins/iu2u
```

## License

Apache-2.0
