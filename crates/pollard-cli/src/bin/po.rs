//! `po`: alias for `pollard` (spec §1). Same binary, shared implementation.

#[path = "pollard.rs"]
mod pollard;

fn main() -> std::process::ExitCode {
    pollard::main()
}
