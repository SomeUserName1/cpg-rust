mod inner {
    pub fn inline_fn(x: u64) -> u64 {
        x + 1
    }
}

mod utils;

use std::collections::HashMap;

trait Animal {
    fn speak(&self) -> String;
    fn legs(&self) -> u8 { 4 }
}

struct Dog {
    name: String,
}

struct Cat;

impl Animal for Dog {
    fn speak(&self) -> String {
        format!("{} says woof", self.name)
    }
}

impl Animal for Cat {
    fn speak(&self) -> String {
        "meow".to_string()
    }
}

impl Dog {
    fn new(name: &str) -> Self {
        Dog { name: name.to_string() }
    }
}

macro_rules! twice {
    ($x:expr) => {
        $x * 2
    };
}

fn generics<T: Clone>(items: Vec<T>, map: &mut HashMap<String, u32>) -> Option<T> {
    let count = map.len() as u32;
    if count > 10 {
        return items.into_iter().next();
    }
    let _ = map.insert("k".to_string(), twice!(count));
    match count {
        0 => None,
        n if n % 2 == 0 => items.first().cloned(),
        _ => None,
    }
}

async fn fetch(url: &str) -> String {
    let resp = do_fetch(url).await;
    resp
}

fn do_fetch(url: &str) -> impl Future<Output = String> {
    async { url.to_string() }
}

fn main() {
    let d = Dog::new("rex");
    let speech = d.speak();
    println!("{}", speech);
    let vals = vec![1, 2, 3];
    let doubled: Vec<i32> = vals.iter().map(|v| v * 2).collect();
    let x = twice!(5);
    if x > 4 {
        println!("big");
    } else {
        println!("small");
    }
}
