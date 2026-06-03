#![allow(dead_code, unused_imports)]

mod error;
mod human_size;
mod pacman;

fn main() {
    use human_size::{format_size, UnitSpec};
    let _ = format_size(1_099_511_627_776, UnitSpec::Ti);

    println!("Hello, world!");
}

use error::Result;
