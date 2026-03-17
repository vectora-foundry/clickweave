use super::{ExecutorError, ExecutorResult, WorkflowExecutor};
use clickweave_core::decision_cache::{self, ClickDisambiguation, ElementResolution};
use clickweave_core::{ExecutionMode, NodeRun};
use clickweave_llm::{ChatBackend, Message};
use serde_json::Value;
use tracing::debug;
use uuid::Uuid;

/// Extract the `available_elements` array from a find_text response.
///
/// When find_text returns no matches, the response is two content blocks
/// joined by `\n`: `[]\n{"available_elements": ["Multiply", "Divide", ...]}`.
///
/// Scans for JSON objects in the text and checks each for an
/// `available_elements` key, so it works regardless of whitespace,
/// key ordering, or additional fields in the object.
pub(crate) fn parse_available_elements(result_text: &str) -> Option<Vec<String>> {
    let mut remaining = result_text;
    while let Some(json_str) = super::app_resolve::extract_json_object(remaining) {
        if let Ok(parsed) = serde_json::from_str::<Value>(json_str)
            && let Some(arr) = parsed.get("available_elements").and_then(|v| v.as_array())
        {
            let elements: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            if !elements.is_empty() {
                return Some(elements);
            }
        }
        // Advance past this object to look for the next one
        let start_in_remaining = json_str.as_ptr() as usize - remaining.as_ptr() as usize;
        let advance = start_in_remaining + json_str.len();
        if advance >= remaining.len() {
            break;
        }
        remaining = &remaining[advance..];
    }
    None
}

impl<C: ChatBackend> WorkflowExecutor<C> {
    /// Resolve a user-provided element name (e.g. "x", "÷") to the correct
    /// accessibility element name (e.g. "Multiply", "Divide") by asking the
    /// orchestrator LLM to match against the available elements list.
    /// Results are cached so repeated references to the same target only
    /// incur one LLM call.
    pub(crate) async fn resolve_element_name(
        &self,
        node_id: Uuid,
        target: &str,
        available_elements: &[String],
        app_name: Option<&str>,
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<String> {
        let cache_key = (target.to_string(), app_name.map(|s| s.to_string()));

        // Check in-memory cache first (populated during this execution)
        if let Some(cached) = self.read_element_cache().get(&cache_key).cloned() {
            debug!(target = target, resolved_name = %cached, "element_cache hit");
            self.log(format!(
                "Element resolved (cached): \"{}\" -> \"{}\"",
                target, cached
            ));
            return Ok(cached);
        }

        // Check persistent decision cache (replays Test-mode decisions in Run mode)
        let ck = decision_cache::cache_key(node_id, target, app_name);
        if let Some(cached) = self
            .read_decision_cache()
            .element_resolution
            .get(&ck)
            .cloned()
        {
            if available_elements
                .iter()
                .any(|e| e == &cached.resolved_name)
            {
                debug!(target = target, resolved_name = %cached.resolved_name, "decision_cache hit");
                self.log(format!(
                    "Element resolved (decision cache): \"{}\" -> \"{}\"",
                    target, cached.resolved_name
                ));
                self.write_element_cache()
                    .insert(cache_key, cached.resolved_name.clone());
                return Ok(cached.resolved_name);
            }
            debug!(
                target = target,
                cached_name = %cached.resolved_name,
                "decision_cache hit but name not in available elements, falling through to LLM"
            );
        }

        let app_context = match app_name {
            Some(name) => format!(" in app \"{}\"", name),
            None => String::new(),
        };

        let elements_list = available_elements
            .iter()
            .map(|e| format!("- {}", e))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "You are resolving a UI element name. The user wants to interact with: \"{target}\"{app_context}\n\
             \n\
             Available UI elements:\n\
             {elements_list}\n\
             \n\
             Which element name matches what the user means? Return ONLY a JSON object:\n\
             {{\"name\": \"<exact element name from the list above>\"}}\n\
             \n\
             IMPORTANT: The name MUST be an exact match from the list above.\n\
             Common mappings: \u{00d7} = Multiply, \u{00f7} = Divide, \u{2212} = Subtract, AC = All Clear.\n\
             If no element is a plausible match, return:\n\
             {{\"name\": null}}"
        );

        let messages = vec![Message::user(prompt)];
        let response = self
            .reasoning_backend()
            .chat(messages, None)
            .await
            .map_err(|e| ExecutorError::ElementResolution(format!("LLM error: {}", e)))?;

        let choice = response.choices.first().ok_or_else(|| {
            ExecutorError::ElementResolution(
                "No response from LLM during element resolution".to_string(),
            )
        })?;

        let raw_text = choice.message.content_text().ok_or_else(|| {
            ExecutorError::ElementResolution(
                "LLM returned empty content during element resolution".to_string(),
            )
        })?;

        let json_text = super::app_resolve::parse_llm_json_response(raw_text).ok_or_else(|| {
            ExecutorError::ElementResolution(format!(
                "No JSON object found in LLM response (raw: {})",
                raw_text
            ))
        })?;

        let parsed: Value = serde_json::from_str(json_text).map_err(|e| {
            ExecutorError::ElementResolution(format!(
                "Failed to parse LLM response as JSON: {} (raw: {})",
                e, raw_text
            ))
        })?;

        let name = parsed["name"].as_str().ok_or_else(|| {
            ExecutorError::ElementResolution(format!(
                "Element \"{}\" not found in available elements (LLM found no match)",
                target
            ))
        })?;

        // Post-validate: ensure the LLM returned a name that actually appears in the list.
        if !available_elements.iter().any(|e| e == name) {
            return Err(ExecutorError::ElementResolution(format!(
                "Element \"{}\" not found (resolved name \"{}\" not in available elements list)",
                target, name
            )));
        }

        self.record_event(
            node_run,
            "element_resolved",
            serde_json::json!({
                "target": target,
                "resolved_name": name,
                "app_name": app_name,
            }),
        );

        self.log(format!("Element resolved: \"{}\" -> \"{}\"", target, name));

        self.write_element_cache()
            .insert(cache_key, name.to_string());

        // Record in decision cache for replay in Run mode
        if self.execution_mode == ExecutionMode::Test {
            let ck = decision_cache::cache_key(node_id, target, app_name);
            self.write_decision_cache().element_resolution.insert(
                ck,
                ElementResolution {
                    target: target.to_string(),
                    resolved_name: name.to_string(),
                },
            );
        }

        Ok(name.to_string())
    }

