# ExtraResources

Everything in this folder ships with Beyond CLI and is copied into the user's
own `ExtraResources` folder on install (`%LOCALAPPDATA%\beyond-cli\ExtraResources`).
Files a user already has are never overwritten.

- `MCPs/` — MCP servers (`.exe`, `.py`, `.js`). They start automatically and
  their tools merge into the agent's tool list.
- `Skills/` — markdown skills with `name:` / `description:` frontmatter,
  injected into every session. The agent writes its own skills here too.

Contributing a skill or an MCP server? Add it here and open a pull request.
