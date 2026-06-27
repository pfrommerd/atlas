use reedline::{Prompt, PromptEditMode, PromptViMode,
            PromptHistorySearch, PromptHistorySearchStatus};
use std::borrow::Cow;

pub static DEFAULT_PROMPT_INDICATOR: &str = ">>> ";
pub static DEFAULT_VI_INSERT_PROMPT_INDICATOR: &str = ": ";
pub static DEFAULT_VI_NORMAL_PROMPT_INDICATOR: &str = ">>> ";
pub static DEFAULT_MULTILINE_INDICATOR: &str = "::: ";

#[derive(Default)]
pub struct AtlasPrompt {}

impl Prompt for AtlasPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, prompt_mode: PromptEditMode) -> Cow<'_, str> {
        // Return plain text so reedline applies its default (cyan) indicator
        // color, matching the core-repl example.
        match prompt_mode {
            PromptEditMode::Default | PromptEditMode::Emacs => {
                Cow::Borrowed(DEFAULT_PROMPT_INDICATOR)
            }
            PromptEditMode::Vi(vi_mode) => match vi_mode {
                PromptViMode::Normal => Cow::Borrowed(DEFAULT_VI_NORMAL_PROMPT_INDICATOR),
                PromptViMode::Insert => Cow::Borrowed(DEFAULT_VI_INSERT_PROMPT_INDICATOR),
            },
            PromptEditMode::Custom(str) => Cow::Owned(format!("({str})")),
        }
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed(DEFAULT_MULTILINE_INDICATOR)
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch
    ) -> Cow<'_, str> {
        let prefix = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing "
        };
        Cow::Owned(format!("({}reverse search: {})", prefix, history_search.term))
    }
}