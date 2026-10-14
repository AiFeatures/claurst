// hooks_config_menu.rs — 4-screen read-only hooks browser.
//
// Mirrors the drill-down navigation of TS HooksConfigMenu.tsx:
//   Screen 1 SelectEvent   — list of hook events with count badges
//   Screen 2 SelectMatcher — matchers for the chosen event
//   Screen 3 SelectHook    — individual hooks for the chosen matcher
//   Screen 4 ViewHook      — full detail for a single hook
//
// The menu is intentionally read-only; as in the TS original, users edit
// ~/.claurst/settings.json directly or ask Claurst to change hooks. It lists
// the hooks the engine actually runs (#447): the effective settings hooks,
// then the hooks registered by plugins.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::overlays::{
    begin_modal_buf, modal_header_line_area, render_modal_title_buf, CLAURST_ACCENT,
    CLAURST_MUTED, CLAURST_PANEL_BG, CLAURST_TEXT,
};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// A single configured hook.
#[derive(Debug, Clone)]
pub struct HookEntry {
    /// Event name, e.g. "PreToolUse".
    pub event: String,
    /// Matcher pattern; "*" when the hook has none.
    pub matcher: String,
    /// The shell command.
    pub command: String,
    /// Timeout in seconds (configured or the default).
    pub timeout_secs: u64,
    /// "settings", or "plugin <name>".
    pub source: String,
}

impl HookEntry {
    /// Short one-line description of the hook shown in the list view.
    pub fn summary(&self) -> String {
        format!("\u{f120} {}", self.command)
    }
}

