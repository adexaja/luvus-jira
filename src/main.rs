use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;
use std::{
    env, io,
    process::{Command, ExitCode},
};

const DOCK_ID: &str = "jira-ticket";

#[derive(Clone, Debug, Deserialize)]
struct Ticket {
    key: String,
    fields: Fields,
}

#[derive(Clone, Debug, Deserialize)]
struct Fields {
    summary: String,
    status: StatusValue,
    issuetype: NamedValue,
    priority: Option<NamedValue>,
    description: Option<serde_json::Value>,
    attachments: Option<Vec<Attachment>>,
}

#[derive(Clone, Debug, Deserialize)]
struct Attachment {
    filename: String,
    content: String,
}

#[derive(Clone, Debug, Deserialize)]
struct NamedValue {
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
struct StatusValue {
    name: String,
    #[serde(rename = "statusCategory")]
    category: Option<StatusCategory>,
}

#[derive(Clone, Debug, Deserialize)]
struct StatusCategory {
    key: String,
}

#[derive(Debug, Deserialize)]
struct TransitionResponse {
    transitions: Vec<Transition>,
}

#[derive(Debug, Deserialize)]
struct Transition {
    id: String,
    to: StatusValue,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    issues: Vec<Ticket>,
}

#[derive(Debug, Deserialize)]
struct ProjectSearchResponse {
    values: Vec<Project>,
}

#[derive(Debug, Deserialize)]
struct Project {
    key: String,
}

#[derive(Debug, Deserialize)]
struct ProjectIssueTypeStatuses {
    statuses: Vec<NamedValue>,
}

fn setting(name: &str) -> Result<String, String> {
    env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("Missing setting: {name}"))
}

fn jira_request(path: &str, query: &[(&str, &str)]) -> Result<reqwest::blocking::Response, String> {
    let base = setting("LUVUS_SETTING_JIRA_BASE_URL")?
        .trim_end_matches('/')
        .to_owned();
    let email = setting("LUVUS_SETTING_JIRA_EMAIL")?;
    let token = setting("LUVUS_SETTING_JIRA_API_TOKEN")?;
    Client::new()
        .get(format!("{base}{path}"))
        .query(query)
        .header(
            "Authorization",
            format!("Basic {}", BASE64.encode(format!("{email}:{token}"))),
        )
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("Jira request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Jira returned an error: {e}"))
}

fn fetch_tickets() -> Result<Vec<Ticket>, String> {
    let jql = setting("LUVUS_SETTING_JIRA_JQL")?;
    jira_request(
        "/rest/api/3/search/jql",
        &[
            ("jql", &jql),
            ("maxResults", "50"),
            (
                "fields",
                "summary,status,issuetype,priority,description,attachment",
            ),
        ],
    )?
    .json::<SearchResponse>()
    .map(|response| response.issues)
    .map_err(|e| format!("Invalid Jira response: {e}"))
}

fn description_text(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::Object(object) => {
            let kind = object.get("type").and_then(serde_json::Value::as_str);
            if kind == Some("hardBreak") {
                output.push('\n');
                return;
            }
            if kind == Some("listItem") {
                output.push_str("- ");
            }
            if let Some(serde_json::Value::String(text)) = object.get("text") {
                output.push_str(text);
            }
            if let Some(serde_json::Value::Array(content)) = object.get("content") {
                for child in content {
                    description_text(child, output);
                }
            }
            if matches!(
                kind,
                Some(
                    "paragraph"
                        | "heading"
                        | "listItem"
                        | "codeBlock"
                        | "blockquote"
                        | "bulletList"
                        | "orderedList"
                )
            ) {
                output.push('\n');
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                description_text(value, output);
            }
        }
        _ => {}
    }
}

fn fetch_projects() -> Result<Vec<String>, String> {
    jira_request("/rest/api/3/project/search", &[("maxResults", "1000")])?
        .json::<ProjectSearchResponse>()
        .map(|response| {
            response
                .values
                .into_iter()
                .map(|project| project.key)
                .collect()
        })
        .map_err(|e| format!("Invalid Jira project response: {e}"))
}

