fn main() {
    let mut buffer = itoa::Buffer::new();
    println!("hello from Rust, built by Neki ({} crates vendored)", buffer.format(1));
}
