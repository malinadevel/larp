mod agent;
mod agents;
mod config;
mod messages;
mod openrouter;
mod security;
mod storage;
mod templates;
mod tools;

use std::io::{self, Write};

use anyhow::{bail, Context, Result};

use crate::{
    agent::Agent, agents::AgentCatalog, config::Config, openrouter::OpenRouterClient,
    security::SecurityPolicy, storage::Storage, tools::ToolRegistry,
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    match parse_args(std::env::args().skip(1))? {
        Command::Help => {
            print_help();
            Ok(())
        }
        command => {
            let mut catalog = AgentCatalog::initialize()?;
            match command {
                Command::ListAgents => {
                    for agent in catalog.agents() {
                        let marker = if agent.id == catalog.default_agent_id() {
                            "*"
                        } else {
                            " "
                        };
                        println!("{marker} {} — {}", agent.id, agent.name);
                    }
                    Ok(())
                }
                Command::CreateAgent { id } => {
                    catalog.create_agent(&id)?;
                    println!("Created agent {id:?}.");
                    Ok(())
                }
                Command::SetCurrentAgent { id } => {
                    catalog.set_default_agent(&id)?;
                    println!("Current agent is now {id:?}.");
                    Ok(())
                }
                Command::Run {
                    agent_id,
                    clear_history,
                    prompt,
                } => {
                    let selected_agent = agent_id
                        .as_deref()
                        .unwrap_or_else(|| catalog.default_agent_id())
                        .to_owned();
                    run_agent(&catalog, &selected_agent, clear_history, &prompt).await
                }
                Command::Help => unreachable!("help is handled before initializing the catalog"),
            }
        }
    }
}

async fn run_agent(
    catalog: &AgentCatalog,
    agent_id: &str,
    clear_history: bool,
    prompt: &str,
) -> Result<()> {
    if prompt.trim().is_empty() && !clear_history {
        bail!("Usage: larp [--agent <id>] [-c|--clear] \"your prompt\"");
    }

    let storage = Storage::for_agent(catalog, agent_id)?;
    if clear_history {
        storage.clear_history()?;
    }
    if prompt.trim().is_empty() {
        println!(
            "Conversation history cleared for agent {:?}.",
            storage.agent_id()
        );
        return Ok(());
    }

    let config = Config::load(storage.config_path())?;
    let system_prompt = storage.load_system_prompt(config.prompt_mode)?;
    let mut messages = storage.load_history()?;
    let security = SecurityPolicy::load(storage.security_path())?;
    let client = OpenRouterClient::new(config.api_key, config.model, config.endpoint)?;
    let tools = ToolRegistry::load(storage.tools_dir(), security)?;
    let agent = Agent::new(client, tools, config.max_tool_rounds);

    let agent_result = agent
        .run(&mut messages, &system_prompt, prompt, |text| {
            print!("{text}");
            let _ = io::stdout().flush();
        })
        .await;

    storage
        .save_history(&messages)
        .context("could not save conversation history")?;
    agent_result.context("agent run failed")?;

    println!();
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run {
        agent_id: Option<String>,
        clear_history: bool,
        prompt: String,
    },
    ListAgents,
    CreateAgent {
        id: String,
    },
    SetCurrentAgent {
        id: String,
    },
    Help,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command> {
    let args = args.into_iter().collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "set") {
        return match args.as_slice() {
            [_, id] => Ok(Command::SetCurrentAgent { id: id.clone() }),
            _ => bail!("Usage: larp set <agent-id>"),
        };
    }
    if args.first().is_some_and(|arg| arg == "agents") {
        return parse_agent_command(&args[1..]);
    }

    let mut clear_history = false;
    let mut parsing_options = true;
    let mut agent_id = None;
    let mut prompt_parts = Vec::new();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        if parsing_options && arg == "--" {
            parsing_options = false;
        } else if parsing_options && matches!(arg.as_str(), "-h" | "--help") {
            return Ok(Command::Help);
        } else if parsing_options && matches!(arg.as_str(), "-c" | "--clear") {
            clear_history = true;
        } else if parsing_options && matches!(arg.as_str(), "-a" | "--agent") {
            let value = args.next().context("--agent requires an agent ID")?;
            if agent_id.replace(value).is_some() {
                bail!("--agent may only be specified once");
            }
        } else if parsing_options && arg.starts_with("--agent=") {
            let value = arg["--agent=".len()..].to_owned();
            if value.is_empty() {
                bail!("--agent requires a non-empty agent ID");
            }
            if agent_id.replace(value).is_some() {
                bail!("--agent may only be specified once");
            }
        } else if parsing_options && arg.starts_with('-') {
            bail!("unknown option: {arg}");
        } else {
            prompt_parts.push(arg);
        }
    }

    Ok(Command::Run {
        agent_id,
        clear_history,
        prompt: prompt_parts.join(" "),
    })
}

