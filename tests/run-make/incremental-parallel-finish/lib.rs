pub fn greeting(name: &str) -> String {
    format!("hello, {name}")
}

pub fn generic<T: Clone>(x: &T) -> (T, T) {
    (x.clone(), x.clone())
}