    /// When find_text returns multiple matches for a click target, ask the LLM
    /// to pick the most appropriate one based on text, role, and position.
    pub(crate) async fn disambiguate_click_matches(
        &self,
        node_id: Uuid,
        target: &str,
        matches: &[Value],
        app_name: Option<&str>,
        node_run: Option<&NodeRun>,
    ) -> ExecutorResult<usize> {
        let app_context = match app_name {
            Some(name) => format!(" in app \"{}\"", name),
            None => String::new(),
        };

        let matches_list = matches
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let text = m["text"].as_str().unwrap_or("?");
                let role = m["role"].as_str().unwrap_or("unknown");
                let x = m["x"].as_f64().unwrap_or(0.0);
                let y = m["y"].as_f64().unwrap_or(0.0);
                format!(
                    "{}: text=\"{}\" role=\"{}\" at ({:.0}, {:.0})",
                    i, text, role, x, y
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        debug!(
            target = target,
            match_count = matches.len(),
            "disambiguating click matches"
        );

        let hint_context = self.format_supervision_hint(&format!(
            "A previous attempt to click '{}' failed. ",
            target
        ));

        let tried_context = {
            let tried = self.read_tried_click_indices();
            Self::format_tried_context(&tried, "indices")
        };

        let prompt = format!(
            "You need to click on \"{target}\"{app_context}.\n\
             \n\
             Multiple UI elements matched:\n\
             {matches_list}\n\
             \n\
             Which element should be clicked? Return ONLY a JSON object:\n\
             {{\"index\": <number>}}\n\
             \n\
             Pick the element that is most likely the intended click target.\n\
             Prefer interactive elements (buttons, links) over static display text.\n\
             Prefer exact text matches over partial/substring matches.\
             {hint_context}{tried_context}"
        );

        let messages = vec![Message::user(prompt)];
        let response = self
            .reasoning_backend()
            .chat(messages, None)
            .await
            .map_err(|e| {
                ExecutorError::ElementResolution(format!(
                    "LLM error during click disambiguation: {}",
                    e
                ))
            })?;

        let choice = response.choices.first().ok_or_else(|| {
            ExecutorError::ElementResolution(
                "No response from LLM during click disambiguation".to_string(),
            )
        })?;

        let raw_text = choice.message.content_text().ok_or_else(|| {
            ExecutorError::ElementResolution(
                "LLM returned empty content during click disambiguation".to_string(),
            )
        })?;

        let json_text = super::app_resolve::parse_llm_json_response(raw_text).ok_or_else(|| {
            ExecutorError::ElementResolution(format!(
                "No JSON object found in LLM response (raw: {})",
                raw_text
            ))
        })?;

        let parsed: Value = serde_json::from_str(json_text).map_err(|e| {
            ExecutorError::ElementResolution(format!(
                "Failed to parse LLM response: {} (raw: {})",
                e, raw_text
            ))
        })?;

        let index = parsed["index"].as_u64().ok_or_else(|| {
            ExecutorError::ElementResolution(format!(
                "LLM returned no valid index for click disambiguation (raw: {})",
                raw_text
            ))
        })? as usize;

        if index >= matches.len() {
            return Err(ExecutorError::ElementResolution(format!(
                "LLM returned out-of-bounds index {} for {} matches",
                index,
                matches.len()
            )));
        }

        self.write_tried_click_indices().push(index);

        let chosen = &matches[index];
        let chosen_text = chosen["text"].as_str().unwrap_or("?");
        let chosen_role = chosen["role"].as_str().unwrap_or("unknown");

        self.record_event(
            node_run,
            "match_disambiguated",
            serde_json::json!({
                "target": target,
                "match_count": matches.len(),
                "chosen_index": index,
                "chosen_text": chosen_text,
                "chosen_role": chosen_role,
            }),
        );

        self.log(format!(
            "Disambiguated '{}' -> index {} (text=\"{}\", role={})",
            target, index, chosen_text, chosen_role
        ));

        // Record decision in cache for replay in Run mode
        if self.execution_mode == ExecutionMode::Test {
            let ck = decision_cache::cache_key(node_id, target, app_name);
            self.write_decision_cache().click_disambiguation.insert(
                ck,
                ClickDisambiguation {
                    target: target.to_string(),
                    app_name: app_name.map(|s| s.to_string()),
                    chosen_text: chosen_text.to_string(),
                    chosen_role: chosen_role.to_string(),
                },
            );
        }

        Ok(index)
    }

    /// Remove a cached element resolution so the next attempt re-resolves via LLM.
    pub(crate) fn evict_element_cache(&self, target: &str, app_name: Option<&str>) {
        let cache_key = (target.to_string(), app_name.map(|s| s.to_string()));
        if self.write_element_cache().remove(&cache_key).is_some() {
            debug!(target = target, app_name = ?app_name, "evicted element_cache entry");
            self.log(format!("Element cache evicted for \"{}\"", target));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_available_elements_from_find_text_response() {
        let input = "[]\n{\"available_elements\":[\"Calculator\",\"Multiply\",\"Divide\"]}";
        assert_eq!(
            parse_available_elements(input),
            Some(vec![
                "Calculator".to_string(),
                "Multiply".to_string(),
                "Divide".to_string(),
            ])
        );
    }

    #[test]
    fn parse_available_elements_matches_only() {
        let input = "[{\"text\":\"2\",\"x\":100,\"y\":200}]";
        assert_eq!(parse_available_elements(input), None);
    }

    #[test]
    fn parse_available_elements_empty_string() {
        assert_eq!(parse_available_elements(""), None);
    }

    #[test]
    fn parse_available_elements_just_empty_array() {
        assert_eq!(parse_available_elements("[]"), None);
    }

    #[test]
    fn parse_available_elements_pretty_printed() {
        let input =
            "[]\n{\n  \"available_elements\": [\n    \"Calculator\",\n    \"Multiply\"\n  ]\n}";
        assert_eq!(
            parse_available_elements(input),
            Some(vec!["Calculator".to_string(), "Multiply".to_string()])
        );
    }

    #[test]
    fn parse_available_elements_extra_fields() {
        let input =
            "[]\n{\"count\":0,\"available_elements\":[\"Add\",\"Subtract\"],\"source\":\"a11y\"}";
        assert_eq!(
            parse_available_elements(input),
            Some(vec!["Add".to_string(), "Subtract".to_string()])
        );
    }

    #[test]
    fn parse_available_elements_whitespace_around_key() {
        let input = "[]\n{ \"available_elements\" : [\"Divide\"] }";
        assert_eq!(
            parse_available_elements(input),
            Some(vec!["Divide".to_string()])
        );
    }
}
