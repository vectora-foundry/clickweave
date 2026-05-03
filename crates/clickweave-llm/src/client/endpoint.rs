use super::*;

/// Check if an LLM endpoint is reachable and the model is available.
/// Hits GET {base_url}/models and, when `model` is provided, verifies
/// it appears in the response. Returns Ok(()) on success.
pub async fn check_endpoint(
    base_url: &str,
    api_key: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("HTTP client error")?;

    let mut req = client.get(&url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = match req.send().await {
        Ok(resp) if resp.status().is_success() => resp,
        Ok(resp) => {
            anyhow::bail!(
                "Endpoint responded with status {} at {}",
                resp.status(),
                url
            );
        }
        Err(e) if e.is_timeout() => {
            anyhow::bail!("Endpoint timed out after 5s at {}", url);
        }
        Err(e) => {
            return Err(anyhow!(e)).with_context(|| format!("Cannot reach endpoint at {}", url));
        }
    };

    // If a model name is provided, verify it exists in the response
    let model = match model {
        Some(m) if !m.is_empty() => m,
        _ => return Ok(()),
    };

    let body = resp.text().await.context("Failed to read response")?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| anyhow!("Endpoint did not return valid JSON"))?;

    // Fuzzy match: server may report a prefixed or suffixed ID
    // (e.g. "/models/Qwen3-27B" or "Qwen3-27B.gguf") vs bare config name.
    fn strip_model_ext(s: &str) -> &str {
        s.strip_suffix(".gguf")
            .or_else(|| s.strip_suffix(".bin"))
            .unwrap_or(s)
    }
    let model_bare = strip_model_ext(model);
    let has_model = json["data"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|m| {
                let id = m["id"].as_str().unwrap_or("");
                let id_bare = strip_model_ext(id);
                id_bare == model_bare
                    || id_bare.ends_with(model_bare)
                    || model_bare.ends_with(id_bare)
            })
        })
        .unwrap_or(false);

    if has_model {
        Ok(())
    } else {
        Err(anyhow!("Model '{}' not found on endpoint", model))
    }
}

/// Fetch the list of model IDs available at `{base_url}/models`.
///
/// Hits GET `{base_url}/models` with a 5-second timeout, parses the
/// OpenAI-shaped `{ "data": [{ "id": "..." }, ...] }` response, and returns
/// the raw `id` strings in the order the server returns them.
///
/// Returns `Err` when:
/// - The HTTP request fails or times out.
/// - The server responds with a non-2xx status.
/// - The body is not valid JSON or does not contain a `data` array.
pub async fn list_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("HTTP client error")?;

    let mut req = client.get(&url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp = match req.send().await {
        Ok(resp) if resp.status().is_success() => resp,
        Ok(resp) => {
            anyhow::bail!(
                "Endpoint responded with status {} at {}",
                resp.status(),
                url
            );
        }
        Err(e) if e.is_timeout() => {
            anyhow::bail!("Endpoint timed out after 5s at {}", url);
        }
        Err(e) => {
            return Err(anyhow!(e)).with_context(|| format!("Cannot reach endpoint at {}", url));
        }
    };

    let body = resp.text().await.context("Failed to read response")?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| anyhow!("Endpoint did not return valid JSON"))?;

    let data = json["data"].as_array().ok_or_else(|| {
        anyhow!("Response missing 'data' array; endpoint may not be OpenAI-compatible")
    })?;

    let ids: Vec<String> = data
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect();

    Ok(ids)
}