// ---------------------------------------------------------------------------
// Navigation mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HooksMenuMode {
    /// Screen 1: list of distinct event names.
    SelectEvent,
    /// Screen 2: matchers for `selected_event`.
    SelectMatcher,
    /// Screen 3: hooks for `selected_event` + `selected_matcher`.
    SelectHook,
    /// Screen 4: detail view for a single hook.
    ViewHook,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct HooksConfigMenuState {
    pub visible: bool,
    pub mode: HooksMenuMode,
    pub hooks: Vec<HookEntry>,
    /// All distinct event names (populated from `hooks`).
    pub events: Vec<String>,
    /// Selected index within the current list (reused across screens).
    pub selected: usize,
    pub scroll_offset: usize,
    /// Drilled-down selection breadcrumb.
    pub selected_event: Option<String>,
    pub selected_matcher: Option<String>,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl HooksConfigMenuState {
    pub fn new() -> Self {
        Self {
            visible: false,
            mode: HooksMenuMode::SelectEvent,
            hooks: Vec::new(),
            events: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            selected_event: None,
            selected_matcher: None,
        }
    }

    /// Open the menu at the event list, showing the effective settings
    /// hooks `hooks` followed by the hooks registered by plugins.
    pub fn open(&mut self, hooks: &claurst_core::config::HookMap) {
        self.mode = HooksMenuMode::SelectEvent;
        self.selected = 0;
        self.scroll_offset = 0;
        self.selected_event = None;
        self.selected_matcher = None;
        self.hooks.clear();
        self.load_hooks(hooks, "settings");
        if let Some(plugin_hooks) = claurst_core::hooks::plugin_hooks() {
            self.load_hooks(&plugin_hooks, "plugin");
        }
        self.build_events();
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Navigate into the selected item (Enter key).
    pub fn enter(&mut self) {
        match &self.mode {
            HooksMenuMode::SelectEvent => {
                if let Some(ev) = self.events.get(self.selected) {
                    self.selected_event = Some(ev.clone());
                    self.mode = HooksMenuMode::SelectMatcher;
                    self.selected = 0;
                    self.scroll_offset = 0;
                }
            }
            HooksMenuMode::SelectMatcher => {
                let matchers = self.matchers_for_event();
                if let Some(m) = matchers.get(self.selected) {
                    self.selected_matcher = Some(m.clone());
                    self.mode = HooksMenuMode::SelectHook;
                    self.selected = 0;
                    self.scroll_offset = 0;
                }
            }
            HooksMenuMode::SelectHook => {
                let hooks = self.hooks_for_selection();
                if hooks.get(self.selected).is_some() {
                    self.mode = HooksMenuMode::ViewHook;
                    self.scroll_offset = 0;
                }
            }
            HooksMenuMode::ViewHook => {} // no deeper level
        }
    }

    /// Navigate back one level (Esc key).
    pub fn back(&mut self) {
        match self.mode {
            HooksMenuMode::SelectEvent => { self.close(); }
            HooksMenuMode::SelectMatcher => {
                self.mode = HooksMenuMode::SelectEvent;
                self.selected = self.events.iter()
                    .position(|e| Some(e) == self.selected_event.as_ref())
                    .unwrap_or(0);
                self.selected_event = None;
                self.scroll_offset = 0;
            }
            HooksMenuMode::SelectHook => {
                self.mode = HooksMenuMode::SelectMatcher;
                let matchers = self.matchers_for_event();
                self.selected = matchers.iter()
                    .position(|m| Some(m) == self.selected_matcher.as_ref())
                    .unwrap_or(0);
                self.selected_matcher = None;
                self.scroll_offset = 0;
            }
            HooksMenuMode::ViewHook => {
                self.mode = HooksMenuMode::SelectHook;
                self.scroll_offset = 0;
            }
        }
    }

    pub fn select_prev(&mut self) {
        let count = match self.mode {
            HooksMenuMode::SelectEvent => self.events.len(),
            HooksMenuMode::SelectMatcher => self.matchers_for_event().len(),
            HooksMenuMode::SelectHook => self.hooks_for_selection().len(),
            HooksMenuMode::ViewHook => 0,
        };
        if count == 0 {
            return;
        }
        if self.selected == 0 {
            self.selected = count - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn select_next(&mut self) {
        let count = match self.mode {
            HooksMenuMode::SelectEvent   => self.events.len(),
            HooksMenuMode::SelectMatcher => self.matchers_for_event().len(),
            HooksMenuMode::SelectHook    => self.hooks_for_selection().len(),
            HooksMenuMode::ViewHook      => 0,
        };
        if count == 0 {
            return;
        }
        self.selected = (self.selected + 1) % count;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self, max: usize) {
        if self.scroll_offset + 1 < max {
            self.scroll_offset += 1;
        }
    }

    // ---- Private helpers --------------------------------------------------

    fn build_events(&mut self) {
        let mut seen = Vec::new();
        for h in &self.hooks {
            if !seen.contains(&h.event) {
                seen.push(h.event.clone());
            }
        }
        // Canonical event order (as documented).
        seen.sort_by_key(|e| {
            claurst_core::config::HookEvent::ALL
                .iter()
                .position(|o| o.as_str() == e.as_str())
                .unwrap_or(usize::MAX)
        });
        self.events = seen;
    }

    fn matchers_for_event(&self) -> Vec<String> {
        let ev = match &self.selected_event {
            Some(e) => e.as_str(),
            None    => return Vec::new(),
        };
        let mut seen: Vec<String> = Vec::new();
        for h in &self.hooks {
            if h.event == ev && !seen.contains(&h.matcher) {
                seen.push(h.matcher.clone());
            }
        }
        seen
    }

    fn hooks_for_selection(&self) -> Vec<&HookEntry> {
        let ev = self.selected_event.as_deref().unwrap_or("");
        let mt = self.selected_matcher.as_deref().unwrap_or("");
        self.hooks.iter().filter(|h| h.event == ev && h.matcher == mt).collect()
    }

    fn hook_count_for_event(&self, event: &str) -> usize {
        self.hooks.iter().filter(|h| h.event == event).count()
    }

    fn hook_count_for_matcher(&self, event: &str, matcher: &str) -> usize {
        self.hooks.iter().filter(|h| h.event == event && h.matcher == matcher).count()
    }

    fn load_hooks(&mut self, hooks: &claurst_core::config::HookMap, origin: &str) {
        use claurst_core::config::HookEvent;
        for event in HookEvent::ALL {
            for group in hooks.get(&event).into_iter().flatten() {
                let matcher = group
                    .matcher
                    .as_deref()
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or("*")
                    .to_string();
                for hook in &group.hooks {
                    let source = match &hook.plugin {
                        Some(plugin) => format!("plugin {}", plugin.name),
                        None => origin.to_string(),
                    };
                    self.hooks.push(HookEntry {
                        event: event.as_str().to_string(),
                        matcher: matcher.clone(),
                        command: hook.command.clone(),
                        timeout_secs: hook
                            .timeout
                            .unwrap_or(claurst_core::hooks::DEFAULT_HOOK_TIMEOUT_SECS),
                        source,
                    });
                }
            }
        }
    }
}

impl Default for HooksConfigMenuState {
    fn default() -> Self { Self::new() }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

pub fn render_hooks_config_menu(
    state: &HooksConfigMenuState,
    area: Rect,
    buf: &mut Buffer,
) {
    if !state.visible { return; }

    let layout = begin_modal_buf(buf, area, 80, 28, 2, 1);
    let inner_h = layout.body_area.height as usize;

    let (title, lines) = match state.mode {
        HooksMenuMode::SelectEvent   => render_event_list(state),
        HooksMenuMode::SelectMatcher => render_matcher_list(state),
        HooksMenuMode::SelectHook    => render_hook_list(state),
        HooksMenuMode::ViewHook      => render_hook_detail(state),
    };
    render_modal_title_buf(buf, layout.header_area, title.trim(), "esc");
    let breadcrumb = match state.mode {
        HooksMenuMode::SelectEvent => " Review configured hook events and matchers.".to_string(),
        HooksMenuMode::SelectMatcher => format!(
            " Event: {}",
            state.selected_event.as_deref().unwrap_or("?")
        ),
        HooksMenuMode::SelectHook => format!(
            " {} / {}",
            state.selected_event.as_deref().unwrap_or("?"),
            state.selected_matcher.as_deref().unwrap_or("?")
        ),
        HooksMenuMode::ViewHook => " Hook details are read-only for now.".to_string(),
    };
    if let Some(subtitle_area) = modal_header_line_area(layout.header_area, 1) {
        Paragraph::new(Line::from(vec![Span::styled(
            breadcrumb,
            Style::default().fg(CLAURST_MUTED),
        )]))
        .render(subtitle_area, buf);
    }

    let total = lines.len();
    let max_scroll = total.saturating_sub(inner_h);
    let scroll = state.scroll_offset.min(max_scroll) as u16;
    Paragraph::new(lines)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .style(Style::default().bg(CLAURST_PANEL_BG))
        .render(layout.body_area, buf);
    let footer = match state.mode {
        HooksMenuMode::SelectEvent => " enter drill  ·  esc close",
        HooksMenuMode::SelectMatcher => " enter drill  ·  esc back",
        HooksMenuMode::SelectHook => " enter view  ·  esc back",
        HooksMenuMode::ViewHook => " esc back",
    };
    Paragraph::new(Line::from(vec![Span::styled(
        footer,
        Style::default().fg(CLAURST_MUTED).add_modifier(Modifier::ITALIC),
    )]))
    .render(layout.footer_area, buf);
}

// ---- Screen 1: event list -------------------------------------------------

fn render_event_list(state: &HooksConfigMenuState) -> (&'static str, Vec<Line<'static>>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));

    if state.events.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No hooks configured.",
            Style::default().fg(Color::DarkGray),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Edit ~/.claurst/settings.json to add hooks.",
            Style::default().fg(Color::DarkGray),
        )]));
    } else {
        for (i, event) in state.events.iter().enumerate() {
            let selected = i == state.selected;
            let count = state.hook_count_for_event(event);
            push_list_row(&mut lines, event, &format!("{count} hook{}", if count == 1 { "" } else { "s" }), selected);
        }
    }

    (" Hooks — Select Event ", lines)
}

