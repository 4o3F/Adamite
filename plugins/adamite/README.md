# Adamite for Claude Code

This plugin connects Claude Code to Adamite's NTFS snapshot queries and scan jobs and installs the `disk-usage` Skill. Install the `adamite` binary separately and put it on PATH before connecting; see the [downloads and build instructions](../../README.md). The plugin does not bundle a binary.

## Plugin installation

In Claude Code, add the repository marketplace and install the plugin:

```text
/plugin marketplace add 4o3F/Adamite
/plugin install adamite@adamite
```

For a local checkout, replace the marketplace command with `/plugin marketplace add /absolute/path/to/Adamite`, then use the same install command. The marketplace is at the repository root; the plugin lives in `plugins/adamite`.

The plugin starts `adamite mcp` over stdio. Check the connection with `/mcp`. Ask, for example, “Why is C: full?” or “Find large videos in my saved Adamite snapshot.” The Skill discovers snapshots, asks before scanning, explains Windows UAC for drive scans, polls progress and inspects candidates. See the [Skill](skills/disk-usage/SKILL.md) and [server options](../../crates/adamite-mcp/README.md) for the workflow and snapshot discovery settings.

## Manual installation

From the repository root:

1. Register the MCP server: `claude mcp add --scope user adamite -- adamite mcp`.
2. Copy `plugins/adamite/skills/disk-usage/` to `~/.claude/skills/disk-usage/`, including its `references/` directory. For example, run `mkdir -p ~/.claude/skills` and `cp -R plugins/adamite/skills/disk-usage ~/.claude/skills/` in a POSIX shell.

When starting from this plugin directory, the copy source is `skills/disk-usage/`. Manual registration uses the same separately installed PATH binary.

## Tool permissions

Merge an example below into Claude Code's `~/.claude/settings.json`. Allow the query tools and scan-status polling individually, and keep `scan_volume` on `ask`. The Skill also requires the user's agreement before starting a scan.

For the plugin install:

```json
{
  "permissions": {
    "allow": [
      "mcp__plugin_adamite_adamite__snapshots",
      "mcp__plugin_adamite_adamite__snapshot_info",
      "mcp__plugin_adamite_adamite__get_tree",
      "mcp__plugin_adamite_adamite__list_dir",
      "mcp__plugin_adamite_adamite__top_files",
      "mcp__plugin_adamite_adamite__ext_breakdown",
      "mcp__plugin_adamite_adamite__search",
      "mcp__plugin_adamite_adamite__node_info",
      "mcp__plugin_adamite_adamite__scan_status"
    ],
    "ask": ["mcp__plugin_adamite_adamite__scan_volume"]
  }
}
```

For the manual install:

```json
{
  "permissions": {
    "allow": [
      "mcp__adamite__snapshots",
      "mcp__adamite__snapshot_info",
      "mcp__adamite__get_tree",
      "mcp__adamite__list_dir",
      "mcp__adamite__top_files",
      "mcp__adamite__ext_breakdown",
      "mcp__adamite__search",
      "mcp__adamite__node_info",
      "mcp__adamite__scan_status"
    ],
    "ask": ["mcp__adamite__scan_volume"]
  }
}
```

Plugin tools follow `mcp__plugin_<plugin-name>_<server-name>__<tool-name>`; manual tools follow `mcp__<server-name>__<tool-name>`. Both names here are `adamite`. A whole-server rule such as `mcp__adamite__*` also includes scanning; use the individual read-only rules above. See Claude Code's [plugin MCP documentation](https://code.claude.com/docs/en/mcp#plugin-provided-mcp-servers) and [MCP permission rules](https://code.claude.com/docs/en/permissions#mcp).
