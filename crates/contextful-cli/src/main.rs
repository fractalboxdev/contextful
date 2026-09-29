//! The `contextful` binary: the command line with no host derive task registered.

fn main() {
    contextful_cli::main_with(contextful_core::run::derive::task::Tasks::default());
}
