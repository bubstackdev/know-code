pub struct Greeter;
mod helper;

#[cfg(know_code_generated)]
pub fn only_after_build_script() {}

impl Greeter {
    pub fn greet(&self, input: &str) -> String {
        let café = format!("Hello {input}"); // 🦀 UTF-8 source coordinates
        café
    }
}

pub fn use_greeter() -> String {
    let greeter = Greeter;
    helper::decorate(greeter.greet("Know Code"))
}

pub fn shadowed() -> usize {
    let café = 42;
    café
}

#[cfg(test)]
mod tests {
    use super::Greeter;

    #[test]
    fn greeting() {
        let expected = "Hello Know Code";
        assert_eq!(Greeter.greet("Know Code"), expected);
    }
}
