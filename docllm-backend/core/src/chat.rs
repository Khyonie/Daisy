use std::{collections::BTreeMap, pin::Pin};

use async_stream::try_stream;
use futures_util::{Stream, StreamExt};
use lancedb::Table;
use tokio::sync::mpsc;

use crate::{
    database::{
        self,
        providers::{RetrievalType, retrieve},
        retrieval::SearchHit,
    },
    llm::{
        message::{ChatMessage, RoleType},
        ollama::{OllamaClient, SYSTEM_PROMPT},
    },
    query::{UserQuery, decompose_query},
};

const RESULT_LIMIT: usize = 3;
const NO_INGESTED_DOCUMENTS: &str =
    "No documents have been ingested. Upload and ingest documents before asking a question.";

pub enum ChatEvent {
    Status(&'static str),
    Content(String),
}

pub type AnswerStream = Pin<Box<dyn Stream<Item = Result<ChatEvent, String>> + Send>>;

#[derive(Default)]
pub struct ChatService;

impl ChatService {
    pub fn stream_answer(
        &self,
        model: &str,
        rag_provider: RetrievalType,
        query: &str,
    ) -> AnswerStream {
        let model = model.to_owned();
        let query = query.to_owned();
        let stream = try_stream! {
            yield ChatEvent::Status("Opening document database...");

            // Poll preparation and its progress messages together. Dropping the
            // response stream also cancels preparation, with no detached task.
            let (sender, mut progress) = mpsc::unbounded_channel();
            let report = |message| { let _ = sender.send(message); };
            let context = Self::context_for_query(&model, rag_provider, &query, &report);
            futures_util::pin_mut!(context);
            let context = loop {
                tokio::select! {
                    biased;
                    Some(message) = progress.recv() => yield ChatEvent::Status(message),
                    result = &mut context => break result,
                }
            }?;

            let mut answer = generate_answer(OllamaClient::new(&model)?, context);
            while let Some(event) = answer.next().await {
                yield event?;
            }
        };

        Box::pin(stream)
    }

    pub async fn available_models(&self) -> Result<Vec<String>, String> {
        OllamaClient::new("")?.available_models().await
    }