fn parse_agent_command(args: &[String]) -> Result<Command> {
    match args {
        [] => Ok(Command::ListAgents),
        [command] if command == "list" => Ok(Command::ListAgents),
        [command, id] if command == "create" => Ok(Command::CreateAgent { id: id.clone() }),
        [command, id] if command == "set-default" => {
            Ok(Command::SetCurrentAgent { id: id.clone() })
        }
        [command, ..] => bail!("unknown or invalid agents command: {command}"),
    }
}

fn print_help() {
    println!(
        "Usage:\n  larp [--agent <id>] [-c|--clear] <prompt>\n  larp agents\n  larp agents create <id>\n  larp set <id>\n\nCommands:\n  larp agents       List agents and their IDs; mark the current agent\n  larp set <id>     Set the current agent used by default\n  larp agents create <id>  Create an agent\n\nOptions:\n  -a, --agent <id>  Select an agent for this run only\n  -c, --clear       Clear this agent's conversation history\n  -h, --help        Show this help"
    );
}

#[cfg(test)]
mod tests {
    use super::{parse_args, Command};

    #[test]
    fn clear_option_can_be_used_without_a_prompt() {
        assert_eq!(
            parse_args(vec!["-c".to_owned()]).unwrap(),
            Command::Run {
                agent_id: None,
                clear_history: true,
                prompt: String::new()
            }
        );
        assert_eq!(
            parse_args(vec!["--clear".to_owned()]).unwrap(),
            Command::Run {
                agent_id: None,
                clear_history: true,
                prompt: String::new()
            }
        );
    }

    #[test]
    fn clear_option_is_removed_from_the_prompt() {
        assert_eq!(
            parse_args(vec![
                "--clear".to_owned(),
                "show".to_owned(),
                "files".to_owned()
            ])
            .unwrap(),
            Command::Run {
                agent_id: None,
                clear_history: true,
                prompt: "show files".to_owned()
            }
        );
    }

    #[test]
    fn agent_can_be_selected_before_prompt_and_clear() {
        assert_eq!(
            parse_args(vec![
                "--agent".to_owned(),
                "reviewer".to_owned(),
                "--clear".to_owned(),
                "review this".to_owned()
            ])
            .unwrap(),
            Command::Run {
                agent_id: Some("reviewer".to_owned()),
                clear_history: true,
                prompt: "review this".to_owned()
            }
        );
    }

    #[test]
    fn agent_management_commands_are_parsed() {
        assert_eq!(
            parse_args(vec!["agents".to_owned()]).unwrap(),
            Command::ListAgents
        );
        assert_eq!(
            parse_args(vec!["set".to_owned(), "reviewer".to_owned()]).unwrap(),
            Command::SetCurrentAgent {
                id: "reviewer".to_owned()
            }
        );
        assert_eq!(
            parse_args(vec![
                "agents".to_owned(),
                "create".to_owned(),
                "reviewer".to_owned()
            ])
            .unwrap(),
            Command::CreateAgent {
                id: "reviewer".to_owned()
            }
        );
    }
}
