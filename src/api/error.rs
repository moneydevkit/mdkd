use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::types::ApiError;

pub enum AppError {
	BadRequest(String),
	NotFound(String),
	Internal(String),
}

impl IntoResponse for AppError {
	fn into_response(self) -> Response {
		let (status, code, message) = match self {
			AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg),
			AppError::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg),
			AppError::Internal(msg) => {
				(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", msg)
			},
		};

		(status, Json(ApiError { error: message, code: code.into() })).into_response()
	}
}
