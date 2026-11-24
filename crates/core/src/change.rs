//! A field of an update that can be left alone, cleared, or set: JSON that leaves it out
//! keeps it, `null` clears it, a value sets it.

use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Change<T> {
	/// Not mentioned: leave the stored value.
	#[default]
	Keep,
	/// `null`: remove the stored value.
	Clear,
	/// A new value.
	Set(T),
}

impl<T> Change<T> {
	/// Whether the update touches the field at all.
	pub fn is_touched(&self) -> bool {
		!matches!(self, Self::Keep)
	}

	/// The value to store when the field is touched: `None` when cleared.
	pub fn value(self) -> Option<T> {
		match self {
			Self::Set(v) => Some(v),
			Self::Keep | Self::Clear => None,
		}
	}
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Change<T> {
	fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		Ok(match Option::<T>::deserialize(d)? {
			Some(v) => Self::Set(v),
			None => Self::Clear,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(Deserialize)]
	struct Body {
		#[serde(default)]
		field: Change<i64>,
	}

	#[test]
	fn absent_null_and_value_are_three_different_things() {
		let parse = |json: &str| serde_json::from_str::<Body>(json).unwrap().field;
		assert_eq!(parse("{}"), Change::Keep);
		assert_eq!(parse(r#"{"field": null}"#), Change::Clear);
		assert_eq!(parse(r#"{"field": 4}"#), Change::Set(4));
		assert!(!Change::<i64>::Keep.is_touched());
		assert!(Change::<i64>::Clear.is_touched() && Change::Set(1).is_touched());
		assert_eq!(Change::Set(1).value(), Some(1));
		assert_eq!(Change::<i64>::Clear.value(), None);
	}
}
