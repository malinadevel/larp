# LARP — Local Agent Runtime Platform

LARP is an early-stage Rust project for creating and running task-focused AI agents that can use
local tools. It is intended to grow beyond a single chatbot into a user-controlled platform for
building agents and automations that interact with the operating system through configurable
tools, explicit permissions, and observable execution.

The current version is a command-line MVP. It supports multiple independently configured agents,
streaming model responses, command-based tool calls, per-agent conversation history, and tool-call
approval controlled by each agent's security policy.

## What works today

- Create and select separate agents, each with its own model settings, instructions, tools,
  security policy, and conversation history.
- Stream responses from an OpenAI-compatible Chat Completions endpoint (the default endpoint is
  OpenRouter).
- Let the model call tools declared in that agent's TOML manifests.
- Require user approval for tool executions and configure the environment passed to each tool.
- Limit tool-call rounds, execution time, and captured output size.

LARP currently runs as a single CLI process. It does not yet include a daemon, TUI, durable task or
event system, workflow engine, remote execution targets, or an OS-level sandbox.

## Quick start

Install a Rust toolchain and provide an API key through the environment variable named in the
agent's configuration. The default template uses `TEST_API_KEY`:

```bash
export TEST_API_KEY="your-api-key"
cargo run -- "Summarize the files in the current directory"
```

On first run, LARP creates `~/.larp/` and installs any missing files from `templates/`. Existing
files are preserved rather than overwritten.

## Agents

The default agent is recorded in `~/.larp/agents/index.toml`. Each agent has an isolated directory:

```text
~/.larp/
├── SYSTEM_PROMPT.md
└── agents/
    ├── index.toml
    ├── default/
    │   ├── config.toml
    │   ├── AGENT_PROMPT.md
    │   ├── security.toml
    │   ├── context.json
    │   └── tools/
    └── reviewer/
        └── ...
```

Each agent owns its tools; there is no shared tool directory. A command tool is described by a TOML
manifest in that agent's `tools/` directory. Its `program` can point to an executable script or
program, and `argv` maps model-provided fields to individual process arguments.

```bash
larp agents                         # List agents and mark the current one
larp agents create reviewer        # Create an agent from the starter template
larp set reviewer                  # Set the current agent for future invocations
larp "Summarize this project"      # Use the current agent
larp --agent reviewer "Review this" # Select an agent for this run only
larp --agent reviewer --clear      # Clear only that agent's history
```

`larp set <id>` persists the current agent. `--agent <id>` overrides it for one invocation without
changing the saved selection. Agent IDs may contain 1–64 ASCII letters, digits, underscores, or
hyphens.

## Instructions and configuration

`~/.larp/SYSTEM_PROMPT.md` contains instructions shared by agents. Each agent can add its own
instructions in `AGENT_PROMPT.md`. Set the composition mode in the agent's `config.toml`:

```toml
[prompt]
mode = "merge"
```

Supported modes are `global` (shared instructions only), `agent` (agent instructions only), and
`merge` (shared instructions followed by agent instructions). The default is `merge`.

The agent's `config.toml` also sets the model ID, API endpoint, API-key environment variable, and
maximum tool-call rounds. The API key itself should stay in the environment, not in a config file.

## Tool permissions and safety

Each agent has its own `security.toml`. In the starter policy, `defaults.enabled = true` and
`approval = "always"`: tools without a specific rule are enabled, but every call still requires
confirmation. Approval can be set to `always` or `never`; environment handling can be `clean` or
`inherit`.

Tools run with the operating-system permissions of the LARP process. A timeout, output limit,
approval prompt, or clean environment is **not** an OS-level sandbox. In particular, Bash can run
arbitrary commands as the current user. Review tool manifests and security policies before using
an agent, and keep API keys in environment variables.

## Longer-term direction

The project aims to become a modular platform for creating, running, and observing personal agents
and automation workflows. The intended direction includes:

- richer session and context management;
- durable tasks, run history, and events;
- sequential and parallel multi-agent workflows;
- a daemon and local API for long-running agents;
- schedulers and event-triggered automation;
- pluggable integrations and execution targets, including isolated and remote environments;
- stronger, operating-system-enforced execution isolation.

These are goals, not current features. The immediate priority is to keep the CLI runtime small,
understandable, and safe while adding capabilities in usable increments.

## Migrating an existing configuration

If `~/.larp/` contains the previous flat configuration, the first launch copies its config,
security policy, history, and tools into `~/.larp/agents/default/`. The existing files are kept;
the previous `SYSTEM_PROMPT.md` remains the shared prompt.

## Development checks

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```