fn fetch_names(path: &str) -> Result<Vec<String>, String> {
    jira_request(path, &[])?
        .json::<Vec<NamedValue>>()
        .map(|values| values.into_iter().map(|value| value.name).collect())
        .map_err(|e| format!("Invalid Jira option response: {e}"))
}

fn fetch_project_statuses(project: &str) -> Result<Vec<String>, String> {
    jira_request(&format!("/rest/api/3/project/{project}/statuses"), &[])?
        .json::<Vec<ProjectIssueTypeStatuses>>()
        .map(|groups| {
            unique_values(
                groups
                    .into_iter()
                    .flat_map(|group| group.statuses.into_iter().map(|status| status.name)),
            )
        })
        .map_err(|e| format!("Invalid Jira project status response: {e}"))
}

fn unique_values(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    values
}

fn status_options(all: &[Ticket], project: &str, fallback: &[String]) -> Vec<String> {
    let values = unique_values(all.iter().filter_map(|ticket| {
        let key_project = ticket.key.split('-').next().unwrap_or_default();
        (project.is_empty() || key_project.eq_ignore_ascii_case(project))
            .then_some(ticket.fields.status.name.clone())
    }));

    if values.is_empty() {
        fallback.to_vec()
    } else {
        values
    }
}

fn status_color(status: &StatusValue) -> Color {
    if let Some(category) = &status.category {
        return match category.key.as_str() {
            "done" => Color::Green,
            "indeterminate" => Color::Yellow,
            "new" => Color::Cyan,
            _ => Color::White,
        };
    }
    let name = status.name.to_lowercase();
    if ["done", "closed", "resolved"]
        .iter()
        .any(|value| name.contains(value))
    {
        Color::Green
    } else if ["progress", "review", "validation"]
        .iter()
        .any(|value| name.contains(value))
    {
        Color::Yellow
    } else if ["blocked", "rejected", "cancelled"]
        .iter()
        .any(|value| name.contains(value))
    {
        Color::Red
    } else if ["to do", "open", "backlog"]
        .iter()
        .any(|value| name.contains(value))
    {
        Color::Cyan
    } else {
        Color::White
    }
}

fn ticket_description(ticket: &Ticket) -> String {
    let mut text = String::new();
    if let Some(description) = &ticket.fields.description {
        description_text(description, &mut text);
    }
    text.trim().to_owned()
}

fn ticket_attachments(ticket: &Ticket) -> String {
    ticket
        .fields
        .attachments
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|attachment| format!("- {}: {}", attachment.filename, attachment.content))
        .collect::<Vec<_>>()
        .join("\n")
}
fn ticket_url(ticket: &Ticket) -> Result<String, String> {
    Ok(format!(
        "{}/browse/{}",
        setting("LUVUS_SETTING_JIRA_BASE_URL")?.trim_end_matches('/'),
        ticket.key
    ))
}

fn available_transitions(ticket: &Ticket) -> Result<Vec<Transition>, String> {
    jira_request(
        &format!("/rest/api/3/issue/{}/transitions", ticket.key),
        &[],
    )?
    .json::<TransitionResponse>()
    .map(|response| response.transitions)
    .map_err(|e| format!("Invalid Jira transition response: {e}"))
}

