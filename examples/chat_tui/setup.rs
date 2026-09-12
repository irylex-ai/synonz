//! The startup wizard: endpoint configuration, then model and reasoning.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use synonz_openai::ReasoningEffort;

use crate::app::TextInput;

/// The wizard steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Endpoint and credentials.
    Api,
    /// Model, reasoning switch, and effort level.
    Model,
    /// The wizard is done (the chat takes over).
    Chat,
}

/// What a key press asks the wizard (or the event loop) to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Something changed; redraw.
    Redraw,
    /// The user asked to quit.
    Quit,
    /// Fetch the model list from the endpoint (async).
    LoadModels,
    /// Configuration is complete; start the session.
    Launch,
}

/// The effort choices offered when reasoning is enabled.
pub const EFFORT_CHOICES: [(&str, Option<ReasoningEffort>); 7] = [
    ("Default", None),
    ("Minimal", Some(ReasoningEffort::Minimal)),
    ("Low", Some(ReasoningEffort::Low)),
    ("Medium", Some(ReasoningEffort::Medium)),
    ("High", Some(ReasoningEffort::High)),
    ("ExtraHigh", Some(ReasoningEffort::ExtraHigh)),
    ("Max", Some(ReasoningEffort::Max)),
];

/// Which list a popup shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    /// The endpoint's model ids (filterable).
    Models,
    /// The effort ladder.
    Levels,
}

/// An open selection popup.
#[derive(Debug, Clone)]
pub struct Popup {
    /// What the popup lists.
    pub kind: PopupKind,
    /// The filter query (models only).
    pub query: String,
    /// The highlighted index into the filtered items.
    pub selected: usize,
}

