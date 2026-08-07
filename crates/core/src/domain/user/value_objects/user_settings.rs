#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Language {
	#[default]
	En,
	Es,
}

//#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] // considerated as too much
/// Per-user preferences.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserSettings {
	/// Whether two-factor auth is turned on.
	pub two_factor_enabled: bool, // false by default
	/// UI language preference.
	pub preferred_language: Language,
}
