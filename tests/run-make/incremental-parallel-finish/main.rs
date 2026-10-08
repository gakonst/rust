fn main() {
    let (a, b) = lib::generic(&lib::greeting("world"));
    println!("{a} {b}");
}
