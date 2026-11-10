use std::time::Duration;

use url::Url;

use crate::error::{AppError, AppResult};

pub async fn resolve_redirect_location(
    client: &reqwest::Client,
    url: &str,
    timeout_seconds: u64,
    user_agent: &str,
) -> AppResult<String> {
    Ok(resolve_redirect_observed(
        client,
        url,
        timeout_seconds,
        user_agent,
        &mut Default::default(),
    )
    .await?
    .url)
}

pub(crate) struct RedirectResolution {
    pub url: String,
    /// Observed Location hops only. Auto-following clients cannot reveal hop count.
    pub redirect_count: Option<u32>,
}

pub(crate) async fn resolve_redirect_observed(
    client: &reqwest::Client,
    url: &str,
    timeout_seconds: u64,
    user_agent: &str,
    observation: &mut crate::diagnostics::UpstreamObservation,
) -> AppResult<RedirectResolution> {
    let parsed = Url::parse(url)?;
    let mut request = client
        .get(parsed.clone())
        .timeout(Duration::from_secs(timeout_seconds));
    if !user_agent.trim().is_empty() {
        request = request.header(reqwest::header::USER_AGENT, user_agent.trim());
    }
    let started = std::time::Instant::now();
    let response =
        crate::diagnostics::network::observe_send(observation, async { request.send().await })
            .await;
    let response = response.map_err(|err| {
        observation.error_kind = Some(crate::error::reqwest_error_kind(&err).to_string());
        AppError::from(err)
    })?;
    observation.response_headers_ms = Some(started.elapsed().as_millis() as u64);
    observation.status = Some(response.status().as_u16());
    if response.status().is_redirection()
        && let Some(location) = response.headers().get(reqwest::header::LOCATION)
        && let Ok(location) = location.to_str()
    {
        return Ok(RedirectResolution {
            url: parsed.join(location)?.to_string(),
            redirect_count: (response.url() == &parsed).then_some(1),
        });
    }
    if !response.status().is_success() && !response.status().is_redirection() {
        observation.error_kind = Some("upstream_http".to_string());
        return Err(AppError::BadGateway(format!(
            "internal redirect probe returned {}",
            response.status()
        )));
    }

    Ok(RedirectResolution {
        url: response.url().to_string(),
        redirect_count: (response.url() == &parsed).then_some(0),
    })
}