impl Popup {
    fn new(kind: PopupKind) -> Self {
        Self {
            kind,
            query: String::new(),
            selected: 0,
        }
    }
}

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
    /// Whether reasoning is requested (off sends the explicit "none").
    pub reasoning_enabled: bool,
    /// The selected effort choice (used when reasoning is enabled).
    pub effort_index: usize,
    /// The focused field within the current step.
    pub field: usize,
    /// The last validation error.
    pub error: Option<String>,
    /// The fetched model ids (`None` = not fetched yet).
    pub models: Option<Vec<String>>,
    /// Whether a model fetch is in flight.
    pub models_loading: bool,
    /// The model fetch error, if any.
    pub models_error: Option<String>,
    /// The open selection popup, if any.
    pub popup: Option<Popup>,
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
            reasoning_enabled: true,
            effort_index: 0,
            field: 0,
            error: None,
            models: None,
            models_loading: false,
            models_error: None,
            popup: None,
        }
    }

    /// The requested reasoning option.
    ///
    /// Disabled sends the provider's explicit `none`; enabled sends the
    /// selected level (`Default` omits the parameter).
    pub fn effort(&self) -> Option<ReasoningEffort> {
        if self.reasoning_enabled {
            EFFORT_CHOICES[self.effort_index].1
        } else {
            Some(ReasoningEffort::Off)
        }
    }

    /// The reasoning summary shown in the wizard.
    pub fn reasoning_label(&self) -> &'static str {
        if self.reasoning_enabled {
            EFFORT_CHOICES[self.effort_index].0
        } else {
            "Off"
        }
    }

    /// Records a successful model fetch; opens the picker when non-empty.
    pub fn set_models(&mut self, models: Vec<String>) {
        self.models_loading = false;
        self.models_error = None;
        if !models.is_empty() && self.popup.is_none() {
            self.popup = Some(Popup::new(PopupKind::Models));
        }
        self.models = Some(models);
    }

    /// Records a failed model fetch.
    pub fn set_models_error(&mut self, message: String) {
        self.models_loading = false;
        self.models_error = Some(message);
    }

    /// The items the open popup shows (filtered for models).
    pub fn popup_items(&self) -> Vec<String> {
        let Some(popup) = &self.popup else {
            return Vec::new();
        };
        match popup.kind {
            PopupKind::Levels => EFFORT_CHOICES
                .iter()
                .map(|(label, _)| (*label).to_string())
                .collect(),
            PopupKind::Models => {
                let query = popup.query.to_ascii_lowercase();
                self.models
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .filter(|model| model.to_ascii_lowercase().contains(&query))
                    .cloned()
                    .collect()
            }
        }
    }

    /// Handles one key press.
    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Outcome::Quit;
        }
        if self.popup.is_some() {
            return self.handle_popup(key);
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
            KeyCode::Left => self.adjust(-1),
            KeyCode::Right => self.adjust(1),
            KeyCode::Char(' ') if self.step == Step::Model && self.field == 1 => {
                self.reasoning_enabled = !self.reasoning_enabled;
                Outcome::Redraw
            }
            code => {
                if let Some(field) = self.text_field() {
                    match code {
                        KeyCode::Char(ch) => field.insert(ch),
                        KeyCode::Backspace => field.backspace(),
                        KeyCode::Delete => field.delete(),
                        KeyCode::Home => field.home(),
                        KeyCode::End => field.end(),
                        _ => {}
                    }
                }
                Outcome::Redraw
            }
        }
    }

    fn handle_popup(&mut self, key: KeyEvent) -> Outcome {
        let Some(kind) = self.popup.as_ref().map(|popup| popup.kind) else {
            return Outcome::Redraw;
        };
        match key.code {
            KeyCode::Esc => self.popup = None,
            KeyCode::Up => {
                if let Some(popup) = &mut self.popup {
                    popup.selected = popup.selected.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                let last = self.popup_items().len().saturating_sub(1);
                if let Some(popup) = &mut self.popup {
                    popup.selected = (popup.selected + 1).min(last);
                }
            }
            KeyCode::Enter => {
                let items = self.popup_items();
                let selected = self.popup.as_ref().map(|popup| popup.selected).unwrap_or(0);
                if let Some(item) = items.get(selected) {
                    match kind {
                        PopupKind::Models => self.model.set(item.clone()),
                        PopupKind::Levels => {
                            if let Some(index) =
                                EFFORT_CHOICES.iter().position(|(label, _)| label == item)
                            {
                                self.effort_index = index;
                            }
                        }
                    }
                }
                self.popup = None;
            }
            KeyCode::Backspace if kind == PopupKind::Models => {
                if let Some(popup) = &mut self.popup {
                    popup.query.pop();
                    popup.selected = 0;
                }
            }
            KeyCode::Char(ch) if kind == PopupKind::Models => {
                if let Some(popup) = &mut self.popup {
                    popup.query.push(ch);
                    popup.selected = 0;
                }
            }
            _ => {}
        }
        Outcome::Redraw
    }

    fn adjust(&mut self, delta: i32) -> Outcome {
        match (self.step, self.field) {
            (Step::Model, 1) => self.reasoning_enabled = !self.reasoning_enabled,
            (Step::Model, 2) if self.reasoning_enabled => {
                let last = EFFORT_CHOICES.len() - 1;
                let next = self.effort_index as i32 + delta;
                self.effort_index = next.clamp(0, last as i32) as usize;
            }
            _ => {
                if let Some(field) = self.text_field() {
                    if delta < 0 {
                        field.move_left();
                    } else {
                        field.move_right();
                    }
                }
            }
        }
        Outcome::Redraw
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
            Step::Model => match self.field {
                0 => {
                    if self.models.is_some() {
                        self.popup = Some(Popup::new(PopupKind::Models));
                        Outcome::Redraw
                    } else if self.models_loading {
                        Outcome::Redraw
                    } else {
                        self.models_loading = true;
                        self.models_error = None;
                        Outcome::LoadModels
                    }
                }
                1 => {
                    self.reasoning_enabled = !self.reasoning_enabled;
                    Outcome::Redraw
                }
                2 if self.reasoning_enabled => {
                    self.popup = Some(Popup::new(PopupKind::Levels));
                    Outcome::Redraw
                }
                3 => match self.validate_model() {
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
                _ => Outcome::Redraw,
            },
            Step::Chat => Outcome::Redraw,
        }
    }

    fn field_count(&self) -> usize {
        match self.step {
            Step::Api => 2,
            Step::Model => 4,
            Step::Chat => 1,
        }
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
            reasoning_enabled: true,
            effort_index: 0,
            field: 0,
            error: None,
            models: None,
            models_loading: false,
            models_error: None,
            popup: None,
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
    fn reasoning_mapping_covers_toggle_and_ladder() {
        let mut wizard = setup();
        assert_eq!(wizard.effort(), None, "Default omits the parameter");
        wizard.effort_index = EFFORT_CHOICES.len() - 1;
        assert_eq!(wizard.effort(), Some(ReasoningEffort::Max));
        wizard.reasoning_enabled = false;
        assert_eq!(wizard.effort(), Some(ReasoningEffort::Off));
        assert_eq!(wizard.reasoning_label(), "Off");
    }

    #[test]
    fn model_popup_filters_and_selects() {
        let mut wizard = setup();
        wizard.step = Step::Model;
        wizard.set_models(vec![
            "gpt-4o-mini".to_string(),
            "gpt-5".to_string(),
            "deepseek-chat".to_string(),
        ]);
        assert!(wizard.popup.is_some(), "a fresh list opens the picker");
        assert_eq!(wizard.popup_items().len(), 3);

        wizard.popup.as_mut().expect("popup").query = "gpt".to_string();
        assert_eq!(wizard.popup_items(), vec!["gpt-4o-mini", "gpt-5"]);

        wizard.popup.as_mut().expect("popup").selected = 1;
        assert_eq!(
            wizard.handle_popup(KeyEvent::from(KeyCode::Enter)),
            Outcome::Redraw
        );
        assert_eq!(wizard.model.text(), "gpt-5");
        assert!(wizard.popup.is_none());
    }

    #[test]
    fn entering_a_loaded_model_field_reopens_the_picker() {
        let mut wizard = setup();
        wizard.step = Step::Model;
        wizard.field = 0;
        wizard.set_models(vec!["gpt-5".to_string()]);
        assert!(wizard.popup.is_some());
        wizard.handle_popup(KeyEvent::from(KeyCode::Esc));
        assert!(wizard.popup.is_none());
        assert_eq!(
            wizard.handle_key(KeyEvent::from(KeyCode::Enter)),
            Outcome::Redraw
        );
        assert!(wizard.popup.is_some(), "the cached list reopens");
    }

    #[test]
    fn model_validation_and_start_field() {
        let mut wizard = setup();
        wizard.step = Step::Model;
        wizard.field = 3;
        wizard.model.set("");
        assert_eq!(
            wizard.handle_key(KeyEvent::from(KeyCode::Enter)),
            Outcome::Redraw
        );
        assert!(wizard.error.is_some());
        wizard.model.set("gpt-5");
        assert_eq!(
            wizard.handle_key(KeyEvent::from(KeyCode::Enter)),
            Outcome::Launch
        );
    }
}
