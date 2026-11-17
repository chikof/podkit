//! Presigned requests against a backup target. The control plane never
//! streams dump bytes: the agent uploads and downloads through these urls.

use std::time::Duration;

use reqwest::StatusCode;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use url::Url;

use super::BackupTarget;

/// Long enough for a large dump to transfer on a slow link.
const SIGNATURE_TTL: Duration = Duration::from_hours(1);

/// The S3 operations a backup needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
	Put,
	Get,
	Delete,
}

/// Messages never contain a presigned url, since its query carries a
/// signature.
#[derive(Debug, thiserror::Error)]
pub enum S3Error {
	#[error("invalid backup target endpoint: {0}")]
	Endpoint(String),
	#[error("invalid backup target bucket: {0}")]
	Bucket(String),
	#[error("storage request failed: {0}")]
	Request(String),
	#[error("storage answered {0}")]
	Status(StatusCode),
}

/// Builds the bucket for an endpoint. Fails when the endpoint cannot address
/// the bucket, e.g. virtual-host style on an IP address.
///
/// # Errors
/// [`S3Error::Bucket`] with the reason.
pub fn bucket(
	endpoint: Url,
	path_style: bool,
	name: &str,
	region: &str,
) -> Result<Bucket, S3Error> {
	let style = if path_style {
		UrlStyle::Path
	} else {
		UrlStyle::VirtualHost
	};
	Bucket::new(endpoint, style, name.to_owned(), region.to_owned())
		.map_err(|e| S3Error::Bucket(e.to_string()))
}

/// A url that performs `method` on `key` for the next hour.
///
/// # Errors
/// [`S3Error`] when the stored endpoint or bucket cannot form a url.
pub fn presign(
	target: &BackupTarget,
	secret: &str,
	method: Method,
	key: &str,
) -> Result<Url, S3Error> {
	let endpoint = Url::parse(&target.endpoint).map_err(|e| S3Error::Endpoint(e.to_string()))?;
	let bucket = bucket(endpoint, target.path_style, &target.bucket, &target.region)?;
	let credentials = Credentials::new(target.access_key_id.clone(), secret.to_owned());
	Ok(match method {
		Method::Put => bucket
			.put_object(Some(&credentials), key)
			.sign(SIGNATURE_TTL),
		Method::Get => bucket
			.get_object(Some(&credentials), key)
			.sign(SIGNATURE_TTL),
		Method::Delete => bucket
			.delete_object(Some(&credentials), key)
			.sign(SIGNATURE_TTL),
	})
}

/// Deletes `key` from the target. An object that is already gone counts as
/// deleted.
///
/// # Errors
/// [`S3Error`] when the request cannot be sent or storage refuses it.
pub async fn delete(
	http: &reqwest::Client,
	target: &BackupTarget,
	secret: &str,
	key: &str,
) -> Result<(), S3Error> {
	let url = presign(target, secret, Method::Delete, key)?;
	let response = http
		.delete(url)
		.send()
		.await
		.map_err(|e| S3Error::Request(e.without_url().to_string()))?;
	match response.status() {
		StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(()),
		status => Err(S3Error::Status(status)),
	}
}

#[cfg(test)]
mod tests {
	use time::OffsetDateTime;

	use super::*;
	use crate::ids::{BackupTargetId, TeamId};

	fn target(endpoint: &str, path_style: bool) -> BackupTarget {
		BackupTarget {
			id: BackupTargetId(1),
			team_id: TeamId(1),
			name: "local".to_owned(),
			endpoint: endpoint.to_owned(),
			region: "us-east-1".to_owned(),
			bucket: "podkit".to_owned(),
			path_style,
			prefix: String::new(),
			access_key_id: "AKIDEXAMPLE".to_owned(),
			secret_access_key: Vec::new(),
			created_at: OffsetDateTime::UNIX_EPOCH,
		}
	}

	#[test]
	fn presigned_put_addresses_the_bucket_by_path_and_is_signed() {
		let url = presign(
			&target("http://127.0.0.1:9000", true),
			"secret",
			Method::Put,
			"team/databases/1/2.sql.gz",
		)
		.unwrap();
		assert_eq!(url.host_str(), Some("127.0.0.1"));
		assert_eq!(url.port(), Some(9000));
		assert_eq!(url.path(), "/podkit/team/databases/1/2.sql.gz");
		assert!(url.query_pairs().any(|(k, _)| k == "X-Amz-Signature"));
	}

	#[test]
	fn virtual_host_style_moves_the_bucket_into_the_host() {
		let url = presign(
			&target("https://s3.example.com", false),
			"secret",
			Method::Get,
			"a.sql.gz",
		)
		.unwrap();
		assert_eq!(url.host_str(), Some("podkit.s3.example.com"));
		assert_eq!(url.path(), "/a.sql.gz");
	}

	#[test]
	fn virtual_host_style_cannot_address_an_ip_endpoint() {
		let err = presign(
			&target("http://127.0.0.1:9000", false),
			"secret",
			Method::Get,
			"a.sql.gz",
		)
		.unwrap_err();
		assert!(matches!(err, S3Error::Bucket(_)));
	}
}
