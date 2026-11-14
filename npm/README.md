# Beyond CLI

A fast, lightweight, galaxy-themed Claude coding agent for your terminal.
Windows only for now.

```bash
npm install -g beyond-cli
pnpm add -g beyond-cli
bun add -g beyond-cli
```

Then run `beyond`. The first launch walks you through setup.

The package ships a small native binary (written in Rust) plus a tiny Node
launcher. Bundled MCP servers and skills are copied to
`%LOCALAPPDATA%\beyond-cli\ExtraResources` — add your own there.

Source, issues and contributing: https://github.com/OGFN-Open-sourceing/Beyond-CLI
