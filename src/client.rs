use crate::config::Config;
use anyhow::{anyhow, Result};
use async_stream::try_stream;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// Bounds how long connecting (DNS/TCP/TLS) may take before giving up —
/// independent of how long a slow-to-answer provider may then take once
/// connected, which the per-call timeouts below cover instead.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// A non-streaming request has no partial progress to show, so it's given
/// a single generous ceiling for the whole round trip — long enough for a
/// slow reasoning model, but bounded so a stalled connection eventually
/// surfaces as an error instead of leaving the caller waiting forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// A streaming request has no such ceiling on its *total* length — a long
/// reply legitimately keeps sending chunks — so instead this bounds the gap
/// between chunks: no new bytes within this window means the connection
/// has stalled, not that the model is still thinking.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// True if `content` is `Some` and has non-whitespace text. Some
    /// providers return `content: ""` instead of `null` when a message
    /// carries no visible text (e.g. a tool-calls-only turn).
    pub fn has_visible_content(&self) -> bool {
        self.content
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty())
    }
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
    /// Omitted entirely (not sent as `null`) when there's no temperature to
    /// use — the provider then falls back to its own default, same as an
    /// omitted `reasoning`/`reasoning_effort`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    /// Flat effort field, e.g. OrcaRouter's `reasoning_effort: "high"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Nested effort field, e.g. OpenRouter's `reasoning: { "effort": "high" }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningEffort>,
    /// Only sent when streaming; omitted entirely for the buffered path so
    /// requests to providers that don't expect it are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
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

/// What a streaming turn produces, in order: any number of `Content` deltas
/// as text arrives, then exactly one `Done` carrying the fully assembled
/// message (text plus any tool calls).
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Content(String),
    Done(ChatMessage),
}

// ---------------------------------------------------------------------------
// Streaming wire format
//
// A chunk looks like:
//   {"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}
// Tool calls arrive in fragments correlated only by `index`, with
// `function.arguments` split across arbitrarily many chunks:
//   {"choices":[{"delta":{"tool_calls":[
//      {"index":0,"id":"call_1","function":{"name":"write_file","arguments":"{\"pa"}}]}}]}
//   {"choices":[{"delta":{"tool_calls":[
//      {"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}]}}]}
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Delta,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Pulls complete `data:` payloads out of a rolling byte buffer.
///
/// Works on bytes rather than text because a network chunk can split a
/// multi-byte UTF-8 character; holding the partial line as bytes until its
/// newline arrives keeps such a character intact.
#[derive(Debug, Default)]
struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    fn push_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns every complete `data:` payload now available, leaving any
    /// trailing partial line buffered for the next call.
    fn drain_payloads(&mut self) -> Vec<String> {
        let mut payloads = Vec::new();
        while let Some(newline) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]);
            let line = line.trim_end_matches('\r');
            // Blank separators and any non-data field (`event:`, `id:`, `:`
            // comments) are not payloads.
            if let Some(rest) = line.strip_prefix("data:") {
                payloads.push(rest.trim().to_string());
            }
        }
        payloads
    }
}

/// Reassembles streamed chunks into one [`ChatMessage`].
#[derive(Debug, Default)]
struct StreamAccumulator {
    content: String,
    /// Keyed by the wire's `index` and ordered by it, so tool calls come out
    /// in the order the model asked for them regardless of chunk arrival.
    tool_calls: BTreeMap<u32, PartialToolCall>,
}

