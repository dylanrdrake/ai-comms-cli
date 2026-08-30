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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
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
    http_client: reqwest::Client,
}

impl Client {
    pub fn new(config: Config) -> Result<Self> {
        if config.api_key.is_none() {
            return Err(anyhow!("API key not configured. Run: orca login"));
        }

        Ok(Client {
            config,
            http_client: reqwest::Client::new(),
        })
    }

    pub async fn chat(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        temperature: f32,
        tools: Option<Vec<serde_json::Value>>,
        effort_level: Option<String>,
    ) -> Result<ChatResponse> {
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
            reasoning_effort: effort_level,
        };

        let response = self
            .http_client
            .post(format!("{}/chat/completions", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key.as_ref().unwrap()))
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(anyhow!("API error: {}", error_text));
        }

        let chat_response: ChatResponse = response.json().await?;
        Ok(chat_response)
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let response = self
            .http_client
            .get(format!("{}/models", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_key.as_ref().unwrap()))
            .send()
            .await?;

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
