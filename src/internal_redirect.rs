use std::time::Duration;

use url::Url;

use crate::error::{AppError, AppResult};

pub async fn resolve_redirect_location(
    url: &str,
    timeout_seconds: u64,
    user_agent: &str,
) -> AppResult<String> {
    let parsed = Url::parse(url)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(timeout_seconds))
        .build()?;

    let mut request = client.get(parsed.clone());
    if !user_agent.trim().is_empty() {
        request = request.header(reqwest::header::USER_AGENT, user_agent.trim());
    }
    let response = request.send().await?;
    if response.status().is_redirection()
        && let Some(location) = response.headers().get(reqwest::header::LOCATION)
        && let Ok(location) = location.to_str()
    {
        return Ok(parsed.join(location)?.to_string());
    }
    if !response.status().is_success() && !response.status().is_redirection() {
        return Err(AppError::BadGateway(format!(
            "internal redirect probe returned {}",
            response.status()
        )));
    }

    Ok(response.url().to_string())
}
