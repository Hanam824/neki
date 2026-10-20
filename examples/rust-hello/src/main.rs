fn main() {
    let mut int = itoa::Buffer::new();
    let mut float = ryu::Buffer::new();
    println!("hello from Rust, built by Neki");
    println!(
        "itoa from crates.io formats {}, ryu from git formats {}",
        int.format(42),
        float.format(1.5)
    );
}
