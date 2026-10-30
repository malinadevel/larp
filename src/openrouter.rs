use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::{header, Client};
use serde_json::{json, Value};

use crate::messages::{AssistantTurn, FunctionCall, Message, ToolCall};

pub struct OpenRouterClient {
    client: Client,
    api_key: String,
    model: String,
    endpoint: String,
}

impl OpenRouterClient {
    pub fn new(api_key: String, model: String, endpoint: String) -> Result<Self> {
        let client = Client::builder()
            .build()
            .context("could not create HTTP client")?;

        Ok(Self {
            client,
            api_key,
            model,
            endpoint,
        })
    }

    pub async fn complete<F>(
        &self,
        messages: &[Message],
        tools: &[Value],
        on_text: &mut F,
    ) -> Result<AssistantTurn>
    where
        F: FnMut(&str),
    {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({
                "model": self.model,
                "stream": true,
                "parallel_tool_calls": false,
                "tool_choice": "auto",
                "messages": messages,
                "tools": tools,
            }))
            .send()
            .await
            .context("request to OpenRouter failed")?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("could not read error response: {error}"));
            bail!("OpenRouter returned HTTP {status}: {body}");
        }

        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut turn = AssistantTurn::default();
        let mut partial_tool_calls = BTreeMap::<usize, PartialToolCall>::new();

        'chunks: while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("error while reading OpenRouter stream")?;
            for event in decoder.push(&chunk)? {
                if event == "[DONE]" {
                    break 'chunks;
                }
                process_event(&event, &mut turn, &mut partial_tool_calls, on_text)?;
            }
        }

        for event in decoder.finish()? {
            if event == "[DONE]" {
                break;
            }
            process_event(&event, &mut turn, &mut partial_tool_calls, on_text)?;
        }

        turn.tool_calls = finish_tool_calls(partial_tool_calls)?;
        Ok(turn)
    }
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn process_event<F>(
    event: &str,
    turn: &mut AssistantTurn,
    tool_calls: &mut BTreeMap<usize, PartialToolCall>,
    on_text: &mut F,
) -> Result<()>
where
    F: FnMut(&str),
{
    let value: Value = serde_json::from_str(event).context("invalid JSON in OpenRouter stream")?;

    if let Some(error) = value.get("error") {
        bail!("OpenRouter stream error: {error}");
    }

    let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return Ok(());
    };

    let Some(delta) = choice.get("delta") else {
        return Ok(());
    };

    if let Some(text) = delta.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            on_text(text);
            turn.content.get_or_insert_with(String::new).push_str(text);
        }
    }

    if let Some(deltas) = delta.get("tool_calls").and_then(Value::as_array) {
        for (position, tool_delta) in deltas.iter().enumerate() {
            let index = tool_delta
                .get("index")
                .and_then(Value::as_u64)
                .map_or(position, |index| index as usize);
            let partial = tool_calls.entry(index).or_default();

            if let Some(id) = tool_delta.get("id").and_then(Value::as_str) {
                partial.id.push_str(id);
            }

            if let Some(function) = tool_delta.get("function") {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    partial.name.push_str(name);
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    partial.arguments.push_str(arguments);
                }
            }
        }
    }

    Ok(())
}

fn finish_tool_calls(partials: BTreeMap<usize, PartialToolCall>) -> Result<Vec<ToolCall>> {
    partials
        .into_values()
        .map(|partial| {
            if partial.id.is_empty() || partial.name.is_empty() {
                bail!("OpenRouter returned an incomplete tool call");
            }

            Ok(ToolCall {
                id: partial.id,
                call_type: "function".to_owned(),
                function: FunctionCall {
                    name: partial.name,
                    arguments: partial.arguments,
                },
            })
        })
        .collect()
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<String>,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();

        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }

            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }

        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<String>> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }
        if !self.data.is_empty() {
            events.push(std::mem::take(&mut self.data).join("\n"));
        }
        Ok(events)
    }

    fn process_line(&mut self, line: &[u8]) -> Result<Option<String>> {
        if line.is_empty() {
            if self.data.is_empty() {
                return Ok(None);
            }
            return Ok(Some(std::mem::take(&mut self.data).join("\n")));
        }

        if line[0] == b':' {
            return Ok(None);
        }

        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            return Ok(None);
        };
        if &line[..colon] != b"data" {
            return Ok(None);
        }

        let mut value = &line[colon + 1..];
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        self.data
            .push(String::from_utf8(value.to_vec()).context("SSE data was not valid UTF-8")?);
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::{process_event, PartialToolCall, SseDecoder};
    use crate::messages::AssistantTurn;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn sse_decoder_handles_split_chunks_and_comments() {
        let mut decoder = SseDecoder::default();

        assert!(decoder.push(b": keepalive\nda").unwrap().is_empty());
        assert_eq!(
            decoder.push(b"ta: {\"answer\":\"ok\"}\r\n\r\n").unwrap(),
            vec![r#"{"answer":"ok"}"#]
        );
        assert!(decoder.finish().unwrap().is_empty());
    }

    #[test]
    fn tool_call_arguments_are_accumulated_from_deltas() {
        let mut turn = AssistantTurn::default();
        let mut calls = BTreeMap::<usize, PartialToolCall>::new();
        let mut output = String::new();

        for event in [
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":"{\"command\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"pwd\"}"}}]}}]}),
        ] {
            process_event(&event.to_string(), &mut turn, &mut calls, &mut |text| {
                output.push_str(text)
            })
            .unwrap();
        }

        let call = calls.get(&0).unwrap();
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "bash");
        assert_eq!(call.arguments, r#"{"command":"pwd"}"#);
    }
}
