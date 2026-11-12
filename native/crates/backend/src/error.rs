use serde::Serialize;

/// Every error surfaced to the frontend carries a short user-friendly
/// message plus the technical detail, which callers should log but never
/// display directly to the user.
#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub message: String,
    pub technical: String,
    pub retryable: bool,
}

impl AppError {
    pub fn new(message: impl Into<String>, technical: impl Into<String>) -> Self {
        let technical = technical.into();
        log::error!("{}", technical);
        Self {
            message: message.into(),
            technical,
            retryable: false,
        }
    }

    pub fn retryable(message: impl Into<String>, technical: impl Into<String>) -> Self {
        let mut err = Self::new(message, technical);
        err.retryable = true;
        err
    }

    pub fn offline() -> Self {
        Self::new(
            "You're offline. Connect to the internet to use this feature.",
            "network request attempted while offline",
        )
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(
            format!("{what} could not be found."),
            format!("404: {what}"),
        )
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            AppError::retryable("The request timed out.", err.to_string())
        } else if err.is_connect() {
            AppError::retryable(
                "Couldn't reach the server. Check your connection.",
                err.to_string(),
            )
        } else if let Some(status) = err.status() {
            AppError::new(
                format!("The server returned an error ({status})."),
                err.to_string(),
            )
        } else {
            AppError::retryable("A network error occurred.", err.to_string())
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::new("A file operation failed.", err.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::new("Received unexpected data.", err.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
