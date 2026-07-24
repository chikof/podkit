use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

/// Derives [`axum::response::IntoResponse`] for an error enum, returning JSON
/// bodies of the form `{"error": "<message>"}`.
///
/// Each variant must carry a `#[status(StatusCode::...)]` attribute that sets
/// the HTTP status code. The error message is taken from [`std::fmt::Display`]
/// (i.e. whatever `#[error("...")]` from `thiserror` produces).
///
/// # Example
///
/// ```ignore
/// #[derive(Debug, thiserror::Error, JsonError)]
/// pub enum MyError {
///     #[error("not found")]
///     #[status(StatusCode::NOT_FOUND)]
///     NotFound,
///
///     #[error("db error: {0}")]
///     #[status(StatusCode::INTERNAL_SERVER_ERROR)]
///     Db(#[from] sqlx::Error),
/// }
/// ```
#[proc_macro_derive(JsonError, attributes(status))]
pub fn derive_json_error(input: TokenStream) -> TokenStream {
	let input = parse_macro_input!(input as DeriveInput);
	let name = &input.ident;

	let Data::Enum(data) = &input.data else {
		return syn::Error::new_spanned(name, "JsonError can only be derived for enums")
			.to_compile_error()
			.into();
	};

	let arms = data.variants.iter().map(|variant| {
		let variant_name = &variant.ident;

		let status = variant
			.attrs
			.iter()
			.find(|attr| attr.path().is_ident("status"))
			.map(syn::Attribute::parse_args::<syn::Expr>)
			.transpose();

		let status = match status {
			Ok(Some(expr)) => expr,
			Ok(None) => {
				return quote! {
					compile_error!("missing #[status(...)] on variant");
				};
			}
			Err(e) => return e.to_compile_error(),
		};

		let pattern = match &variant.fields {
			Fields::Unit => quote! { Self::#variant_name },
			Fields::Unnamed(_) => quote! { Self::#variant_name(..) },
			Fields::Named(_) => quote! { Self::#variant_name { .. } },
		};

		quote! { #pattern => #status }
	});

	quote! {
		impl ::axum::response::IntoResponse for #name {
			fn into_response(self) -> ::axum::response::Response {
				let error = self.to_string();
				let status = match self {
					#( #arms, )*
				};
				#[derive(::serde::Serialize)]
				struct __Body { error: String }
				(status, ::axum::Json(__Body { error })).into_response()
			}
		}
	}
	.into()
}
