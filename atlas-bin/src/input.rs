//! The single-line input box with Up/Down history recall, persisted across
//! sessions.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::path::PathBuf;

pub struct InputBox {
    line: String,
    cursor: usize,
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
            line: String::new(),
            cursor: 0,
            history,
            browse: None,
            stash: String::new(),
            path,
        }
    }

    pub fn widget(&self) -> Paragraph<'_> {
        let before = &self.line[..self.cursor];
        let (cursor, after) = self.line[self.cursor..]
            .chars()
            .next()
            .map(|character| {
                let end = self.cursor + character.len_utf8();
                (&self.line[self.cursor..end], &self.line[end..])
            })
            .unwrap_or((" ", ""));
        Paragraph::new(Line::from(vec![
            Span::raw(before),
            Span::styled(cursor, Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(after),
        ]))
    }

    pub fn is_empty(&self) -> bool {
        self.line().trim().is_empty()
    }

    pub fn line(&self) -> &str {
        &self.line
    }

    pub fn replace_line(&mut self, line: String) {
        self.cursor = line.len();
        self.line = line;
    }

    /// Feed one key event; returns the submitted line on Enter.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        match key.code {
            KeyCode::Enter => {
                let line = self.line().to_string();
                self.replace_line(String::new());
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
                self.replace_line(self.history[next].clone());
                None
            }
            KeyCode::Down => {
                match self.browse {
                    None => {}
                    Some(i) if i + 1 < self.history.len() => {
                        self.browse = Some(i + 1);
                        self.replace_line(self.history[i + 1].clone());
                    }
                    Some(_) => {
                        self.browse = None;
                        let stash = std::mem::take(&mut self.stash);
                        self.replace_line(stash);
                    }
                }
                None
            }
            KeyCode::Left => {
                self.browse = None;
                if self.cursor > 0 {
                    self.cursor = self.line[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map(|(index, _)| index)
                        .unwrap_or(0);
                }
                None
            }
            KeyCode::Right => {
                self.browse = None;
                if let Some(character) = self.line[self.cursor..].chars().next() {
                    self.cursor += character.len_utf8();
                }
                None
            }
            KeyCode::Home => {
                self.browse = None;
                self.cursor = 0;
                None
            }
            KeyCode::End => {
                self.browse = None;
                self.cursor = self.line.len();
                None
            }
            KeyCode::Backspace => {
                self.browse = None;
                if self.cursor > 0 {
                    let previous = self.line[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map(|(index, _)| index)
                        .unwrap_or(0);
                    self.line.drain(previous..self.cursor);
                    self.cursor = previous;
                }
                None
            }
            KeyCode::Delete => {
                self.browse = None;
                if let Some(character) = self.line[self.cursor..].chars().next() {
                    self.line
                        .drain(self.cursor..self.cursor + character.len_utf8());
                }
                None
            }
            KeyCode::Char(character) => {
                self.browse = None;
                self.line.insert(self.cursor, character);
                self.cursor += character.len_utf8();
                None
            }
            _ => {
                self.browse = None;
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