    async fn context_for_query(
        model: &str,
        rag_provider: RetrievalType,
        query: &str,
        report: &(dyn Fn(&'static str) + Send + Sync),
    ) -> Result<ChatContext, String> {
        let table = require_ingested_documents(database::open_database().await).await?;

        //let sources = retrieve(rag_provider, &table, query, model, RESULT_LIMIT, report)
        //    .await
        //    .map_err(|error| format!("Failed to retrieve sources: {error}"))?;

        // Build sources from queries
        let mut sources = Vec::new();
        let queries = decompose_query(query, model).await?;
        for q in queries {
            let mut query_sources = Self::retrieve_context_from_decomposed_query(
                q,
                model,
                rag_provider,
                &table,
                report,
            )
            .await?;
            sources.append(&mut query_sources);
        }

        let mut source_numbers = BTreeMap::new();
        let mut source_labels = Vec::new();
        let mut prompt = String::from("<documents>\n");
        for source in sources {
            let source_number = source_number(
                &mut source_numbers,
                &mut source_labels,
                source.source.clone(),
            );

            prompt.push_str("<excerpt>\n");
            prompt.push_str(&format!("<source_number>{source_number}</source_number>\n"));
            prompt.push_str(&format!("<document>{}</document>\n", source.source));
            prompt.push_str(&format!("<chunk>{}</chunk>\n", source.chunk_index));
            prompt.push_str("<content>\n");
            prompt.push_str(&source.content);
            prompt.push_str("\n</content>\n");
            prompt.push_str("</excerpt>\n");
        }
        prompt.push_str("</documents>\n");
        prompt.push_str("<available_sources>\n");
        for (index, source_label) in source_labels.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", index + 1, source_label));
        }
        prompt.push_str("</available_sources>\n");
        prompt.push_str(&format!("<question>\n{query}\n</question>\n\n"));
        prompt.push_str("Answer the question using the document excerpts above. Cite document-supported claims with numbered superscripts like <sup>1</sup>, matching the source numbers in <available_sources>. Use a citation only when the claim is supported by that source. Do not include footnotes or a Sources section.");

        println!("{}", prompt);

        Ok(ChatContext {
            messages: vec![
                ChatMessage::new(RoleType::System, SYSTEM_PROMPT.to_owned()),
                ChatMessage::new(RoleType::User, prompt),
            ],
            sources_section: format_sources_section(&source_labels),
        })
    }

    async fn retrieve_context_from_decomposed_query(
        query: UserQuery,
        model: &str,
        rag_provider: RetrievalType,
        table: &Table,
        report: &(dyn Fn(&'static str) + Send + Sync),
    ) -> Result<Vec<SearchHit>, String> {
        let prompt = format!("{} {}", query.query, query.context); // TODO This probably
        // isn't the best way to
        // do this
        retrieve(rag_provider, table, &prompt, model, RESULT_LIMIT, report).await
    }
}

struct ChatContext {
    messages: Vec<ChatMessage>,
    sources_section: String,
}

// Run before decomposition or retrieval so an empty index never triggers an LLM call.
async fn require_ingested_documents(table: Result<Table, lancedb::Error>) -> Result<Table, String> {
    let table = match table {
        Ok(table) => table,
        Err(lancedb::Error::TableNotFound { .. }) => return Err(NO_INGESTED_DOCUMENTS.to_owned()),
        Err(error) => return Err(format!("Failed to open database table: {error}")),
    };
    let count = table
        .count_rows(None)
        .await
        .map_err(|error| format!("Failed to check ingested documents: {error}"))?;
    if count == 0 {
        return Err(NO_INGESTED_DOCUMENTS.to_owned());
    }
    Ok(table)
}

fn generate_answer(client: OllamaClient, mut context: ChatContext) -> AnswerStream {
    Box::pin(try_stream! {
        for attempt in 0..2 {
            yield ChatEvent::Status(if attempt == 0 {
                "Preparing answer..."
            } else {
                "No answer received. Retrying..."
            });

            // Keep the retrieved context for one retry; do not rerun the RAG pipeline.
            let mut answer = client.stream_chat(context.messages.clone()).await?;
            let mut answer_text = String::new();
            let mut has_content = false;
            while let Some(fragment) = answer.next().await {
                let fragment = fragment?;
                answer_text.push_str(&fragment);
                if has_content {
                    yield ChatEvent::Content(fragment);
                } else if !answer_text.trim().is_empty() {
                    has_content = true;
                    yield ChatEvent::Content(answer_text.clone());
                }
            }

            if has_content {
                if should_include_sources(&answer_text) && !context.sources_section.is_empty() {
                    yield ChatEvent::Content(context.sources_section);
                }
                return;
            }

            if attempt == 0 {
                context.messages.push(ChatMessage::new(RoleType::User, String::from(
                    "Your previous attempt returned no answer text. Please provide a concise final answer to the question using the supplied excerpts. If they are insufficient, explicitly say so without citations."
                )));
            }
        }
        Err("The model returned no answer after two attempts. Please try again or select a different model.".to_owned())?;
    })
}

fn format_sources_section(sources: &[String]) -> String {
    if sources.is_empty() {
        return String::new();
    }

    let mut section = String::from("\n\n**Sources**\n");
    for (index, source) in sources.iter().enumerate() {
        section.push_str(&format!("{}. ", index + 1));
        section.push_str(source);
        section.push('\n');
    }

    section
}

fn source_number(
    source_numbers: &mut BTreeMap<String, usize>,
    source_labels: &mut Vec<String>,
    source: String,
) -> usize {
    if let Some(number) = source_numbers.get(&source) {
        return *number;
    }

    let number = source_labels.len() + 1;
    source_labels.push(source.clone());
    source_numbers.insert(source, number);

    number
}

fn should_include_sources(answer: &str) -> bool {
    if answer.trim().is_empty() {
        return false;
    }
    let normalized = answer.to_ascii_lowercase();
    let cannot_answer_phrases = [
        "available documents cannot",
        "cannot sufficiently answer",
        "can't sufficiently answer",
        "do not contain enough",
        "don't contain enough",
        "does not contain enough",
        "not enough information",
        "insufficient information",
        "cannot answer",
        "can't answer",
        "unable to answer",
        "not provided",
        "not covered",
    ];

    !cannot_answer_phrases
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{Int32Array, RecordBatch};
    use futures_util::StreamExt;
    use serde_json::json;

    use super::{
        ChatContext, ChatEvent, ChatMessage, ChatService, NO_INGESTED_DOCUMENTS, RetrievalType,
        RoleType, generate_answer, require_ingested_documents, should_include_sources,
    };
    use crate::llm::ollama::tests::mock_ollama;

    #[tokio::test]
    async fn requires_documents_in_missing_empty_and_cleared_tables() {
        let directory = tempfile::tempdir().unwrap();
        let database = lancedb::connect(directory.path().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let missing = database.open_table("document_chunks").execute().await;
        assert_eq!(
            require_ingested_documents(missing).await.unwrap_err(),
            NO_INGESTED_DOCUMENTS
        );

        let batch = RecordBatch::try_from_iter([(
            "id",
            Arc::new(Int32Array::from(vec![1])) as arrow_array::ArrayRef,
        )])
        .unwrap();
        let table = database
            .create_empty_table("document_chunks", batch.schema())
            .execute()
            .await
            .unwrap();
        assert_eq!(
            require_ingested_documents(Ok(table.clone()))
                .await
                .unwrap_err(),
            NO_INGESTED_DOCUMENTS
        );

        table.add(batch).execute().await.unwrap();
        assert!(require_ingested_documents(Ok(table)).await.is_ok());

        database.drop_all_tables(&[]).await.unwrap();
        let cleared = database.open_table("document_chunks").execute().await;
        assert_eq!(
            require_ingested_documents(cleared).await.unwrap_err(),
            NO_INGESTED_DOCUMENTS
        );
    }

    #[tokio::test]
    async fn does_not_disguise_database_failures_as_an_empty_index() {
        let error = lancedb::Error::InvalidInput {
            message: "Invalid database location".to_owned(),
        };
        let message = require_ingested_documents(Err(error)).await.unwrap_err();
        assert!(message.starts_with("Failed to open database table:"));
        assert!(message.contains("Invalid database location"));
    }

    fn context() -> ChatContext {
        ChatContext {
            messages: vec![ChatMessage::new(
                RoleType::User,
                "Question and retrieved excerpts".to_owned(),
            )],
            sources_section: "\n\n**Sources**\n1. document.md\n".to_owned(),
        }
    }

    fn completed(content: &str) -> String {
        json!({"message": {"role": "assistant", "content": content}, "done": true, "done_reason": "stop"}).to_string()
    }

    #[tokio::test]
    async fn retries_empty_whitespace_and_thinking_only_answers() {
        for empty in [
            completed(""),
            completed(" \n\t"),
            json!({"message": {"role": "assistant", "content": "", "thinking": "Internal reasoning"}, "done": true}).to_string(),
        ] {
            let (client, server) = mock_ollama(vec![empty, completed("Supported answer.")]).await;
            let events: Vec<_> = generate_answer(client, context()).collect().await;
            let requests = server.await.unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0]["messages"][0], requests[1]["messages"][0]);
            assert_eq!(requests[1]["stream"], true);
            assert!(events.iter().all(Result::is_ok));
            assert!(events.iter().any(|event| matches!(event, Ok(ChatEvent::Status("No answer received. Retrying...")))));
            let content: String = events.into_iter().filter_map(|event| match event {
                Ok(ChatEvent::Content(content)) => Some(content),
                _ => None,
            }).collect();
            assert_eq!(content, "Supported answer.\n\n**Sources**\n1. document.md\n");
        }
    }

    #[tokio::test]
    async fn two_empty_attempts_fail_without_content_or_sources() {
        let (client, server) = mock_ollama(vec![completed(""), completed(" \n")]).await;
        let events: Vec<_> = generate_answer(client, context()).collect().await;
        assert_eq!(server.await.unwrap().len(), 2);
        assert!(matches!(events.last(), Some(Err(error)) if error.contains("after two attempts")));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(ChatEvent::Content(_))))
        );
    }

