//! The single-line input box: a tui-textarea wrapper with Up/Down history
//! recall, persisted across sessions.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Style;
use std::path::PathBuf;
use tui_textarea::TextArea;

pub struct InputBox {
    textarea: TextArea<'static>,
    history: Vec<String>,
    /// Index into `history` while browsing, `None` while editing a fresh line.
    browse: Option<usize>,
    /// The in-progress line stashed while browsing history.
    stash: String,
    path: Option<PathBuf>,
}

impl InputBox {
    pub fn new() -> Self {
        let path = directories::ProjectDirs::from("org", "atlas", "atlas")
            .map(|dirs| dirs.cache_dir().join("history.txt"));
        let history = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|src| src.lines().map(str::to_string).collect())
            .unwrap_or_default();
        InputBox {
            textarea: fresh_textarea(String::new()),
            history,
            browse: None,
            stash: String::new(),
            path,
        }
    }

    pub fn widget(&self) -> &TextArea<'static> {
        &self.textarea
    }

    pub fn is_empty(&self) -> bool {
        self.line().trim().is_empty()
    }

    pub fn line(&self) -> &str {
        self.textarea
            .lines()
            .first()
            .map(String::as_str)
            .unwrap_or("")
    }

    fn set_line(&mut self, line: String) {
        self.textarea = fresh_textarea(line);
    }

    /// Feed one key event; returns the submitted line on Enter.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        match key.code {
            KeyCode::Enter => {
                let line = self.line().to_string();
                self.set_line(String::new());
                self.browse = None;
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                }
                Some(line)
            }
            KeyCode::Up => {
                let next = match self.browse {
                    None if self.history.is_empty() => return None,
                    None => {
                        self.stash = self.line().to_string();
                        self.history.len() - 1
                    }
                    Some(0) => return None,
                    Some(i) => i - 1,
                };
                self.browse = Some(next);
                self.set_line(self.history[next].clone());
                None
            }
            KeyCode::Down => {
                match self.browse {
                    None => {}
                    Some(i) if i + 1 < self.history.len() => {
                        self.browse = Some(i + 1);
                        self.set_line(self.history[i + 1].clone());
                    }
                    Some(_) => {
                        self.browse = None;
                        let stash = std::mem::take(&mut self.stash);
                        self.set_line(stash);
                    }
                }
                None
            }
            _ => {
                self.browse = None;
                self.textarea.input(key);
                None
            }
        }
    }

    /// Persist the history (best-effort; called on exit).
    pub fn save_history(&self) {
        let Some(path) = &self.path else { return };
        const KEEP: usize = 1000;
        let start = self.history.len().saturating_sub(KEEP);
        let contents = self.history[start..].join("\n");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, contents);
    }
}

fn fresh_textarea(line: String) -> TextArea<'static> {
    let mut textarea = TextArea::new(vec![line]);
    // The default underlines the cursor's line; a one-line input doesn't need it.
    textarea.set_cursor_line_style(Style::default());
    textarea.move_cursor(tui_textarea::CursorMove::End);
    textarea
}
