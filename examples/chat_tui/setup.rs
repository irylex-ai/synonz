//! The startup wizard: endpoint configuration, then model and thinking.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use synonz_openai::ReasoningEffort;

use crate::app::TextInput;

/// The wizard steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Endpoint and credentials.
    Api,
    /// Model and thinking level.
    Model,
    /// The wizard is done (the chat takes over).
    Chat,
}

/// What a key press asks the wizard to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Something changed; redraw.
    Redraw,
    /// The user asked to quit.
    Quit,
    /// Configuration is complete; start the session.
    Launch,
}

/// The thinking choices offered by the wizard (label, wire option).
///
/// `Default` omits the parameter (the provider decides); `Off` sends the
/// provider's explicit "no reasoning" value.
pub const EFFORT_CHOICES: [(&str, Option<ReasoningEffort>); 8] = [
    ("Default", None),
    ("Off", Some(ReasoningEffort::Off)),
    ("Minimal", Some(ReasoningEffort::Minimal)),
    ("Low", Some(ReasoningEffort::Low)),
    ("Medium", Some(ReasoningEffort::Medium)),
    ("High", Some(ReasoningEffort::High)),
    ("ExtraHigh", Some(ReasoningEffort::ExtraHigh)),
    ("Max", Some(ReasoningEffort::Max)),
];

/// The wizard state.
pub struct Setup {
    /// The current step.
    pub step: Step,
    /// The API base URL.
    pub base_url: TextInput,
    /// The API key (rendered masked).
    pub api_key: TextInput,
    /// The model name.
    pub model: TextInput,
    /// The selected thinking choice.
    pub effort_index: usize,
    /// The focused field within the current step.
    pub field: usize,
    /// The last validation error.
    pub error: Option<String>,
}

impl Setup {
    /// Creates the wizard, pre-filling from the environment.
    pub fn from_env() -> Self {
        Self {
            step: Step::Api,
            base_url: TextInput::new(
                std::env::var("SYNONZ_OPENAI_BASE_URL")
                    .unwrap_or_else(|_| synonz_openai::DEFAULT_BASE_URL.to_string()),
            ),
            api_key: TextInput::new(std::env::var("SYNONZ_OPENAI_API_KEY").unwrap_or_default()),
            model: TextInput::new(
                std::env::var("SYNONZ_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string()),
            ),
            effort_index: 0,
            field: 0,
            error: None,
        }
    }

    /// The selected thinking option.
    pub fn effort(&self) -> Option<ReasoningEffort> {
        EFFORT_CHOICES[self.effort_index].1
    }

    /// The selected thinking label.
    pub fn effort_label(&self) -> &'static str {
        EFFORT_CHOICES[self.effort_index].0
    }

    /// Handles one key press.
    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Outcome::Quit;
        }
        match key.code {
            KeyCode::Esc => match self.step {
                Step::Api => Outcome::Quit,
                Step::Model => {
                    self.step = Step::Api;
                    self.field = 0;
                    self.error = None;
                    Outcome::Redraw
                }
                Step::Chat => Outcome::Redraw,
            },
            KeyCode::Tab => {
                self.field = (self.field + 1) % self.field_count();
                Outcome::Redraw
            }
            KeyCode::BackTab => {
                self.field = (self.field + self.field_count() - 1) % self.field_count();
                Outcome::Redraw
            }
            KeyCode::Enter => self.confirm(),
            KeyCode::Left if self.on_effort_field() => {
                self.effort_index = self.effort_index.saturating_sub(1);
                Outcome::Redraw
            }
            KeyCode::Right if self.on_effort_field() => {
                self.effort_index = (self.effort_index + 1).min(EFFORT_CHOICES.len() - 1);
                Outcome::Redraw
            }
            KeyCode::Char(' ') if self.on_effort_field() => {
                self.effort_index = (self.effort_index + 1) % EFFORT_CHOICES.len();
                Outcome::Redraw
            }
            code => {
                if let Some(field) = self.text_field() {
                    match code {
                        KeyCode::Char(ch) => field.insert(ch),
                        KeyCode::Backspace => field.backspace(),
                        KeyCode::Delete => field.delete(),
                        KeyCode::Left => field.move_left(),
                        KeyCode::Right => field.move_right(),
                        KeyCode::Home => field.home(),
                        KeyCode::End => field.end(),
                        _ => {}
                    }
                }
                Outcome::Redraw
            }
        }
    }

    fn confirm(&mut self) -> Outcome {
        match self.step {
            Step::Api => match self.validate_api() {
                Ok(()) => {
                    self.step = Step::Model;
                    self.field = 0;
                    self.error = None;
                    Outcome::Redraw
                }
                Err(message) => {
                    self.error = Some(message);
                    Outcome::Redraw
                }
            },
            Step::Model => match self.validate_model() {
                Ok(()) => {
                    self.step = Step::Chat;
                    self.error = None;
                    Outcome::Launch
                }
                Err(message) => {
                    self.error = Some(message);
                    Outcome::Redraw
                }
            },
            Step::Chat => Outcome::Redraw,
        }
    }

    fn field_count(&self) -> usize {
        match self.step {
            Step::Api | Step::Model => 2,
            Step::Chat => 1,
        }
    }

    fn on_effort_field(&self) -> bool {
        self.step == Step::Model && self.field == 1
    }

    fn text_field(&mut self) -> Option<&mut TextInput> {
        match (self.step, self.field) {
            (Step::Api, 0) => Some(&mut self.base_url),
            (Step::Api, 1) => Some(&mut self.api_key),
            (Step::Model, 0) => Some(&mut self.model),
            _ => None,
        }
    }

    fn validate_api(&self) -> Result<(), String> {
        let base_url = self.base_url.text().trim();
        if base_url.is_empty() {
            return Err("Base URL must not be empty".to_string());
        }
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err("Base URL must start with http:// or https://".to_string());
        }
        if self.api_key.text().trim().is_empty() {
            return Err("API key must not be empty".to_string());
        }
        Ok(())
    }

    fn validate_model(&self) -> Result<(), String> {
        if self.model.text().trim().is_empty() {
            return Err("Model must not be empty".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Setup {
        Setup {
            step: Step::Api,
            base_url: TextInput::new("https://api.openai.com/v1"),
            api_key: TextInput::new("sk-test"),
            model: TextInput::new("gpt-4o-mini"),
            effort_index: 0,
            field: 0,
            error: None,
        }
    }

    #[test]
    fn api_validation_rejects_bad_input() {
        let mut wizard = setup();
        wizard.base_url.set("ftp://example.com");
        assert!(wizard.validate_api().is_err());
        wizard.base_url.set("https://example.com");
        wizard.api_key.set("");
        assert!(wizard.validate_api().is_err());
        wizard.api_key.set("k");
        assert!(wizard.validate_api().is_ok());
    }

    #[test]
    fn model_validation_and_effort_mapping() {
        let mut wizard = setup();
        wizard.model.set("");
        assert!(wizard.validate_model().is_err());
        wizard.model.set("gpt-5");
        assert!(wizard.validate_model().is_ok());
        assert_eq!(wizard.effort(), None);
        wizard.effort_index = 1;
        assert_eq!(wizard.effort(), Some(ReasoningEffort::Off));
        wizard.effort_index = EFFORT_CHOICES.len() - 1;
        assert_eq!(wizard.effort(), Some(ReasoningEffort::Max));
    }
}
