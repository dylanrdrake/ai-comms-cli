use crate::config::Config;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCall {
    pub id: String,
    pub function: FunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    /// Flat effort field, e.g. OrcaRouter's `reasoning_effort: "high"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Nested effort field, e.g. OpenRouter's `reasoning: { "effort": "high" }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningEffort>,
}

#[derive(Debug, Serialize)]
pub struct ReasoningEffort {
    pub effort: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub message: ChatMessage,
}

#[derive(Debug, Deserialize)]
pub struct ModelList {
    pub data: Vec<Model>,
}

#[derive(Debug, Deserialize)]
pub struct Model {
    pub id: String,
}

pub struct Client {
    config: Config,
    api_key: String,
    http_client: reqwest::Client,
}

impl Client {
    pub fn new(config: Config) -> Result<Self> {
        let api_key = crate::config::get_api_key()?
            .ok_or_else(|| anyhow!("API key not configured. Run: orca login"))?;

        Ok(Client {
            config,
            api_key,
            http_client: reqwest::Client::new(),
        })
    }

    /// Applies the `Authorization` header plus any user-configured
    /// `extra_headers` (e.g. OpenRouter's optional `HTTP-Referer`/`X-Title`)
    /// to an outgoing request.
    fn apply_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req = req.header("Authorization", format!("Bearer {}", self.api_key));
        for (key, value) in &self.config.extra_headers {
            req = req.header(key.as_str(), value.as_str());
        }
        req
    }

    pub async fn chat(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        temperature: f32,
        tools: Option<Vec<serde_json::Value>>,
        effort_level: Option<String>,
    ) -> Result<ChatResponse> {
        let effort_style = self
            .config
            .effort_style
            .as_deref()
            .unwrap_or(crate::config::DEFAULT_EFFORT_STYLE);

        let (reasoning_effort, reasoning) = match (&effort_level, effort_style) {
            (Some(effort), "flat") => (Some(effort.clone()), None),
            (Some(effort), "nested") => (
                None,
                Some(ReasoningEffort {
                    effort: effort.clone(),
                }),
            ),
            _ => (None, None),
        };

        let request = ChatRequest {
            model,
            messages,
            tools: tools.clone(),
            temperature,
            tool_choice: if tools.is_some() {
                Some("auto".to_string())
            } else {
                None
            },
            reasoning_effort,
            reasoning,
        };

        let req = self
            .http_client
            .post(format!("{}/chat/completions", self.config.base_url));
        let response = self.apply_headers(req).json(&request).send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(anyhow!("API error: {}", error_text));
        }

        let chat_response: ChatResponse = response.json().await?;
        Ok(chat_response)
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let req = self
            .http_client
            .get(format!("{}/models", self.config.base_url));
        let response = self.apply_headers(req).send().await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(anyhow!("API error: {}", error_text));
        }

        let model_list: ModelList = response.json().await?;
        Ok(model_list.data.into_iter().map(|m| m.id).collect())
    }
}

#[cfg(test)]
mod deser_tests {
    use super::*;

    #[test]
    fn chat_message_deserializes_without_tool_call_id() {
        let json = r#"{"role":"assistant","content":"hi"}"#;
        let m: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(m.role, "assistant");
        assert_eq!(m.tool_call_id, None);
    }
}
