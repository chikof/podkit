#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Language {
    En,
    Es
}

impl Default for Language {
    fn default() -> Self {
        Language::En
    }
}

//#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] // considerated as too much
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSettings {
    pub two_factor_enabled: bool,
    pub preferred_language: Language,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            two_factor_enabled: false,
            preferred_language: Language::En,
        }
    }
}