// ---- Screen 2: matcher list -----------------------------------------------

fn render_matcher_list(state: &HooksConfigMenuState) -> (&'static str, Vec<Line<'static>>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let event = state.selected_event.as_deref().unwrap_or("?");

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Event: ", Style::default().fg(Color::DarkGray)),
        Span::styled(event.to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(""));

    let matchers = state.matchers_for_event();
    for (i, matcher) in matchers.iter().enumerate() {
        let selected = i == state.selected;
        let count = state.hook_count_for_matcher(event, matcher);
        push_list_row(&mut lines, matcher, &format!("{count} hook{}", if count == 1 { "" } else { "s" }), selected);
    }

    (" Hooks — Select Matcher ", lines)
}

// ---- Screen 3: hook list --------------------------------------------------

fn render_hook_list(state: &HooksConfigMenuState) -> (&'static str, Vec<Line<'static>>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let event   = state.selected_event.as_deref().unwrap_or("?");
    let matcher = state.selected_matcher.as_deref().unwrap_or("?");

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(event.to_string(), Style::default().fg(Color::Cyan)),
        Span::styled(" / ", Style::default().fg(Color::DarkGray)),
        Span::styled(matcher.to_string(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(""));

    let hooks = state.hooks_for_selection();
    for (i, hook) in hooks.iter().enumerate() {
        let selected = i == state.selected;
        push_list_row(&mut lines, &hook.summary(), &hook.source, selected);
    }

    (" Hooks — Select Hook ", lines)
}

// ---- Screen 4: hook detail ------------------------------------------------

fn render_hook_detail(state: &HooksConfigMenuState) -> (&'static str, Vec<Line<'static>>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let hooks = state.hooks_for_selection();
    let hook = match hooks.get(state.selected) {
        Some(h) => h,
        None    => {
            lines.push(Line::from(vec![Span::styled("  Hook not found.", Style::default().fg(Color::Red))]));
            return (" Hook Detail ", lines);
        }
    };

    lines.push(Line::from(""));
    push_detail_row(&mut lines, "Event",   &hook.event);
    push_detail_row(&mut lines, "Matcher", &hook.matcher);
    push_detail_row(&mut lines, "Timeout", &format!("{}s", hook.timeout_secs));
    push_detail_row(&mut lines, "Source",  &hook.source);
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  Command:",
        Style::default().fg(Color::DarkGray),
    )]));
    // Wrap long commands across multiple lines
    for chunk in hook.command.chars().collect::<Vec<_>>().chunks(60) {
        let text: String = chunk.iter().collect();
        lines.push(Line::from(vec![Span::styled(
            format!("    {text}"),
            Style::default().fg(Color::White),
        )]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  Edit ~/.claurst/settings.json to modify hooks.",
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
    )]));
    (" Hook Detail ", lines)
}

