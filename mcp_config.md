[LEGEND]
CLIENT_CONFIG = Client-side stdio wiring: command, args, and environment.
STDIO_SERVER = Infra running over stdin/stdout.
PROFILE_DIR = Directory for local profiles and state.
UNSAFE_LOCAL = Explicit opt-in for local exec and filesystem tools.
SANITY_CHECK = A minimal call to confirm the server responds.

[CONTENT]
# MCP client configuration

Infra is a [STDIO_SERVER]. Configure your MCP client to launch the binary and
pass environment variables as needed ([CLIENT_CONFIG]).

## Build

```bash
cargo build --release
```

Binary path:

`target/release/infra`

## Example client config

Most MCP clients accept a `command`, `args`, and `env`. Adjust to your client.

```json
{
  "mcpServers": {
    "infra": {
      "command": "/path/to/infra/target/release/infra",
      "args": [],
      "env": {
        "LOG_LEVEL": "info",
        "MCP_PROFILES_DIR": "/tmp/infra-profiles",
        "INFRA_UNSAFE_LOCAL": "0"
      }
    }
  }
}
```

## High-signal env vars

- `MCP_PROFILES_DIR`: isolate profiles and local state ([PROFILE_DIR]).
- `INFRA_UNSAFE_LOCAL=1`: enable local exec + filesystem tools ([UNSAFE_LOCAL]).
- `INFRA_ALLOW_SECRET_EXPORT=1`: allow explicit profile export of secrets.
- `LOG_LEVEL=debug`: verbose logs for debugging.

## [SANITY_CHECK]

Call `help` to verify the server is alive:

```json
{ "tool": "help", "args": { "query": "mcp_project" } }
```
