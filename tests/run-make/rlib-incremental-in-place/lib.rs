pub fn add(a: u32, b: u32) -> u32 {
    a.wrapping_add(b)
}

pub fn generic<T: Clone>(x: &T) -> (T, T) {
    (x.clone(), x.clone())
}

pub struct Foo {
    pub v: Vec<String>,
}

impl Foo {
    pub fn len(&self) -> usize {
        self.v.iter().map(|s| s.len()).sum()
    }
}
