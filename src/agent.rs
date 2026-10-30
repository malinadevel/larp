use anyhow::{bail, Result};

use crate::{messages::Message, openrouter::OpenRouterClient, tools::ToolRegistry};

pub struct Agent {
    client: OpenRouterClient,
    tools: ToolRegistry,
    max_tool_rounds: usize,
}

impl Agent {
    pub fn new(client: OpenRouterClient, tools: ToolRegistry, max_tool_rounds: usize) -> Self {
        Self {
            client,
            tools,
            max_tool_rounds,
        }
    }

    pub async fn run<F>(
        &self,
        messages: &mut Vec<Message>,
        system_prompt: &str,
        prompt: &str,
        mut on_text: F,
    ) -> Result<()>
    where
        F: FnMut(&str),
    {
        messages.push(Message::user(prompt));
        let mut tool_rounds = 0;

        loop {
            let mut request_messages = Vec::with_capacity(messages.len() + 1);
            request_messages.push(Message::system(system_prompt));
            request_messages.extend(messages.iter().cloned());

            let turn = self
                .client
                .complete(&request_messages, &self.tools.definitions(), &mut on_text)
                .await?;

            if turn.tool_calls.is_empty() {
                messages.push(Message::assistant_text(turn.content));
                return Ok(());
            }

            if tool_rounds >= self.max_tool_rounds {
                bail!("agent stopped after {} tool rounds", self.max_tool_rounds);
            }

            on_text("\n");
            messages.push(Message::assistant(turn.content, turn.tool_calls.clone()));

            for call in &turn.tool_calls {
                let result = self.tools.execute(call).await;
                messages.push(Message::tool(&call.id, result));
            }

            tool_rounds += 1;
        }
    }
}
