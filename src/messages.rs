use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn assistant_text(content: Option<String>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Default)]
pub struct AssistantTurn {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{FunctionCall, Message, ToolCall};

    #[test]
    fn tool_result_uses_openai_compatible_message_shape() {
        let message = Message::tool("call_123", "command output");
        let value = serde_json::to_value(message).unwrap();

        assert_eq!(
            value,
            json!({
                "role": "tool",
                "content": "command output",
                "tool_call_id": "call_123"
            })
        );
    }

    #[test]
    fn assistant_tool_call_serializes_with_null_content() {
        let message = Message::assistant(
            None,
            vec![ToolCall {
                id: "call_123".to_owned(),
                call_type: "function".to_owned(),
                function: FunctionCall {
                    name: "bash".to_owned(),
                    arguments: r#"{"command":"pwd"}"#.to_owned(),
                },
            }],
        );
        let value = serde_json::to_value(message).unwrap();

        assert_eq!(value["role"], "assistant");
        assert!(value["content"].is_null());
        assert_eq!(value["tool_calls"][0]["function"]["name"], "bash");
    }

    #[test]
    fn system_message_serializes_with_system_role() {
        let value = serde_json::to_value(Message::system("Be helpful.")).unwrap();

        assert_eq!(
            value,
            json!({
                "role": "system",
                "content": "Be helpful."
            })
        );
    }
}