impl StreamAccumulator {
    /// Folds one `data:` payload in, returning any new text it carried.
    fn push_payload(&mut self, payload: &str) -> Result<Option<String>> {
        let chunk: StreamChunk =
            serde_json::from_str(payload).map_err(|e| anyhow!("Malformed stream chunk: {e}"))?;

        let mut new_text = None;
        for choice in chunk.choices {
            if let Some(content) = choice.delta.content {
                if !content.is_empty() {
                    self.content.push_str(&content);
                    new_text.get_or_insert_with(String::new).push_str(&content);
                }
            }

            for delta in choice.delta.tool_calls.unwrap_or_default() {
                let entry = self.tool_calls.entry(delta.index).or_default();
                if let Some(id) = delta.id {
                    entry.id = id;
                }
                if let Some(function) = delta.function {
                    if let Some(name) = function.name {
                        // OpenAI sends the name once, in the opening chunk,
                        // but plenty of compatible providers repeat it whole
                        // in every delta. Appending blindly turns that into
                        // "write_filewrite_file" and the call then fails as
                        // an unknown tool, so only extend on genuinely new
                        // text.
                        if entry.name.is_empty() {
                            entry.name = name;
                        } else if entry.name != name {
                            entry.name.push_str(&name);
                        }
                    }
                    if let Some(arguments) = function.arguments {
                        entry.arguments.push_str(&arguments);
                    }
                }
            }
        }

        Ok(new_text)
    }

    fn finish(self) -> ChatMessage {
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .into_values()
            .map(|partial| ToolCall {
                id: partial.id,
                function: FunctionCall {
                    name: partial.name,
                    arguments: partial.arguments,
                },
            })
            .collect();

        ChatMessage {
            role: "assistant".to_string(),
            // Empty text is reported as absent, matching how the buffered
            // path's providers return `content: null` on a tool-only turn.
            content: if self.content.is_empty() {
                None
            } else {
                Some(self.content)
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
        }
    }
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
            .ok_or_else(|| anyhow!("API key not configured. Run: comms login"))?;

        let http_client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;

        Ok(Client {
            config,
            api_key,
            http_client,
        })
    }

    /// Whether responses should stream, per the user's `comms stream`
    /// setting. Callers pick [`Client::chat_stream`] or [`Client::chat`]
    /// accordingly.
    pub fn streaming_enabled(&self) -> bool {
        self.config.stream
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

    /// Builds the request body shared by the buffered and streaming paths,
    /// including translating `effort_level` into whichever shape the
    /// configured provider expects.
    fn build_request(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        temperature: Option<f32>,
        tools: Option<Vec<serde_json::Value>>,
        effort_level: Option<String>,
        stream: bool,
    ) -> ChatRequest {
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

        ChatRequest {
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
            stream: if stream { Some(true) } else { None },
        }
    }

    pub async fn chat(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        temperature: Option<f32>,
        tools: Option<Vec<serde_json::Value>>,
        effort_level: Option<String>,
    ) -> Result<ChatResponse> {
        let request = self.build_request(model, messages, temperature, tools, effort_level, false);

        let req = self
            .http_client
            .post(format!("{}/chat/completions", self.config.base_url));
        let response = self
            .apply_headers(req)
            .json(&request)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(anyhow!("API error: {}", error_text));
        }

        let chat_response: ChatResponse = response.json().await?;
        Ok(chat_response)
    }

    /// The streaming counterpart to [`Client::chat`]: yields text as it
    /// arrives, then one [`StreamEvent::Done`] with the assembled message.
    ///
    /// Callers that need the whole reply before acting (tool calls, saving to
    /// history) use the `Done` message; callers rendering live use the
    /// `Content` deltas. The two never disagree — the deltas concatenate to
    /// the final message's content.
    pub fn chat_stream(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        temperature: Option<f32>,
        tools: Option<Vec<serde_json::Value>>,
        effort_level: Option<String>,
    ) -> impl Stream<Item = Result<StreamEvent>> + '_ {
        let request = self.build_request(model, messages, temperature, tools, effort_level, true);
        let url = format!("{}/chat/completions", self.config.base_url);

