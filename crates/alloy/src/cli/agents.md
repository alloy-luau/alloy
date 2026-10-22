# Alloy agent guide

`alloy init` wrote this file for coding agents. Read it before you change the code of this project.

## What Alloy is

Alloy is a strict superset of Luau: every Luau program is also an Alloy program. The compiler turns each `.aly` file into plain Luau, and each statement keeps its line. A Luau error or a stack trace therefore points at the right Alloy line. An `.alx` file is Alloy with markup in it, the way JSX sits in TypeScript.

Edit the `.aly` and `.alx` sources. Do not edit the Luau that the build writes.

## Layout

`alloy.toml` sets the layout of this project:

{layout}

`alloy doc config` explains each key of `alloy.toml`.

## Commands

{commands}

## Style

{style}

## Look things up

`alloy doc` lists every topic: the articles, the keywords, the operators, the std, and the lints. `alloy doc <topic>` prints one topic, for example `alloy doc match`, `alloy doc ?.`, or `alloy doc style`.

`alloy mcp` serves the same docs over MCP, the Model Context Protocol. Its tools are `search_docs`, `read_doc`, `list_topics`, and `style_guide`. Register it with Claude Code once:

```sh
claude mcp add alloy -- alloy mcp
```

Another MCP client starts the command `alloy mcp` over stdio, with no arguments.
