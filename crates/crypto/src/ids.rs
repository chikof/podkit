pub fn generate_id(len: Option<usize>) -> String {
	nanoid::nanoid!(len.unwrap_or(21))
}