        try_stream! {
            let req = self.http_client.post(url);
            // Not `.timeout()` on the request itself — that would also
            // bound the total time spent reading a long-but-still-arriving
            // stream below, which is exactly what the idle timeout further
            // down is meant to allow. This only bounds how long a first
            // response takes to start showing up at all.
            let response = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, self.apply_headers(req).json(&request).send()).await {
                Ok(response) => response?,
                Err(_) => {
                    Err(anyhow!(
                        "No response from provider within {}s; the connection may have stalled",
                        STREAM_IDLE_TIMEOUT.as_secs()
                    ))?;
                    return;
                }
            };

            if !response.status().is_success() {
                let error_text = response.text().await?;
                Err(anyhow!("API error: {}", error_text))?;
                return;
            }

            let mut bytes = response.bytes_stream();
            let mut decoder = SseDecoder::default();
            let mut accumulator = StreamAccumulator::default();

            'outer: loop {
                let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, bytes.next()).await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break 'outer,
                    Err(_) => {
                        Err(anyhow!(
                            "No response from provider within {}s; the connection may have stalled",
                            STREAM_IDLE_TIMEOUT.as_secs()
                        ))?;
                        return;
                    }
                };
                decoder.push_bytes(&chunk?);
                for payload in decoder.drain_payloads() {
                    if payload == "[DONE]" {
                        break 'outer;
                    }
                    if payload.is_empty() {
                        continue;
                    }
                    if let Some(text) = accumulator.push_payload(&payload)? {
                        yield StreamEvent::Content(text);
                    }
                }
            }

            yield StreamEvent::Done(accumulator.finish());
        }
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

    // --- SSE framing ---------------------------------------------------

    #[test]
    fn decoder_returns_only_complete_data_lines() {
        let mut d = SseDecoder::default();
        d.push_bytes(b"data: one\ndata: two\ndata: par");
        assert_eq!(d.drain_payloads(), vec!["one", "two"]);
        // The partial third line stays buffered until its newline arrives.
        assert!(d.drain_payloads().is_empty());
        d.push_bytes(b"tial\n");
        assert_eq!(d.drain_payloads(), vec!["partial"]);
    }

    #[test]
    fn decoder_ignores_blank_lines_and_non_data_fields() {
        let mut d = SseDecoder::default();
        d.push_bytes(b"event: message\n: a comment\n\ndata: payload\n\n");
        assert_eq!(d.drain_payloads(), vec!["payload"]);
    }

    #[test]
    fn decoder_handles_crlf() {
        let mut d = SseDecoder::default();
        d.push_bytes(b"data: hello\r\n\r\n");
        assert_eq!(d.drain_payloads(), vec!["hello"]);
    }

    #[test]
    fn decoder_survives_utf8_split_across_chunks() {
        // "café" — the é is two bytes, split across the chunk boundary.
        let mut d = SseDecoder::default();
        d.push_bytes(b"data: caf\xc3");
        assert!(d.drain_payloads().is_empty());
        d.push_bytes(b"\xa9\n");
        assert_eq!(d.drain_payloads(), vec!["café"]);
    }

    // --- Chunk accumulation ---------------------------------------------

    fn content_chunk(text: &str) -> String {
        format!(
            r#"{{"choices":[{{"delta":{{"content":{}}}}}]}}"#,
            serde_json::to_string(text).unwrap()
        )
    }

    #[test]
    fn accumulates_content_deltas_in_order() {
        let mut acc = StreamAccumulator::default();
        assert_eq!(
            acc.push_payload(&content_chunk("Hello")).unwrap(),
            Some("Hello".to_string())
        );
        assert_eq!(
            acc.push_payload(&content_chunk(", world")).unwrap(),
            Some(", world".to_string())
        );
        let message = acc.finish();
        assert_eq!(message.content, Some("Hello, world".to_string()));
        assert!(message.tool_calls.is_none());
        assert_eq!(message.role, "assistant");
    }

    #[test]
    fn empty_content_delta_yields_no_text() {
        let mut acc = StreamAccumulator::default();
        // Providers commonly open a stream with a role-only or empty delta.
        assert_eq!(acc.push_payload(&content_chunk("")).unwrap(), None);
        assert_eq!(
            acc.push_payload(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#)
                .unwrap(),
            None
        );
        // No text at all is reported as absent, not as an empty string.
        assert_eq!(acc.finish().content, None);
    }

    #[test]
    fn reassembles_tool_call_arguments_fragmented_across_chunks() {
        let mut acc = StreamAccumulator::default();
        for payload in [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"write_file","arguments":"{\"filep"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ath\":\"a."}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"txt\"}"}}]}}]}"#,
        ] {
            assert_eq!(acc.push_payload(payload).unwrap(), None);
        }

        let message = acc.finish();
        assert_eq!(message.content, None);
        let calls = message.tool_calls.expect("tool call");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "write_file");
        // The concatenated fragments must be valid JSON, since this string is
        // what gets parsed to actually run the tool.
        assert_eq!(calls[0].function.arguments, r#"{"filepath":"a.txt"}"#);
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["filepath"], "a.txt");
    }

    #[test]
    fn a_repeated_function_name_is_not_concatenated() {
        // Providers that echo the whole name in every delta must not end up
        // with "write_filewrite_file", which would fail as an unknown tool.
        let mut acc = StreamAccumulator::default();
        for payload in [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write_file","arguments":"{\"a\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"write_file","arguments":"1}"}}]}}]}"#,
        ] {
            acc.push_payload(payload).unwrap();
        }
        let calls = acc.finish().tool_calls.expect("tool call");
        assert_eq!(calls[0].function.name, "write_file");
        assert_eq!(calls[0].function.arguments, r#"{"a":1}"#);
    }

    #[test]
    fn a_name_split_across_chunks_still_joins() {
        let mut acc = StreamAccumulator::default();
        for payload in [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write_"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"file"}}]}}]}"#,
        ] {
            acc.push_payload(payload).unwrap();
        }
        let calls = acc.finish().tool_calls.expect("tool call");
        assert_eq!(calls[0].function.name, "write_file");
    }

    #[test]
    fn orders_tool_calls_by_index_not_arrival() {
        let mut acc = StreamAccumulator::default();
        // Second call's fragment arrives before the first's is finished.
        acc.push_payload(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"second","arguments":"{}"}}]}}]}"#,
        )
        .unwrap();
        acc.push_payload(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"first","arguments":"{}"}}]}}]}"#,
        )
        .unwrap();

        let calls = acc.finish().tool_calls.expect("tool calls");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "first");
        assert_eq!(calls[1].function.name, "second");
    }

    #[test]
    fn content_and_tool_calls_can_arrive_in_one_turn() {
        let mut acc = StreamAccumulator::default();
        acc.push_payload(&content_chunk("Let me check.")).unwrap();
        acc.push_payload(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"read_file","arguments":"{}"}}]}}]}"#,
        )
        .unwrap();

        let message = acc.finish();
        assert_eq!(message.content, Some("Let me check.".to_string()));
        assert_eq!(message.tool_calls.unwrap().len(), 1);
    }

    #[test]
    fn malformed_chunk_is_an_error_not_a_panic() {
        let mut acc = StreamAccumulator::default();
        assert!(acc.push_payload("{not json").is_err());
    }

    #[test]
    fn chunk_without_choices_is_tolerated() {
        // Some providers emit keepalive/usage-only frames.
        let mut acc = StreamAccumulator::default();
        assert_eq!(
            acc.push_payload(r#"{"usage":{"total_tokens":5}}"#).unwrap(),
            None
        );
        assert_eq!(acc.finish().content, None);
    }

    #[test]
    fn has_visible_content_treats_none_and_blank_string_alike() {
        let none = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
        };
        let blank = ChatMessage {
            content: Some("   ".to_string()),
            ..none.clone()
        };
        let real = ChatMessage {
            content: Some("hi".to_string()),
            ..none.clone()
        };
        assert!(!none.has_visible_content());
        assert!(!blank.has_visible_content());
        assert!(real.has_visible_content());
    }
}
