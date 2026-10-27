//! Quick Commands overlay: list, filter, manage, and insert user-defined
//! commands into the active pane's input line (without executing them).
//!
//! The event loop runs inside a termwiz overlay (same mechanism as
//! `overlay/selector.rs`); the pure helpers below are unit-tested.

use crate::quick_commands::{sanitize_command, QuickCommandsFile, Row};
use mux_lua::MuxPane;
use termwiz::input::{InputEvent, KeyCode, KeyEvent, Modifiers, MouseButtons, MouseEvent};
use termwiz::lineedit::{BasicHistory, LineEditor, LineEditorHost};
use termwiz::surface::{Change, Position};
use termwiz::terminal::Terminal;
use mux::termwiztermtab::TermWizTerminal;
use termwiz_funcs::truncate_right;
use crate::scripting::guiwin::GuiWin;

/// Right-hand preview of a command row, truncated to the available columns.
/// Keeps at least the ellipsis itself so callers never render an empty hint.
fn truncate_command_line(command: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let width = termwiz::cell::unicode_column_width(command, None);
    if width <= max_cols {
        return command.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(command, true) {
        let w = termwiz::cell::unicode_column_width(grapheme, None);
        if used + w > max_cols.saturating_sub(1) {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push('…');
    out
}

/// Sanitize a command at the insert boundary. Kept as a function so the
/// overlay's one insert path has a single, testable choke point.
fn command_to_insert(command: &str) -> String {
    sanitize_command(command)
}

struct PromptHost {
    history: BasicHistory,
}

impl LineEditorHost for PromptHost {
    fn history(&mut self) -> &mut dyn termwiz::lineedit::History {
        &mut self.history
    }
}

struct QuickCommandsState {
    file: QuickCommandsFile,
    filter_term: String,
    filtering: bool,
    rows: Vec<Row>,
    active_idx: usize,
    top_row: usize,
    max_items: usize,
}

const TITLE: &str = "快捷命令  Enter/单击=填入  n=新建  e=编辑  d=删除  /=过滤  Esc=关闭";
const DEFAULT_GROUP: &str = "日常";

impl QuickCommandsState {
    fn new() -> Self {
        let file = crate::quick_commands::load_from(
            &crate::quick_commands::quick_commands_path(),
        );
        let rows = file.rows("");
        Self {
            file,
            filter_term: String::new(),
            filtering: false,
            rows,
            active_idx: 0,
            top_row: 0,
            max_items: 1,
        }
    }

    fn reload(&mut self) {
        self.file = crate::quick_commands::load_from(
            &crate::quick_commands::quick_commands_path(),
        );
        self.update_rows();
    }

    fn update_rows(&mut self) {
        self.rows = self.file.rows(&self.filter_term);
        self.active_idx = 0;
        self.top_row = 0;
    }

    fn move_up(&mut self) {
        self.active_idx = self.active_idx.saturating_sub(1);
        if self.active_idx < self.top_row {
            self.top_row = self.active_idx;
        }
    }

    fn move_down(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.active_idx = (self.active_idx + 1).min(self.rows.len() - 1);
        if self.active_idx > self.top_row + self.max_items {
            self.top_row = self.active_idx.saturating_sub(self.max_items);
        }
    }

    fn selected_command(&self) -> Option<(usize, usize, String, String)> {
        match self.rows.get(self.active_idx) {
            Some(Row::Command {
                group_idx,
                cmd_idx,
                label,
                command,
            }) => Some((*group_idx, *cmd_idx, label.clone(), command.clone())),
            _ => None,
        }
    }

    fn command_at(&self, y: usize) -> Option<(usize, usize, String, String)> {
        let idx = self.top_row + y.saturating_sub(1);
        match self.rows.get(idx) {
            Some(Row::Command {
                group_idx,
                cmd_idx,
                label,
                command,
            }) => Some((*group_idx, *cmd_idx, label.clone(), command.clone())),
            _ => None,
        }
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> termwiz::Result<()> {
        use termwiz::cell::{AttributeChange, CellAttributes};
        use termwiz::color::ColorAttribute;

        let size = term.get_screen_size()?;
        let max_width = size.cols.saturating_sub(2);
        self.max_items = size.rows.saturating_sub(4);

        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(0),
            },
            Change::Text(format!(
                "{}\r\n",
                termwiz_funcs::truncate_right(TITLE, max_width)
            )),
        ];

        // Dim group headings and command previews by blending toward the
        // background, the same way overlay dividers stay quiet.
        let config = config::configuration();
        let palette = &config.resolved_palette;
        let bg: termwiz::color::SrgbaTuple =
            *palette.background.unwrap_or(config::SrgbaTuple(0.0, 0.0, 0.0, 1.0).into());
        let fg: termwiz::color::SrgbaTuple =
            *palette.foreground.unwrap_or(config::SrgbaTuple(1.0, 1.0, 1.0, 1.0).into());
        let dim_color = crate::overlay::divider_color(bg, fg);
        let dim = ColorAttribute::from(config::ColorSpec::Color(dim_color.into()));

        for (idx, row) in self
            .rows
            .iter()
            .enumerate()
            .skip(self.top_row)
            .take(self.max_items)
        {
            let mut attr = CellAttributes::blank();
            changes.push(Change::Text("\r\n".to_string()));
            if idx == self.active_idx {
                attr.set_reverse(true);
                changes.push(AttributeChange::Reverse(true).into());
            }
            match row {
                Row::Group { name } => {
                    attr.set_foreground(dim.clone());
                    changes.push(AttributeChange::Foreground(dim.clone()).into());
                    changes.push(Change::Text(format!("── {name} ")));
                }
                Row::Command { label, command, .. } => {
                    changes.push(Change::Text(format!("  {label}  ")));
                    attr.set_foreground(dim.clone());
                    changes.push(AttributeChange::Foreground(dim.clone()).into());
                    let used = termwiz::cell::unicode_column_width(label, None) + 4;
                    let remaining = max_width.saturating_sub(used);
                    changes.push(Change::Text(truncate_command_line(
                        command, remaining,
                    )));
                }
            }
            changes.push(AttributeChange::Reverse(false).into());
            changes.push(AttributeChange::Foreground(ColorAttribute::Default).into());
        }

        if self.filtering || !self.filter_term.is_empty() {
            changes.push(Change::CursorPosition {
                x: Position::Absolute(0),
                y: Position::Absolute(0),
            });
            changes.push(Change::ClearToEndOfLine(ColorAttribute::Default));
            changes.push(Change::Text(truncate_right(
                &format!("过滤: {}", self.filter_term),
                max_width,
            )));
        }

        term.render(&changes)
    }
}

/// One LineEditor prompt; Ok(None) means the user aborted (Esc on empty).
fn prompt_line(
    term: &mut TermWizTerminal,
    prompt: &str,
    initial: Option<&str>,
) -> anyhow::Result<Option<String>> {
    term.render(&[Change::ClearScreen(termwiz::color::ColorAttribute::Default),
        Change::CursorPosition {
            x: Position::Absolute(0),
            y: Position::Absolute(0),
        },
        Change::Text(format!("{prompt}\r\n"))])?;
    let mut host = PromptHost {
        history: BasicHistory::default(),
    };
    let mut editor = LineEditor::new(term);
    editor.set_prompt(prompt);
    Ok(editor.read_line_with_optional_initial_value(&mut host, initial)?)
}

fn save(state: &QuickCommandsState) -> anyhow::Result<()> {
    crate::quick_commands::save_to(
        &crate::quick_commands::quick_commands_path(),
        &state.file,
    )
}

fn insert_into_pane(pane: &MuxPane, text: &str) {
    if let Some(p) = mux::Mux::get().get_pane(pane.0) {
        if let Err(err) = p.writer().write_all(text.as_bytes()) {
            log::error!("quick commands: failed to insert text: {:#}", err);
        }
    }
}

fn insert_selected(pane: &MuxPane, command: &str) {
    insert_into_pane(pane, &command_to_insert(command));
}

pub fn run_quick_commands_overlay(
    mut term: TermWizTerminal,
    _window: GuiWin,
    pane: MuxPane,
) -> anyhow::Result<()> {
    term.set_raw_mode()?;
    let mut state = QuickCommandsState::new();
    state.render(&mut term)?;

    while let Ok(Some(event)) = term.poll_input(None) {
        match event {
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('G' | 'C'),
                modifiers: Modifiers::CTRL,
            })
            | InputEvent::Key(KeyEvent {
                key: KeyCode::Escape,
                ..
            }) => break,
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('j' | 'n'),
                modifiers: Modifiers::NONE,
            }) if !state.filtering => state.move_down(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('k' | 'p'),
                modifiers: Modifiers::NONE,
            }) if !state.filtering => state.move_up(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::UpArrow,
                ..
            }) => state.move_up(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::DownArrow,
                ..
            }) => state.move_down(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('P' | 'K'),
                modifiers: Modifiers::CTRL,
            }) => state.move_up(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('N' | 'J'),
                modifiers: Modifiers::CTRL,
            }) => state.move_down(),
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('/'),
                ..
            }) if !state.filtering => {
                state.filtering = true;
                state.filter_term.clear();
                state.update_rows();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Backspace,
                ..
            }) => {
                if state.filtering {
                    if state.filter_term.pop().is_some() {
                        state.update_rows();
                    } else {
                        state.filtering = false;
                        state.update_rows();
                    }
                }
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char(c),
                ..
            }) if state.filtering => {
                state.filter_term.push(c);
                state.update_rows();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Enter,
                ..
            }) if !state.filtering => {
                if let Some((_, _, _, command)) = state.selected_command() {
                    insert_selected(&pane, &command);
                    break;
                }
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('n'),
                modifiers: Modifiers::NONE,
            }) if !state.filtering => {
                let group = prompt_line(&mut term, "分组 (空=日常):", None)?;
                let Some(group) = group else { continue };
                let Some(label) = prompt_line(&mut term, "名称:", None)? else {
                    continue;
                };
                if label.trim().is_empty() {
                    continue;
                }
                let Some(command) = prompt_line(&mut term, "命令:", None)? else {
                    continue;
                };
                if command.trim().is_empty() {
                    continue;
                }
                state.file.insert_command(group.trim(), label.trim(), &command);
                save(&state)?;
                state.reload();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('e'),
                modifiers: Modifiers::NONE,
            }) if !state.filtering => {
                let Some((group_idx, cmd_idx, label, command)) = state.selected_command()
                else {
                    continue;
                };
                let Some(new_label) =
                    prompt_line(&mut term, "名称:", Some(&label))?
                else {
                    continue;
                };
                let new_label = if new_label.trim().is_empty() {
                    label
                } else {
                    new_label.trim().to_string()
                };
                let Some(new_command) =
                    prompt_line(&mut term, "命令:", Some(&command))?
                else {
                    continue;
                };
                let new_command = if new_command.trim().is_empty() {
                    command
                } else {
                    new_command.trim().to_string()
                };
                let group_name = state
                    .file
                    .groups
                    .get(group_idx)
                    .map(|g| g.name.clone())
                    .unwrap_or_else(|| DEFAULT_GROUP.to_string());
                // Move-or-edit: edit in place is enough; a group change is
                // expressed as delete + new. Keep `e` simple per spec.
                let _ = group_name;
                state.file.update_command(group_idx, cmd_idx, &new_label, &new_command);
                save(&state)?;
                state.reload();
            }
            InputEvent::Key(KeyEvent {
                key: KeyCode::Char('d'),
                modifiers: Modifiers::NONE,
            }) if !state.filtering => {
                let Some((group_idx, cmd_idx, label, _)) = state.selected_command()
                else {
                    continue;
                };
                let Some(answer) = prompt_line(
                    &mut term,
                    &format!("删除 \"{label}\"? (y/N):"),
                    None,
                )?
                else {
                    continue;
                };
                if answer.trim().eq_ignore_ascii_case("y") {
                    state.file.delete_command(group_idx, cmd_idx);
                    save(&state)?;
                    state.reload();
                }
            }
            InputEvent::Mouse(MouseEvent {
                y,
                mouse_buttons,
                ..
            }) if mouse_buttons.contains(MouseButtons::VERT_WHEEL) => {
                if mouse_buttons.contains(MouseButtons::WHEEL_POSITIVE) {
                    state.top_row = state.top_row.saturating_sub(1);
                } else {
                    state.top_row = (state.top_row + 1)
                        .min(state.rows.len().saturating_sub(state.max_items).max(0));
                }
                let _ = y;
            }
            InputEvent::Mouse(MouseEvent {
                y,
                mouse_buttons,
                ..
            }) => {
                if mouse_buttons == MouseButtons::LEFT {
                    if let Some((_, _, _, command)) = state.command_at(y as usize) {
                        insert_selected(&pane, &command);
                        break;
                    }
                } else if mouse_buttons != MouseButtons::NONE {
                    break;
                }
            }
            _ => {}
        }
        state.render(&mut term)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_to_insert_is_sanitized() {
        // The single insert path must strip CR/LF, or a pasted multi-line
        // command would execute itself on insert.
        assert_eq!(command_to_insert("echo a\nrm -rf /"), "echo arm -rf /");
        assert_eq!(command_to_insert("git status\r\n"), "git status");
        assert_eq!(command_to_insert("./start.sh"), "./start.sh");
    }

    #[test]
    fn display_truncates_long_command() {
        assert_eq!(truncate_command_line("short", 10), "short");
        let long = truncate_command_line("a".repeat(100).as_str(), 20);
        assert_eq!(long.chars().count(), 20);
        assert!(long.ends_with('…'));
        // CJK: 10 chars are 20 columns wide; truncating to 12 columns must
        // not panic and must not exceed the column budget.
        let cjk = truncate_command_line("快捷命令测试一大段文字", 12);
        assert!(termwiz::cell::unicode_column_width(&cjk, None) <= 12);
        assert!(truncate_command_line("abc", 0).is_empty());
    }
}
