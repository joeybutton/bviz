use bazel_viz_greeting::greeting;
use clap::Parser;

/// Placeholder entry point for the bazel_viz visualization tool.
#[derive(Parser)]
#[command(name = "bazel_viz", version, about)]
struct Cli {
    /// Who to greet.
    #[arg(long, default_value = "world")]
    name: String,
}

fn main() {
    let cli = Cli::parse();
    println!("{}", greeting(&cli.name));
}
