//! Native CLI frontend. Subcommands (`run`, `diff`, `bench`, `replay-*`)
//! land with their phases; this is the Phase 0 skeleton.

fn main() {
    println!(
        "nestris-cli {} (bootstrap skeleton)",
        env!("CARGO_PKG_VERSION")
    );
}
