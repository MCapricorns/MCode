//! The `ask_user` tool: structured agent-to-user questions.
//!
//! The tool serializes 1..=4 typed questions through the host-supplied
//! [`AskChannel`], awaits the user's answers cancel-safely, and returns them
//! to the model as the tool result. The channel seam keeps the tool free of
//! any UI dependency; hosts answer, tests replay.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// Maximum questions per ask.
pub const MAX_QUESTIONS: usize = 4;
/// Maximum characters accepted per answer.
pub const MAX_ANSWER_CHARS: usize = 4 * 1024;

/// One question the agent asks the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AskQuestion {
    /// Question headline.
    pub question: String,
    /// Optional candidate answers; free text is always allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Whether the user may skip this question.
    #[serde(default)]
    pub optional: bool,
}

/// One answered question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskAnswer {
    /// The question headline this answer belongs to.
    pub question: String,
    /// The chosen or typed answer; empty means skipped.
    pub answer: String,
}

/// Host side of the ask interaction.
#[async_trait]
pub trait AskChannel: Send + Sync + 'static {
    /// Presents the questions and waits for the user.
    ///
    /// Implementations must honor `cancel`: firing it resolves the wait
    /// without leaking the pending interaction.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Execution`] when the user dismissed the ask or
    /// the channel failed.
    async fn ask(
        &self,
        questions: &[AskQuestion],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<AskAnswer>, ToolError>;
}

/// The built-in `ask_user` tool.
pub struct AskTool {
    channel: Arc<dyn AskChannel>,
}

impl AskTool {
    /// Binds one answering channel.
    pub fn new(channel: Arc<dyn AskChannel>) -> Self {
        Self { channel }
    }
}

/// Wire shape of the tool arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AskArgs {
    /// 1..=4 questions for the user.
    pub questions: Vec<AskQuestion>,
}

#[async_trait]
impl Tool for AskTool {
    type Args = AskArgs;
    type Output = ();

    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user 1-4 clarifying questions and wait for their answers. \
         Use when a decision needs human input before proceeding."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "ask_user: batch every clarification into one call; the turn pauses \
             until the user answers.",
        )
    }

    fn concurrency(&self) -> crate::tool::Concurrency {
        crate::tool::Concurrency::Exclusive
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        if args.questions.is_empty() || args.questions.len() > MAX_QUESTIONS {
            return Err(ToolError::InvalidArgs(format!(
                "1..={MAX_QUESTIONS} questions required"
            )));
        }
        for question in &args.questions {
            if question.question.trim().is_empty() {
                return Err(ToolError::InvalidArgs("question text is required".into()));
            }
        }
        let answers = self.channel.ask(&args.questions, &ctx.cancel).await?;
        let mut rendered = String::new();
        for (index, answer) in answers.iter().enumerate() {
            if index > 0 {
                rendered.push('\n');
            }
            if answer.answer.is_empty() {
                rendered.push_str(&format!("{}. (skipped)", index + 1));
            } else {
                let bounded: String = answer.answer.chars().take(MAX_ANSWER_CHARS).collect();
                rendered.push_str(&format!("{}. {}", index + 1, bounded));
            }
        }
        Ok(ToolResult::text(rendered))
    }
}

/// Fails the ask when the user dismisses it.
pub fn user_dismissed() -> ToolError {
    ToolError::Execution("the user dismissed the questions".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    struct MockChannel {
        script: Mutex<Vec<Result<Vec<AskAnswer>, ToolError>>>,
    }

    #[async_trait]
    impl AskChannel for MockChannel {
        async fn ask(
            &self,
            questions: &[AskQuestion],
            _cancel: &CancellationToken,
        ) -> Result<Vec<AskAnswer>, ToolError> {
            let mut script = self.script.lock().expect("script");
            assert!(!questions.is_empty());
            script.remove(0)
        }
    }

    fn tool(script: Vec<Result<Vec<AskAnswer>, ToolError>>) -> AskTool {
        AskTool::new(Arc::new(MockChannel {
            script: Mutex::new(script),
        }))
    }

    fn ctx() -> ToolCtx {
        ToolCtx::new(".")
    }

    fn stream() -> ToolStream {
        ToolStream::channel().0
    }

    fn question(text: &str) -> AskQuestion {
        AskQuestion {
            question: text.to_owned(),
            choices: vec!["a".to_owned(), "b".to_owned()],
            optional: false,
        }
    }

    #[tokio::test]
    async fn ask_renders_answers_in_order() {
        let ask = tool(vec![Ok(vec![
            AskAnswer {
                question: "q1".into(),
                answer: "use gpui".into(),
            },
            AskAnswer {
                question: "q2".into(),
                answer: String::new(),
            },
        ])]);
        let result = ask
            .execute(
                AskArgs {
                    questions: vec![question("q1"), question("q2")],
                },
                &ctx(),
                &mut stream(),
            )
            .await
            .expect("result");
        assert!(!result.is_error);
        let text = match &result.content[0] {
            mycode_core::message::ContentBlock::Text(text) => text.text.clone(),
            other => panic!("text block required: {other:?}"),
        };
        assert_eq!(
            text,
            "1. use gpui
2. (skipped)"
        );
    }

    #[tokio::test]
    async fn bounds_and_validation_fail_closed() {
        let ask = tool(vec![]);
        let empty = AskArgs { questions: vec![] };
        assert!(ask.execute(empty, &ctx(), &mut stream()).await.is_err());

        let too_many = AskArgs {
            questions: (0..MAX_QUESTIONS + 1)
                .map(|i| question(&i.to_string()))
                .collect(),
        };
        assert!(ask.execute(too_many, &ctx(), &mut stream()).await.is_err());

        let blank = AskArgs {
            questions: vec![question("  ")],
        };
        assert!(ask.execute(blank, &ctx(), &mut stream()).await.is_err());
    }

    #[tokio::test]
    async fn dismissal_maps_to_execution_error() {
        let ask = tool(vec![Err(user_dismissed())]);
        let result = ask
            .execute(
                AskArgs {
                    questions: vec![question("continue?")],
                },
                &ctx(),
                &mut stream(),
            )
            .await;
        assert!(matches!(result, Err(ToolError::Execution(_))));
    }
}
