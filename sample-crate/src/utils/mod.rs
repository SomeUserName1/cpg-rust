pub mod deep;

pub fn helper(a: i32, b: i32) -> i32 {
    let s = a + b;
    if s > 0 {
        s
    } else {
        -s
    }
}

pub struct Config {
    pub verbose: bool,
}

impl Config {
    pub fn verbose() -> bool { true }
}
