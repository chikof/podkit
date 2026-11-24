//! Turning an upload into the one picture podkit keeps: decode, turn upright, crop to a
//! square around the centre, resize to [`SIZE`] px and encode as lossless WebP.
//!
//! Re-encoding is the whole safety story. The bytes that leave this module were written by
//! the encoder, so no EXIF, ICC profile, comment or trailing data from the upload survives,
//! and what is served can only be a WebP. SVG is not an image format the decoder knows, so
//! it never gets this far.

use std::io::Cursor;

use image::{
	DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits,
	codecs::webp::WebPEncoder, imageops::FilterType,
};
use sha2::{Digest, Sha256};

use crate::error::{AppResult, ServerError};

/// The largest upload, in bytes.
pub const MAX_UPLOAD: usize = 2 * 1024 * 1024;

/// The side of the stored square, in pixels.
pub const SIZE: u32 = 256;

/// What the decoder may allocate and how big a picture it may open; a small file can
/// claim to be a very large image.
const MAX_SIDE: u32 = 8192;
const MAX_ALLOC: u64 = 256 * 1024 * 1024;

/// The stored form of an upload.
#[derive(Debug, Clone)]
pub struct Processed {
	pub mime: &'static str,
	pub bytes: Vec<u8>,
	/// A short digest of `bytes`, for the image URL.
	pub hash: String,
}

fn invalid(message: &str) -> ServerError {
	ServerError::Validation(message.to_owned())
}

/// The image format a declared media type names; `None` for anything else (SVG included).
fn declared_format(mime: &str) -> Option<ImageFormat> {
	match mime.trim().to_ascii_lowercase().as_str() {
		"image/png" => Some(ImageFormat::Png),
		"image/jpeg" | "image/jpg" => Some(ImageFormat::Jpeg),
		"image/webp" => Some(ImageFormat::WebP),
		"image/gif" => Some(ImageFormat::Gif),
		_ => None,
	}
}

/// Checks and normalizes an upload.
///
/// # Errors
/// `PayloadTooLarge` over [`MAX_UPLOAD`]; `Validation` when the declared type is not PNG,
/// JPEG, WebP or GIF, when the bytes are not an image of that type, or when the picture
/// cannot be decoded.
pub fn process(declared: Option<&str>, data: &[u8]) -> AppResult<Processed> {
	if data.len() > MAX_UPLOAD {
		return Err(ServerError::PayloadTooLarge(
			"The picture is over 2 MB. Choose a smaller one.".to_owned(),
		));
	}
	if data.is_empty() {
		return Err(invalid("Choose a picture to upload."));
	}
	let Some(declared) = declared.and_then(declared_format) else {
		return Err(invalid("Use a PNG, JPEG, WebP or GIF picture."));
	};
	// The decoder looks at the bytes, never at the name or the declared type.
	let sniffed = image::guess_format(data)
		.map_err(|_| invalid("That file is not a PNG, JPEG, WebP or GIF picture."))?;
	if sniffed != declared {
		return Err(invalid(
			"The file is not the kind of picture its type says.",
		));
	}

	let mut reader = ImageReader::with_format(Cursor::new(data), sniffed);
	let mut limits = Limits::default();
	limits.max_image_width = Some(MAX_SIDE);
	limits.max_image_height = Some(MAX_SIDE);
	limits.max_alloc = Some(MAX_ALLOC);
	reader.limits(limits);
	let unreadable = |_| invalid("That picture could not be read.");
	let mut decoder = reader.into_decoder().map_err(unreadable)?;
	let orientation = decoder.orientation().map_err(unreadable)?;
	// A GIF decodes to its first frame.
	let mut picture = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
	picture.apply_orientation(orientation);

	let square = picture
		.resize_to_fill(SIZE, SIZE, FilterType::Lanczos3)
		.to_rgba8();
	let mut bytes = Vec::new();
	WebPEncoder::new_lossless(&mut bytes)
		.write_image(square.as_raw(), SIZE, SIZE, ExtendedColorType::Rgba8)
		.map_err(|_| ServerError::Internal)?;
	let hash = hex(&Sha256::digest(&bytes)[..8]);
	Ok(Processed {
		mime: "image/webp",
		bytes,
		hash,
	})
}

fn hex(bytes: &[u8]) -> String {
	use std::fmt::Write as _;
	bytes.iter().fold(String::new(), |mut out, b| {
		let _ = write!(out, "{b:02x}");
		out
	})
}
