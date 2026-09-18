# Luvus Jira

A Luvus module for browsing Jira issues and handing them to an agent in a dedicated Git worktree.

## Features

- Jira issue dock and terminal UI.
- Project, status, and priority filters.
- Jira status transitions.
- Issue descriptions and attachments.
- Open issues in a browser.
- Work on an issue directly in a new worktree.

## Install

From GitHub:

```bash
luvus module install adexaja/luvus-jira
```

For local development:

```bash
cargo build --release
luvus module link /path/to/luvus-jira
```

## Configuration

Open **Settings → Modules → Jira Ticket** and set:

- **Jira base URL** — for example, `https://your-domain.atlassian.net`.
- **Jira project key** — optional default project filter.
- **Jira email**.
- **Jira API token** — stored as a secret.
- **Jira JQL** — default: `assignee = currentUser() ORDER BY updated DESC`.
- **Default agent kind** — defaults to `codex`.

## Usage

Open **Jira Board** from the workspace context menu or run:

```bash
luvus module run adexaja.luvus-jira open
```

Inside the board:

- `/` — edit the Jira JQL/filter query.
- `p` — cycle project filter.
- `s` — cycle status filter.
- `y` — cycle priority filter.
- `r` — refresh issues.
- `t` — switch the selected issue's Jira status.
- `w` — create a worktree and start the configured agent with the issue prompt.
- `o` — open the selected issue in a browser.
- `Esc` — go back.

The `w` flow creates a `jira/<issue-key>` branch, opens its worktree as a workspace, starts the configured agent there, and sends the Jira issue details as the prompt.
