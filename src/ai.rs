use std::sync::OnceLock;

static CACHED_API_KEY: OnceLock<Option<String>> = OnceLock::new();

/// Read the Anthropic API key from cosmic-edit's config.
pub fn get_api_key() -> Option<String> {
    CACHED_API_KEY
        .get_or_init(|| {
            let path = dirs_for_key()?;
            let content = std::fs::read_to_string(&path).ok()?;
            parse_ron_option_string(&content)
        })
        .clone()
}

fn dirs_for_key() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::PathBuf::from(home)
        .join(".config/cosmic/com.system76.CosmicEdit/v1/anthropic_api_key");
    if path.exists() { Some(path) } else { None }
}

fn parse_ron_option_string(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed == "None" || trimmed.is_empty() {
        return None;
    }
    if let Some(inner) = trimmed.strip_prefix("Some(") {
        let inner = inner.strip_suffix(')')?;
        return parse_quoted_string(inner);
    }
    if let Some(val) = parse_quoted_string(trimmed) {
        return Some(val);
    }
    if trimmed.starts_with("sk-") {
        return Some(trimmed.to_string());
    }
    None
}

fn parse_quoted_string(s: &str) -> Option<String> {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

pub fn sanitize_title(name: &str) -> String {
    let mut s = name.trim().to_string();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s = s[1..s.len() - 1].to_string();
    }
    s = s.trim().to_string();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s
}

/// Suggest a short window title based on terminal viewport content.
pub async fn suggest_terminal_title(content: &str) -> Option<String> {
    let api_key = get_api_key()?;

    if content.trim().is_empty() {
        return None;
    }

    let truncated = if content.len() > 10_240 {
        &content[..10_240]
    } else {
        content
    };

    let client = misanthropy::Anthropic::new(&api_key);

    let mut request = misanthropy::MessagesRequest::default()
        .with_model("claude-3-5-haiku-latest".to_string())
        .with_max_tokens(30);

    request.add_system(misanthropy::Content::text(
        "Suggest a short, descriptive window title (3-8 words) for this terminal session \
         based on the visible content. Respond with ONLY the title, no quotes or explanation.",
    ));
    request.add_user(misanthropy::Content::text(truncated));

    match client.messages(&request).await {
        Ok(response) => {
            let text = response.format_content();
            let title = sanitize_title(&text);
            if title.is_empty() { None } else { Some(title) }
        }
        Err(e) => {
            log::warn!("AI title suggestion failed: {e}");
            None
        }
    }
}
