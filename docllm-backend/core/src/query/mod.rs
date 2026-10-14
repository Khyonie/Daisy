use schemars::JsonSchema;
use serde::Deserialize;

use crate::llm::{
    message::{ChatMessage, RoleType},
    ollama::OllamaClient,
};

#[derive(Deserialize, JsonSchema)]
pub struct UserQuery {
    pub query: String,
    pub context: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct UserQueryResponse {
    questions: Vec<UserQuery>,
}

const SYSTEM_PROMPT: &str = include_str!("../../../../prompts/DECOMPOSE_SYSTEM_PROMPT.md");

pub async fn decompose_query(query: &str, model: &str) -> Result<Vec<UserQuery>, String> {
    let system_message = ChatMessage::new(RoleType::System, String::from(SYSTEM_PROMPT));
    let prompt = ChatMessage::new(RoleType::User, String::from(query));

    let decomposition_model = OllamaClient::new(model)
        .map_err(|error| format!("Failed to create decomposition model: {error}"))?;

    let response: UserQueryResponse = decomposition_model
        .send_structured_chat(vec![system_message, prompt], 0.5, Some(true))
        .await?;

    Ok(response.questions)
}
