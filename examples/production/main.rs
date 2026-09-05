fn answer() -> u32 {
    42
}

fn main() {
    println!("{}", answer());
}

#[test]
fn computes_answer() {
    assert_eq!(answer(), 42);
}
