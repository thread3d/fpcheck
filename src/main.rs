//! `fpcheck` entry point. All logic lives in the library so it can be tested.

fn main() {
    std::process::exit(fpcheck::cli::main_entry());
}
