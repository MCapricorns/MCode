//! `mcode` binary composition root.
//!
//! The fail-closed product command skeleton lives in [`mcode_cli`]; this thin
//! forwarder only provides the executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    mcode_cli::main()
}
