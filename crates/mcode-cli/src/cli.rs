//! Defines the fail-closed `mcode` command-line skeleton.
//!
//! ```text
//! mcode [--cwd <path>] run "<prompt>"
//! mcode [--cwd <path>] resume <session> "<prompt>"
//! ```
//!
//! The product commands are reserved while their Manager-bound services are
//! unavailable. Parsing does not imply that a run or persisted session starts.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Parsed command line.
#[derive(Debug, Parser)]
#[command(
    name = "mcode",
    version,
    about = "MCode command skeleton (run and resume require signed Provider and Session Packs)"
)]
pub struct Cli {
    /// Working directory requested for the invocation.
    ///
    /// The fail-closed command skeleton accepts this value without accessing
    /// or validating the path.
    #[arg(long, global = true, value_name = "PATH")]
    pub cwd: Option<PathBuf>,

    /// Requested product command.
    #[command(subcommand)]
    pub command: Command,
}

/// Product command skeletons reserved for Manager-bound services.
#[derive(Debug, PartialEq, Subcommand)]
pub enum Command {
    /// Request a new run.
    ///
    /// This command currently fails closed before accessing the working
    /// directory or starting any product service.
    Run {
        /// User prompt reserved for the future run service.
        prompt: String,
    },
    /// Request continuation of a persisted session.
    ///
    /// This command currently fails closed before resolving the selector or
    /// accessing persisted state.
    Resume {
        /// Opaque session selector reserved for the future Session Pack.
        session: String,
        /// User prompt reserved for the future resume service.
        prompt: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::{CommandFactory, Parser, error::ErrorKind};
    use std::path::PathBuf;

    #[test]
    fn cli_declares_expected_subcommands() {
        let command = Cli::command();
        let names: Vec<&str> = command.get_subcommands().map(|s| s.get_name()).collect();
        assert_eq!(names, vec!["run", "resume"]);
    }

    #[test]
    fn parses_run_with_cwd_without_accessing_it() {
        let cli =
            Cli::try_parse_from(["mcode", "--cwd", "path-that-need-not-exist", "run", "hello"])
                .unwrap();
        assert_eq!(cli.cwd, Some(PathBuf::from("path-that-need-not-exist")));
        assert_eq!(
            cli.command,
            Command::Run {
                prompt: "hello".into()
            }
        );
    }

    #[test]
    fn parses_resume_with_session_selector() {
        let cli = Cli::try_parse_from(["mcode", "resume", "latest", "continue"]).unwrap();
        assert!(cli.cwd.is_none());
        assert_eq!(
            cli.command,
            Command::Resume {
                session: "latest".into(),
                prompt: "continue".into()
            }
        );
    }

    #[test]
    fn cwd_remains_global_after_the_subcommand() {
        let cli = Cli::try_parse_from(["mcode", "run", "hi", "--cwd", "missing"]).unwrap();
        assert_eq!(cli.cwd, Some(PathBuf::from("missing")));
    }

    #[test]
    fn legacy_product_flags_are_unknown_arguments() {
        for flag in ["--provider", "--profile", "--model", "--fake", "--yolo"] {
            let error = Cli::try_parse_from(["mcode", flag, "dummy", "run", "hi"])
                .expect_err("legacy product flags must stay outside the CLI surface");
            assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{flag}");
        }
    }

    #[test]
    fn run_without_prompt_is_a_usage_error() {
        let error = Cli::try_parse_from(["mcode", "run"]).expect_err("prompt is required");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn resume_without_prompt_is_a_usage_error() {
        let error = Cli::try_parse_from(["mcode", "resume", "latest"])
            .expect_err("resume prompt is required");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }
}