fn apply_transition(ticket: &Ticket, transition: &Transition) -> Result<String, String> {
    let base = setting("LUVUS_SETTING_JIRA_BASE_URL")?
        .trim_end_matches('/')
        .to_owned();
    let email = setting("LUVUS_SETTING_JIRA_EMAIL")?;
    let token = setting("LUVUS_SETTING_JIRA_API_TOKEN")?;
    Client::new()
        .post(format!(
            "{base}/rest/api/3/issue/{}/transitions",
            ticket.key
        ))
        .header(
            "Authorization",
            format!("Basic {}", BASE64.encode(format!("{email}:{token}"))),
        )
        .header("Accept", "application/json")
        .json(&json!({"transition": {"id": transition.id}}))
        .send()
        .map_err(|e| format!("Jira transition failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Jira rejected the status transition: {e}"))?;
    Ok(transition.to.name.clone())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterKind {
    Search,
    Project,
    Status,
    Priority,
}

struct Filters {
    search: String,
    project: String,
    status: String,
    priority: String,
    active: Option<FilterKind>,
    input: String,
}

impl Filters {
    fn new() -> Self {
        Self {
            search: String::new(),
            project: String::new(),
            status: String::new(),
            priority: String::new(),
            active: None,
            input: String::new(),
        }
    }

    fn with_project(project: String) -> Self {
        let mut filters = Self::new();
        filters.project = project;
        filters
    }

    fn begin(&mut self, kind: FilterKind) {
        self.active = Some(kind);
        self.input = match kind {
            FilterKind::Search => self.search.clone(),
            FilterKind::Project => self.project.clone(),
            FilterKind::Status => self.status.clone(),
            FilterKind::Priority => self.priority.clone(),
        };
    }

    fn commit(&mut self) {
        if let Some(kind) = self.active.take() {
            match kind {
                FilterKind::Search => self.search = self.input.clone(),
                FilterKind::Project => self.project = self.input.clone(),
                FilterKind::Status => self.status = self.input.clone(),
                FilterKind::Priority => self.priority = self.input.clone(),
            }
        }
        self.input.clear();
    }

    fn value(&self, kind: FilterKind) -> &str {
        if self.active == Some(kind) {
            &self.input
        } else {
            match kind {
                FilterKind::Search => &self.search,
                FilterKind::Project => &self.project,
                FilterKind::Status => &self.status,
                FilterKind::Priority => &self.priority,
            }
        }
    }
}

fn filter_tickets(all: &[Ticket], filters: &Filters) -> Vec<Ticket> {
    let contains = |value: &str, filter: &str| {
        filter.is_empty() || value.to_lowercase().contains(&filter.to_lowercase())
    };
    all.iter()
        .filter(|ticket| {
            let project = ticket.key.split('-').next().unwrap_or_default();
            let priority = ticket
                .fields
                .priority
                .as_ref()
                .map(|value| value.name.as_str())
                .unwrap_or("None");
            let search_text = format!(
                "{} {} {}",
                ticket.key,
                ticket.fields.summary,
                ticket_description(ticket)
            );
            contains(&search_text, &filters.search)
                && contains(project, &filters.project)
                && contains(&ticket.fields.status.name, &filters.status)
                && contains(priority, &filters.priority)
        })
        .cloned()
        .collect()
}

fn cycle_filter(filters: &mut Filters, kind: FilterKind, options: &[String]) {
    if options.is_empty() {
        return;
    }
    let current = filters.value(kind);
    let next = current
        .is_empty()
        .then_some(0)
        .or_else(|| {
            options
                .iter()
                .position(|value| value == current)
                .map(|index| (index + 1) % options.len())
        })
        .unwrap_or(0);
    match kind {
        FilterKind::Search => filters.search = options[next].clone(),
        FilterKind::Project => filters.project = options[next].clone(),
        FilterKind::Status => filters.status = options[next].clone(),
        FilterKind::Priority => filters.priority = options[next].clone(),
    }
}
fn run_tui() -> Result<(), String> {
    if let Ok(workspace_cwd) = env::var("LUVUS_WORKSPACE_CWD") {
        env::set_current_dir(&workspace_cwd)
            .map_err(|e| format!("Unable to enter workspace: {e}"))?;
    }
    let all_tickets = fetch_tickets()?;
    let projects = fetch_projects().unwrap_or_else(|_| {
        unique_values(
            all_tickets
                .iter()
                .filter_map(|ticket| ticket.key.split('-').next().map(str::to_owned)),
        )
    });
    let statuses = fetch_names("/rest/api/3/status").unwrap_or_else(|_| {
        unique_values(
            all_tickets
                .iter()
                .map(|ticket| ticket.fields.status.name.clone()),
        )
    });
    let priorities = fetch_names("/rest/api/3/priority").unwrap_or_else(|_| {
        unique_values(all_tickets.iter().map(|ticket| {
            ticket
                .fields
                .priority
                .as_ref()
                .map(|value| value.name.clone())
                .unwrap_or_else(|| "None".to_owned())
        }))
    });
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let default_project = env::var("LUVUS_SETTING_JIRA_PROJECT_KEY").unwrap_or_default();
    let result = tui_loop(all_tickets, projects, statuses, priorities, default_project);
    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();
    result
}

fn tui_loop(
    mut all_tickets: Vec<Ticket>,
    projects: Vec<String>,
    statuses: Vec<String>,
    priorities: Vec<String>,
    default_project: String,
) -> Result<(), String> {
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    let mut filters = Filters::with_project(default_project);
    let mut tickets = filter_tickets(&all_tickets, &filters);
    let mut selected = 0usize;
    let mut detail = true;
    let mut list_state = ListState::default();
    let mut notice = String::new();
    let mut pending_transitions: Option<Vec<Transition>> = None;
    let mut transition_index = 0usize;
    loop {
        terminal
            .draw(|frame| {
                let root = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Length(3),
                        Constraint::Min(1),
                    ])
                    .split(frame.area());
                frame.render_widget(
                    Paragraph::new(filters.value(FilterKind::Search))
                        .style(Style::default().fg(Color::White))
                        .block(Block::default().title(" Search (/ to edit) ").borders(Borders::ALL)),
                    root[0],
                );
                let filters_text = format!(
                    " Project [ {} ]   Status [ {} ]   Priority [ {} ]   (p/s/y: select, x: clear, r: refresh)",
                    filters.value(FilterKind::Project),
                    filters.value(FilterKind::Status),
                    filters.value(FilterKind::Priority),
                );
                frame.render_widget(
                    Paragraph::new(filters_text)
                        .style(Style::default().fg(Color::Cyan))
                        .block(Block::default().title(" Filters ").borders(Borders::ALL)),
                    root[1],
                );
                let areas = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(root[2]);
                let items = tickets
                    .iter()
                    .map(|ticket| ListItem::new(format!("{}  {}", ticket.key, ticket.fields.summary)))
                    .collect::<Vec<_>>();
                list_state.select(tickets.get(selected).map(|_| selected));
                let list = List::new(items)
                    .block(Block::default().title(" Jira tickets ").borders(Borders::ALL))
                    .highlight_symbol("▶ ")
                    .highlight_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    );
                frame.render_stateful_widget(list, areas[0], &mut list_state);
                if let Some(ticket) = tickets.get(selected) {
                    let label = Style::default().add_modifier(ratatui::style::Modifier::BOLD);
                    let action = Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD);
                    let text = if detail {
                        let mut lines = vec![
                            Line::from(Span::styled(
                                format!("{}: {}", ticket.key, ticket.fields.summary),
                                Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                            )),
                            Line::from(""),
                            Line::from(vec![
                                Span::styled("Status: ", label),
                                Span::styled(
                                    ticket.fields.status.name.clone(),
                                    Style::default().fg(status_color(&ticket.fields.status)),
                                ),
                            ]),
                            Line::from(vec![
                                Span::styled("Type: ", label),
                                Span::raw(ticket.fields.issuetype.name.clone()),
                            ]),
                            Line::from(vec![
                                Span::styled("Priority: ", label),
                                Span::raw(
                                    ticket
                                        .fields
                                        .priority
                                        .as_ref()
                                        .map(|p| p.name.clone())
                                        .unwrap_or_else(|| "None".to_owned()),
                                ),
                            ]),
                            Line::from(""),
                            Line::from(Span::styled("Description", label)),
                        ];
                        let description = ticket_description(ticket);
                        lines.extend(description.lines().map(|line| Line::from(line.to_owned())));
                        lines.push(Line::from(""));
                        lines.push(Line::from(Span::styled("Attachments", label)));
                        let attachments = ticket_attachments(ticket);
                        lines.extend(attachments.lines().map(|line| Line::from(line.to_owned())));
                        lines.push(Line::from(""));
                        lines.push(Line::from(vec![
                            Span::styled("[t] Switch status", action),
                            Span::raw("  "),
                            Span::styled("[w] Work this ticket", action),
                            Span::raw("  "),
                            Span::styled("[o] Open in browser", action),
                            Span::raw("  [Esc] Back"),
                        ]));
                        if let Some(transitions) = &pending_transitions {
                            if let Some(transition) = transitions.get(transition_index) {
                                lines.push(Line::from(vec![
                                    Span::styled(
                                        "Confirm status change to ",
                                        Style::default().fg(Color::Yellow),
                                    ),
                                    Span::styled(
                                        transition.to.name.clone(),
                                        Style::default()
                                            .fg(status_color(&transition.to))
                                            .add_modifier(ratatui::style::Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        "? [Enter] confirm  [Esc] cancel  [↑/↓] choose",
                                        Style::default().fg(Color::Yellow),
                                    ),
                                ]));
                            }
                        }
                        if !notice.is_empty() {
                            let notice_color = if notice.starts_with("Status switched")
                                || notice.starts_with("Started agent")
                            {
                                Color::Green
                            } else {
                                Color::Red
                            };
                            lines.push(Line::from(Span::styled(
                                notice.clone(),
                                Style::default().fg(notice_color),
                            )));
                        }
                        Text::from(lines)
                    } else {
                        Text::from("Select a ticket and press Enter for details.")
                    };
                    frame.render_widget(
                        Paragraph::new(text)
                            .wrap(Wrap { trim: true })
                            .block(Block::default().title(" Details ").borders(Borders::ALL)),
                        areas[1],
                    );
                }
            })
            .map_err(|e| e.to_string())?;
        if event::poll(std::time::Duration::from_millis(250)).map_err(|e| e.to_string())? {
            if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                if filters.active.is_some() {
                    match key.code {
                        KeyCode::Enter => {
                            filters.commit();
                            tickets = filter_tickets(&all_tickets, &filters);
                            selected = 0;
                        }
                        KeyCode::Esc => filters.active = None,
                        KeyCode::Backspace => {
                            filters.input.pop();
                        }
                        KeyCode::Char(character) => filters.input.push(character),
                        _ => {}
                    }
                    continue;
                }
                if pending_transitions.is_some() {
                    match key.code {
                        KeyCode::Esc => pending_transitions = None,
                        KeyCode::Up | KeyCode::Char('k') => {
                            if let Some(transitions) = &pending_transitions {
                                transition_index = transition_index
                                    .checked_sub(1)
                                    .unwrap_or(transitions.len().saturating_sub(1));
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if let Some(transitions) = &pending_transitions {
                                transition_index =
                                    (transition_index + 1) % transitions.len().max(1);
                            }
                        }
                        KeyCode::Enter => {
                            if let (Some(transitions), Some(ticket)) =
                                (&pending_transitions, tickets.get(selected).cloned())
                            {
                                if let Some(transition) = transitions.get(transition_index) {
                                    match apply_transition(&ticket, transition) {
                                        Ok(result) => {
                                            let new_status = transition.to.name.clone();
                                            notice = format!("Status switched to {result}");
                                            if let Some(current) = tickets.get_mut(selected) {
                                                current.fields.status.name = new_status.clone();
                                            }
                                            for current in &mut all_tickets {
                                                if current.key == ticket.key {
                                                    current.fields.status.name = new_status.clone();
                                                }
                                            }
                                            pending_transitions = None;
                                        }
                                        Err(error) => {
                                            notice = format!("Status switch failed: {error}");
                                            pending_transitions = None;
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Esc if detail => detail = false,
                    KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') if !tickets.is_empty() => {
                        selected = (selected + 1).min(tickets.len() - 1);
                        notice.clear();
                    }
                    KeyCode::Up | KeyCode::Char('k') if !tickets.is_empty() => {
                        selected = selected.saturating_sub(1);
                        notice.clear();
                    }
                    KeyCode::Enter => detail = true,
                    KeyCode::Char('/') => filters.begin(FilterKind::Search),
                    KeyCode::Char('p') => {
                        cycle_filter(&mut filters, FilterKind::Project, &projects);
                        let options = if filters.project.is_empty() {
                            statuses.clone()
                        } else {
                            fetch_project_statuses(&filters.project).unwrap_or_else(|_| {
                                status_options(&all_tickets, &filters.project, &statuses)
                            })
                        };
                        if !filters.status.is_empty()
                            && !options.iter().any(|value| value == &filters.status)
                        {
                            filters.status.clear();
                        }
                        tickets = filter_tickets(&all_tickets, &filters);
                        selected = 0;
                    }
                    KeyCode::Char('s') => {
                        let options = if filters.project.is_empty() {
                            statuses.clone()
                        } else {
                            fetch_project_statuses(&filters.project).unwrap_or_else(|_| {
                                status_options(&all_tickets, &filters.project, &statuses)
                            })
                        };
                        cycle_filter(&mut filters, FilterKind::Status, &options);
                        tickets = filter_tickets(&all_tickets, &filters);
                        selected = 0;
                    }
                    KeyCode::Char('y') => {
                        cycle_filter(&mut filters, FilterKind::Priority, &priorities);
                        tickets = filter_tickets(&all_tickets, &filters);
                        selected = 0;
                    }
                    KeyCode::Char('x') => {
                        filters = Filters::new();
                        tickets = filter_tickets(&all_tickets, &filters);
                        selected = 0;
                    }
                    KeyCode::Char('r') => {
                        all_tickets = fetch_tickets()?;
                        tickets = filter_tickets(&all_tickets, &filters);
                    }
                    KeyCode::Char('o') if detail => {
                        if let Some(ticket) = tickets.get(selected) {
                            open_url(&ticket_url(ticket)?)?;
                        }
                    }
                    KeyCode::Char('t') if detail => {
                        if let Some(ticket) = tickets.get(selected).cloned() {
                            match available_transitions(&ticket) {
                                Ok(transitions) if transitions.is_empty() => {
                                    notice = "No status transition is available".to_owned();
                                }
                                Ok(transitions) => {
                                    transition_index = 0;
                                    pending_transitions = Some(transitions);
                                    notice.clear();
                                }
                                Err(error) => {
                                    notice = format!("Status switch failed: {error}");
                                }
                            }
                        }
                    }
                    KeyCode::Char('w') if detail => {
                        if let Some(ticket) = tickets.get(selected) {
                            match work_ticket(ticket) {
                                Ok(()) => {
                                    notice = format!("Started worktree agent for {}", ticket.key);
                                }
                                Err(error) => {
                                    notice = format!("Work handoff failed: {error}");
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn open_url(url: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(program)
        .arg(url)
        .status()
        .map_err(|e| e.to_string())?
        .success()
        .then_some(())
        .ok_or_else(|| "URL opener failed".to_owned())
}

fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("exit status {}", output.status)
}

fn focus_workspace(bin: &str) -> Result<(), String> {
    let id = env::var("LUVUS_WORKSPACE_ID").ok();
    let Some(id) = id else {
        return Ok(());
    };
    let listed = Command::new(bin)
        .args(["workspace", "list"])
        .output()
        .map_err(|e| format!("Unable to list Luvus workspaces: {e}"))?;
    if !listed.status.success() {
        return Err(format!(
            "Unable to list Luvus workspaces: {}",
            command_error(&listed)
        ));
    }
    let response = serde_json::from_slice::<serde_json::Value>(&listed.stdout)
        .map_err(|e| format!("Luvus returned invalid workspace response: {e}"))?;
    let index = response["result"]["workspaces"]
        .as_array()
        .and_then(|workspaces| {
            workspaces.iter().find_map(|workspace| {
                (workspace["workspace_id"].as_str() == Some(&id))
                    .then(|| workspace["workspace"].as_str())
                    .flatten()
            })
        });
    let Some(index) = index else {
        let cwd = env::var("LUVUS_WORKSPACE_CWD")
            .map_err(|_| format!("Workspace {id} is no longer open"))?;
        let opened = Command::new(bin)
            .args(["workspace", "open", &cwd])
            .output()
            .map_err(|e| format!("Unable to reopen Jira workspace: {e}"))?;
        if !opened.status.success() {
            return Err(format!(
                "Unable to reopen Jira workspace: {}",
                command_error(&opened)
            ));
        }
        return Ok(());
    };
    let focused = Command::new(bin)
        .args(["workspace", "focus", index])
        .output()
        .map_err(|e| format!("Unable to focus Jira workspace: {e}"))?;
    if !focused.status.success() {
        return Err(format!(
            "Unable to focus Jira workspace: {}",
            command_error(&focused)
        ));
    }
    Ok(())
}

fn work_ticket(ticket: &Ticket) -> Result<(), String> {
    let bin = env::var("LUVUS_BIN_PATH").map_err(|_| "Missing LUVUS_BIN_PATH".to_owned())?;
    let prompt = format!(
        "Work on Jira ticket {}.\n\nJira reference: {}\nSummary: {}\nStatus: {}\nType: {}\nPriority: {}\n\nDescription:\n{}\n\nAttachments:\n{}\n\nUse the Jira ticket as the source of truth and implement the requested change in this workspace.",
        ticket.key,
        ticket_url(ticket)?,
        ticket.fields.summary,
        ticket.fields.status.name,
        ticket.fields.issuetype.name,
        ticket
            .fields
            .priority
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or("None"),
        ticket_description(ticket),
        ticket_attachments(ticket),
    );
    focus_workspace(&bin)?;
    let branch = format!("jira/{}", ticket.key.to_lowercase());
    let mut create = Command::new(&bin);
    create.args(["worktree", "create", &branch]);
    let output = create
        .output()
        .map_err(|e| format!("Unable to create Jira worktree: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Worktree creation failed: {}",
            command_error(&output)
        ));
    }

    let agent_kind = env::var("LUVUS_SETTING_JIRA_AGENT_KIND")
        .ok()
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or_else(|| "codex".to_owned());
    let agent_name = format!("jira-{}", ticket.key.to_lowercase());
    let mut start = Command::new(&bin);
    start.args([
        "agent",
        "start",
        &agent_name,
        "--kind",
        &agent_kind,
        "--timeout",
        "10",
    ]);
    start.env_remove("LUVUS_PANE_ID");
    let started = start
        .output()
        .map_err(|e| format!("Unable to start Jira agent: {e}"))?;
    let response = serde_json::from_slice::<serde_json::Value>(&started.stdout).ok();
    if response
        .as_ref()
        .and_then(|value| value.get("error"))
        .is_some()
    {
        return Err(format!(
            "Agent start failed: {}",
            response
                .as_ref()
                .and_then(|value| value["error"]["message"].as_str())
                .unwrap_or("unknown error")
        ));
    }
    if started.status.code() != Some(2) && !started.status.success() {
        return Err(format!("Agent start failed: {}", command_error(&started)));
    }
    let prompted = Command::new(&bin)
        .args(["agent", "prompt", &agent_name, &prompt])
        .output()
        .map_err(|e| format!("Unable to send Jira prompt: {e}"))?;
    if !prompted.status.success() {
        return Err(format!(
            "Jira agent started, but prompt failed: {}",
            command_error(&prompted)
        ));
    }
    let _ = Command::new(&bin)
        .args(["ui", "toast", &format!("Started Jira agent in {}", branch)])
        .status();
    Ok(())
}

fn publish(rows: serde_json::Value) -> Result<(), String> {
    let bin = env::var("LUVUS_BIN_PATH").map_err(|_| "Missing LUVUS_BIN_PATH".to_owned())?;
    let output = Command::new(bin)
        .args(["ui", "dock", "push", "--id", DOCK_ID, "--rows"])
        .arg(rows.to_string())
        .output()
        .map_err(|e| e.to_string())?;
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| String::from_utf8_lossy(&output.stderr).trim().to_owned())
}

fn run_dock() -> Result<(), String> {
    let tickets = fetch_tickets()?;
    let ticket = tickets
        .first()
        .ok_or_else(|| "No Jira tickets matched the JQL".to_owned())?;
    publish(json!([
        {"text": format!("{}: {}", ticket.key, ticket.fields.summary), "tone": "accent", "action": "open"},
        {"text": format!("{} · {}", ticket.fields.status.name, ticket.fields.issuetype.name), "tone": "normal"},
    ]))
}

fn open_tui_pane() -> Result<(), String> {
    let bin = env::var("LUVUS_BIN_PATH").map_err(|_| "Missing LUVUS_BIN_PATH".to_owned())?;
    Command::new(bin)
        .args([
            "module",
            "pane",
            "open",
            "adexaja.luvus-jira",
            "jira-board",
            "--placement",
            "tab",
        ])
        .status()
        .map_err(|e| e.to_string())?
        .success()
        .then_some(())
        .ok_or_else(|| "Luvus could not open the Jira pane".to_owned())
}

fn main() -> ExitCode {
    let result = match env::args().nth(1).as_deref() {
        Some("tui") => run_tui(),
        Some("open") => open_tui_pane(),
        _ => run_dock(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
