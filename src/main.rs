//! Binary entry point for the mra crate.

use mra::ids::AgentId;

fn main() {
    let id = AgentId::new();
    println!("mra starting, test id: {id}");
}
