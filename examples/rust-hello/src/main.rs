use rayon::prelude::*;

fn main() {
    let mut int = itoa::Buffer::new();
    let sum: u64 = (1..=100u64).into_par_iter().sum();
    println!("hello from Rust, built by Neki");
    println!(
        "itoa from crates.io formats {}, rayon from git sums 1..=100 to {sum}",
        int.format(42)
    );
}
