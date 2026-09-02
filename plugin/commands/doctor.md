---
description: Check that agmem memory is wired into this session — binary, store, model, MCP registration — and print the fix for anything that is not.
allowed-tools: Bash(agmem --doctor:*), Bash(agmem --version:*), Bash(claude mcp list:*), Bash(which agmem:*)
---

# agmem doctor

Installation self-check, from the binary:

```
!`agmem --doctor 2>&1 || echo "agmem --doctor exited $?"`
```

MCP servers this session can see:

```
!`claude mcp list 2>&1`
```

Read both and report in at most six lines:

1. Whether the binary is on PATH and its version (the plugin needs 0.1.10 or
   newer for `agmem hook`; older binaries serve MCP fine but the hooks print
   nothing).
2. Whether the self-check passed; for each failing line, its printed fix.
3. Whether the `agmem` server is registered **twice** — once by this plugin
   and once at user or project scope. Claude Code connects only the
   higher-precedence one (local > project > user > plugin), so this is
   harmless, but it decides the tool names: `mcp__agmem__*` when your own
   registration wins, `mcp__plugin_agmem_agmem__*` when the plugin's does.
   Permission rules and hooks in this plugin accept both.
4. If no agmem tools are in this session at all, the likely cause: the
   plugin's server is disabled in `/mcp`, or the binary failed to start —
   `agmem --doctor` says which.