    #[tokio::test]
    async fn insufficient_answer_is_not_retried_or_given_sources() {
        let answer = "The available documents cannot sufficiently answer this question.";
        let (client, server) = mock_ollama(vec![completed(answer)]).await;
        let events: Vec<_> = generate_answer(client, context()).collect().await;
        assert_eq!(server.await.unwrap().len(), 1);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], Ok(ChatEvent::Content(content)) if content == answer));
    }

    #[tokio::test]
    async fn interrupted_answer_is_not_retried_or_given_sources() {
        let partial =
            json!({"message": {"role": "assistant", "content": "Partial answer"}, "done": false});
        let (client, server) = mock_ollama(vec![partial.to_string()]).await;
        let events: Vec<_> = generate_answer(client, context()).collect().await;
        assert_eq!(server.await.unwrap().len(), 1);
        assert_eq!(events.len(), 3);
        assert!(
            matches!(&events[1], Ok(ChatEvent::Content(content)) if content == "Partial answer")
        );
        assert!(matches!(&events[2], Err(error) if error.contains("before confirming")));
    }

    #[tokio::test]
    async fn reports_progress_before_database_or_model_access() {
        let mut answer =
            ChatService.stream_answer("unused-model", RetrievalType::Basic, "Question");
        assert!(matches!(
            answer.next().await,
            Some(Ok(ChatEvent::Status("Opening document database...")))
        ));
        // Cancelling at this point must not start retrieval in a background task.
        drop(answer);
    }

    #[test]
    fn includes_sources_for_supported_answer() {
        assert!(should_include_sources(
            "The project supports PDF and Markdown ingestion."
        ));
    }

    #[test]
    fn skips_sources_for_insufficient_answer() {
        assert!(!should_include_sources(
            "The available documents cannot sufficiently answer this question."
        ));
    }
}