// ---- Line helpers ----------------------------------------------------------

fn push_list_row(lines: &mut Vec<Line<'static>>, label: &str, badge: &str, selected: bool) {
    let bg = if selected { CLAURST_ACCENT } else { CLAURST_PANEL_BG };
    let row_style = if selected {
        Style::default().fg(Color::White).bg(bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(CLAURST_TEXT).bg(bg)
    };
    let badge_style = if selected {
        Style::default().fg(Color::Rgb(248, 220, 236)).bg(bg)
    } else {
        Style::default().fg(CLAURST_MUTED).bg(bg)
    };
    lines.push(Line::from(vec![
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled(format!("{:<42}", label), row_style),
        Span::styled(badge.to_string(), badge_style),
    ]));
}

fn push_detail_row(lines: &mut Vec<Line<'static>>, key: &str, value: &str) {
    lines.push(Line::from(vec![
        Span::styled(format!("  {key:<10}  "), Style::default().fg(Color::DarkGray)),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ]));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn hooks_menu_lists_effective_config_hooks() {
        use claurst_core::config::{HookCommand, HookEvent, HookMap, HookMatcher, HookType};
        let command = |command: &str, timeout| HookCommand {
            kind: HookType::Command,
            command: command.to_string(),
            timeout,
            plugin: None,
        };
        let mut hooks = HookMap::new();
        hooks.insert(
            HookEvent::Stop,
            vec![HookMatcher { matcher: None, hooks: vec![command("notify-done", None)] }],
        );
        hooks.insert(
            HookEvent::PreToolUse,
            vec![HookMatcher { matcher: Some("Edit|Write".into()), hooks: vec![command("fmt", Some(5))] }],
        );
        let mut state = HooksConfigMenuState::new();
        state.open(&hooks);

        assert_eq!(state.events, vec!["PreToolUse".to_string(), "Stop".to_string()]);
        let pre = &state.hooks[0];
        assert_eq!((pre.matcher.as_str(), pre.command.as_str(), pre.timeout_secs), ("Edit|Write", "fmt", 5));
        assert_eq!(pre.source, "settings");
        let stop = &state.hooks[1];
        assert_eq!((stop.matcher.as_str(), stop.timeout_secs), ("*", 60));
    }

    #[test]
    fn hooks_menu_renders_opaque_event_list() {
        let mut state = HooksConfigMenuState::new();
        state.visible = true;
        state.hooks = vec![HookEntry {
            event: "PreToolUse".to_string(),
            matcher: "Bash".to_string(),
            command: "echo hi".to_string(),
            timeout_secs: 60,
            source: "settings".to_string(),
        }];
        state.build_events();

        let area = Rect { x: 0, y: 0, width: 90, height: 28 };
        let mut buf = Buffer::empty(area);
        render_hooks_config_menu(&state, area, &mut buf);

        let rendered = buf
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");
        assert!(rendered.contains("Hooks"));
        assert!(rendered.contains("PreToolUse"));
    }
}

