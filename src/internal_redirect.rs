use std::time::Duration;

use url::Url;

use crate::error::{AppError, AppResult};

pub async fn resolve_with_head(url: &str, timeout_seconds: u64) -> AppResult<String> {
    let parsed = Url::parse(url)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(Duration::from_secs(timeout_seconds))
        .build()?;

    let response = client.head(parsed).send().await?;
    if !response.status().is_success() && !response.status().is_redirection() {
        return Err(AppError::BadGateway(format!(
            "internal redirect HEAD returned {}",
            response.status()
        )));
    }

    Ok(response.url().to_string())
}
