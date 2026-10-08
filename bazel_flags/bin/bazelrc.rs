//! `bviz bazelrc` as a binary of its own, which builds without the viewers.

use bviz_bazel_flags_args::BazelrcArgs;
use clap::Parser;

#[derive(Parser)]
#[command(name = "bazelrc", version)]
struct Cli {
    #[command(flatten)]
    bazelrc: BazelrcArgs,
}

fn main() {
    Cli::parse().bazelrc.print();
}
