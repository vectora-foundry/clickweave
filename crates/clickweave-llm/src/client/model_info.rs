use super::*;

impl LlmClient {
    pub(crate) async fn try_models_endpoint(
        &self,
        url: &str,
        model_id: &str,
    ) -> Result<Option<ModelInfo>> {
        let mut req = self.http.get(url);
        if let Some(api_key) = &self.config.api_key {
            req = req.bearer_auth(api_key);
        }

        let response = req
            .send()
            .await
            .context("Failed to query models endpoint")?;

        if !response.status().is_success() {
            debug!(url = %url, status = %response.status(), "Models endpoint returned error");
            return Ok(None);
        }

        let response_text = response
            .text()
            .await
            .context("Failed to read models response")?;

        trace!(url = %url, response = %response_text, "Raw models response");

        let body: ModelsResponse =
            serde_json::from_str(&response_text).context("Failed to parse models response")?;

        let info = body
            .data
            .into_iter()
            .find(|m| m.id == model_id || model_id.contains(&m.id) || m.id.contains(model_id));

        Ok(info)
    }
}
