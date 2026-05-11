use super::*;

/// Call the VLM to analyze images and return a text summary.
pub async fn analyze_images(
    vlm: &(impl ChatBackend + ?Sized),
    step_goal: &str,
    tool_name: &str,
    images: Vec<(String, String)>,
) -> Result<String> {
    let messages = vec![
        Message::system(vlm_system_prompt()),
        Message::user_with_images(build_vlm_prompt(step_goal, tool_name), images),
    ];

    let response = vlm.chat(&messages, None).await?;

    let text = response
        .choices
        .first()
        .and_then(|c| c.message.content_text())
        .unwrap_or("")
        .to_string();

    Ok(text)
}
