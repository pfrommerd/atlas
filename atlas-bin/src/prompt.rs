use reedline::{Prompt, PromptEditMode, PromptViMode, 
            PromptHistorySearch, PromptHistorySearchStatus,
            StyledText};
use nu_ansi_term::{Color, Style};
use std::borrow::Cow;
use super::env_info;

pub static DEFAULT_PROMPT_INDICATOR: &str = "> ";
pub static DEFAULT_VI_INSERT_PROMPT_INDICATOR: &str = ": ";
pub static DEFAULT_VI_NORMAL_PROMPT_INDICATOR: &str = "> ";
pub static DEFAULT_MULTILINE_INDICATOR: &str = "::: ";

#[derive(Default)]
pub struct AtlasPrompt {}

impl Prompt for AtlasPrompt {
    fn render_prompt_left(&self) -> Cow<str> {
        let mut styled = StyledText::new();
        // Clear out the style that reedline puts in front
        styled.push((Color::Red.bold(), String::from("")));

        styled.push((Color::Blue.normal(), env_info::user()));
        styled.push((Style::new(), String::from("@")));
        styled.push((Style::new(), env_info::host()));
        styled.push((Style::new(), String::from(" ")));
        styled.push((Color::Blue.normal(), String::from("atlas")));
        Cow::Owned(styled.render_simple())
    }

    fn render_prompt_right(&self) -> Cow<str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, prompt_mode: PromptEditMode) -> Cow<str> {
        let prompt = match prompt_mode {
            PromptEditMode::Default | PromptEditMode::Emacs => DEFAULT_PROMPT_INDICATOR.into(),
            PromptEditMode::Vi(vi_mode) => match vi_mode {
                PromptViMode::Normal => DEFAULT_VI_NORMAL_PROMPT_INDICATOR.into(),
                PromptViMode::Insert => DEFAULT_VI_INSERT_PROMPT_INDICATOR.into(),
            },
            PromptEditMode::Custom(str) => format!("({})", str).into(),
        };
        let mut styled = StyledText::new();
        styled.push((Color::Default.normal(), prompt));
        Cow::Owned(styled.render_simple())
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<str> {
        Cow::Borrowed(DEFAULT_MULTILINE_INDICATOR)
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch
    ) -> Cow<str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing "
        };
        Cow::Owned(format!("({}reverse search: {})", prefix, history_search.term))
    }
}